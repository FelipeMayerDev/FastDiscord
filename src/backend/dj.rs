//! Shared DJ protocol and client, peer to peer over iroh (docs/DJ.md).
//! No Discord credentials leave the app; no DJ server is configured.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use iroh::{Endpoint, EndpointId, SecretKey, endpoint::presets};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

use super::dj_server::{Room, Rooms};
use super::music::Music;

pub const MAX_MESSAGE: usize = 1024 * 1024;
pub const VERSION: u8 = 3;
const ALPN: &[u8] = b"fastdiscord/dj/3";

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub duration_ms: u64,
}
impl Track {
    pub fn validate(&self) -> Result<(), String> {
        if self.video_id.len() != 11
            || !self
                .video_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err("Vídeo do YouTube inválido".into());
        }
        if self.title.is_empty()
            || self.title.len() > 1024
            || self.title.chars().any(char::is_control)
            || self.duration_ms == 0
            || self.duration_ms > 86_400_000
        {
            return Err(
                "Título ou duração inválidos; use músicas de até 24 horas, sem live".into(),
            );
        }
        Ok(())
    }
    pub fn url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.video_id)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    pub revision: u64,
    pub epoch: u64,
    pub tracks: Vec<Track>,
    pub index: Option<usize>,
    pub paused: bool,
    pub preparing: bool,
    pub ready_count: usize,
    pub position_ms: u64,
    pub start_at_ms: u64,
    pub participants: usize,
    /// Discord ids of the connected DJ clients; picks the next leader.
    pub peers: Vec<String>,
}
impl Session {
    pub fn current(&self) -> Option<&Track> {
        self.index.and_then(|i| self.tracks.get(i))
    }
    pub fn position(&self, server_now: u64) -> u64 {
        let elapsed = if self.paused || self.preparing {
            0
        } else {
            server_now.saturating_sub(self.start_at_ms)
        };
        (self.position_ms.saturating_add(elapsed)).min(self.current().map_or(0, |t| t.duration_ms))
    }
    fn start(&mut self, index: Option<usize>, now: u64) {
        self.index = index;
        self.position_ms = 0;
        self.paused = false;
        self.preparing = index.is_some();
        self.ready_count = 0;
        self.start_at_ms = now.saturating_add(300_000);
        self.epoch += 1;
    }
    pub fn apply(&mut self, command: Action, now: u64) -> Result<(), String> {
        match command {
            Action::Add(track) => {
                track.validate()?;
                // Bound the wire snapshot, rather than silently truncating the playlist.
                if serde_json::to_vec(&self.tracks)
                    .map_err(|e| e.to_string())?
                    .len()
                    + 2048
                    > MAX_MESSAGE / 2
                {
                    return Err("Playlist grande demais para esta sessão".into());
                }
                let index = self.tracks.len();
                self.tracks.push(track);
                if self.index.is_none() {
                    self.start(Some(index), now);
                }
            }
            Action::Previous => {
                if let Some(index) = self.index {
                    let previous = if self.position(now) > 3000 {
                        index
                    } else {
                        index.saturating_sub(1)
                    };
                    self.start(Some(previous), now);
                }
            }
            Action::Next => {
                if let Some(index) = self.index {
                    self.start((index + 1 < self.tracks.len()).then_some(index + 1), now);
                }
            }
            Action::Play => {
                if self.index.is_none() && !self.tracks.is_empty() {
                    self.start(Some(0), now);
                } else if self.paused {
                    self.paused = false;
                    if !self.preparing {
                        self.start_at_ms = now.saturating_add(500);
                    }
                }
            }
            Action::Pause => {
                if self.index.is_some() && !self.paused {
                    self.position_ms = self.position(now);
                    self.paused = true;
                }
            }
            Action::Stop => self.start(None, now),
        }
        self.revision += 1;
        Ok(())
    }
    pub fn tick(&mut self, now: u64) -> bool {
        if !self.paused
            && !self.preparing
            && self
                .current()
                .is_some_and(|t| self.position(now) >= t.duration_ms)
        {
            let _ = self.apply(Action::Next, now);
            true
        } else {
            false
        }
    }
    /// Moves the timeline from the old leader's clock onto this one's.
    fn rebase(&mut self, old_now: u64, now: u64) {
        self.position_ms = self.position(old_now);
        self.start_at_ms = if self.preparing { now + 300_000 } else { now };
        self.participants = 0;
        self.peers.clear();
        self.ready_count = 0;
        self.revision += 1;
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.peers.len() > 1000 || self.peers.iter().any(|id| id.parse::<u64>().is_err()) {
            return Err("Participantes inválidos no DJ".into());
        }
        if self.index.is_some_and(|i| i >= self.tracks.len()) {
            return Err("Posição inválida na playlist".into());
        }
        for track in &self.tracks {
            track.validate()?;
        }
        if self.position_ms > self.current().map_or(0, |t| t.duration_ms) {
            return Err("Tempo inválido do DJ".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Action {
    Add(Track),
    Previous,
    Play,
    Pause,
    Next,
    Stop,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Join { version: u8, room: String },
    Action { action: Action },
    Ready { epoch: u64, success: bool },
    Ping { nonce: u64 },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    State { session: Session, server_ms: u64 },
    Pong { nonce: u64, server_ms: u64 },
    Error { message: String },
}

#[derive(Clone, Debug)]
pub struct Clock {
    at: Instant,
    server_ms: u64,
}
impl Clock {
    pub fn new(server_ms: u64) -> Self {
        Self {
            at: Instant::now(),
            server_ms,
        }
    }
    pub fn now(&self) -> u64 {
        self.server_ms
            .saturating_add(self.at.elapsed().as_millis() as u64)
    }
}
#[derive(Default)]
pub struct View {
    pub session: Option<Session>,
    pub clock: Option<Clock>,
    pub connected: bool,
    pub error: Option<String>,
    pub resolving: bool,
    pub search_results: Vec<Track>,
}

#[derive(Default)]
pub struct DjClient {
    pub view: Arc<Mutex<View>>,
    pub music: Music,
    connection: Option<(String, String)>,
    task: Option<tokio::task::JoinHandle<()>>,
    commands: Option<mpsc::Sender<Action>>,
    members: Option<watch::Sender<Vec<String>>>,
}
impl Drop for DjClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}
impl DjClient {
    pub fn disconnect(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.commands = None;
        self.members = None;
        self.connection = None;
        self.music.shutdown();
        self.music = Music::default();
        // Old resolver/task results retain the old Arc and cannot affect another room.
        self.view = Arc::default();
    }
    pub fn configure(
        &mut self,
        runtime: &tokio::runtime::Handle,
        ctx: &egui::Context,
        room: Option<(String, String)>,
        members: Vec<String>,
    ) {
        if room == self.connection {
            if let Some(tx) = &self.members {
                tx.send_if_modified(|old| (*old != members).then(|| *old = members).is_some());
            }
            return;
        }
        self.disconnect();
        self.connection = room.clone();
        let Some((room, me)) = room else {
            return;
        };
        let (tx, rx) = mpsc::channel(32);
        self.commands = Some(tx);
        let (members_tx, members) = watch::channel(members);
        self.members = Some(members_tx);
        let view = self.view.clone();
        let music = self.music.clone();
        let ctx = ctx.clone();
        self.task = Some(runtime.spawn(async move {
            connect(room, me, members, rx, view, music, ctx).await;
        }));
    }
    pub fn action(&self, action: Action) {
        let result = self
            .commands
            .as_ref()
            .ok_or("Entre em uma sala de voz para usar o DJ")
            .and_then(|tx| {
                tx.try_send(action)
                    .map_err(|_| "DJ desconectado ou ocupado; tente novamente")
            });
        if let Err(error) = result {
            self.view.lock().unwrap().error = Some(error.into());
        }
    }
    pub fn enqueue(
        &self,
        request: &str,
        runtime: &tokio::runtime::Handle,
        ctx: &egui::Context,
    ) -> bool {
        let request = request.trim().to_string();
        if request.is_empty() {
            return false;
        }
        let Some(tx) = self.commands.clone() else {
            self.action(Action::Play);
            return false;
        };
        let is_link = request.contains("://");
        let view = self.view.clone();
        {
            let mut view = view.lock().unwrap();
            if !view.connected || view.resolving {
                return false;
            }
            view.resolving = true;
            view.error = None;
            if !is_link {
                view.search_results.clear();
            }
        }
        let ctx = ctx.clone();
        runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                if is_link {
                    super::music::resolve(&request).map(|track| vec![track])
                } else {
                    super::music::search(&request)
                }
            })
            .await;
            let result = match result {
                Ok(Ok(tracks)) if is_link => tx
                    .send(Action::Add(tracks.into_iter().next().unwrap()))
                    .await
                    .map(|_| Vec::new())
                    .map_err(|_| "DJ desconectado".to_string()),
                Ok(result) => result,
                Err(_) => Err("Não foi possível buscar a música".into()),
            };
            let mut view = view.lock().unwrap();
            view.resolving = false;
            match result {
                Ok(tracks) if !is_link => view.search_results = tracks,
                Err(error) => view.error = Some(error),
                _ => {}
            }
            ctx.request_repaint();
        });
        true
    }
}

pub fn websocket_config() -> WebSocketConfig {
    WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE),
        max_frame_size: Some(MAX_MESSAGE),
        ..Default::default()
    }
}

/// The derived identity is the whole trust model: anyone who knows a voice
/// member's Discord id can compute it, the same as the old shared relay key.
fn secret(user: &str) -> SecretKey {
    SecretKey::from_bytes(&Sha256::digest(format!("fastdiscord-dj-v3:{user}")).into())
}
fn endpoint_id(user: &str) -> EndpointId {
    secret(user).public()
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Ws = WebSocketStream<Box<dyn Io>>;
/// A joined room: the socket plus the leader's first state.
type Joined = (Ws, ServerMessage);

async fn join(io: Box<dyn Io>, room: &str) -> Result<Joined, String> {
    let (mut ws, _) =
        tokio_tungstenite::client_async_with_config("ws://dj/", io, Some(websocket_config()))
            .await
            .map_err(|_| "Não foi possível conectar ao DJ")?;
    let join = ClientMessage::Join {
        version: VERSION,
        room: room.into(),
    };
    ws.send(Message::text(serde_json::to_string(&join).unwrap()))
        .await
        .map_err(|_| "Falha ao entrar no DJ")?;
    let Ok(Some(Ok(Message::Text(text)))) =
        tokio::time::timeout(Duration::from_secs(5), ws.next()).await
    else {
        return Err("O DJ não respondeu".into());
    };
    match serde_json::from_str(&text).map_err(|_| "Resposta inválida do DJ")? {
        ServerMessage::Error { message } => Err(message),
        first => Ok((ws, first)),
    }
}
async fn dial(endpoint: &Endpoint, room: &str, user: &str) -> Option<Joined> {
    let conn = endpoint.connect(endpoint_id(user), ALPN).await.ok()?;
    let (send, recv) = conn.open_bi().await.ok()?;
    join(Box::new(tokio::io::join(recv, send)), room).await.ok()
}
/// Who wins when two clients lead at once: music first, then the lowest id.
fn rank(session: &Session, user: &str) -> (bool, u64) {
    (session.tracks.is_empty(), user.parse().unwrap_or(u64::MAX))
}
/// The best leader among the other voice members, if any is leading.
async fn elect(
    endpoint: &Endpoint,
    room: &str,
    others: Vec<String>,
) -> Option<((bool, u64), Joined)> {
    let dials = others.into_iter().map(|user| async move {
        let joined = tokio::time::timeout(Duration::from_secs(5), dial(endpoint, room, &user))
            .await
            .ok()??;
        let ServerMessage::State { session, .. } = &joined.1 else {
            return None;
        };
        Some((rank(session, &user), joined))
    });
    futures_util::future::join_all(dials)
        .await
        .into_iter()
        .flatten()
        .min_by_key(|(rank, _)| *rank)
}
fn others(members: &watch::Receiver<Vec<String>>, me: &str) -> Vec<String> {
    members
        .borrow()
        .iter()
        .filter(|u| *u != me)
        .cloned()
        .collect()
}

/// Serves the room to voice members while this client leads.
async fn accept(endpoint: Endpoint, rooms: Rooms, members: watch::Receiver<Vec<String>>) {
    let mut peers = JoinSet::new();
    while let Some(incoming) = endpoint.accept().await {
        while peers.try_join_next().is_some() {}
        let rooms = rooms.clone();
        let members = members.clone();
        peers.spawn(async move {
            let Ok(conn) = incoming.await else { return };
            let user = members
                .borrow()
                .iter()
                .find(|user| endpoint_id(user) == conn.remote_id())
                .cloned();
            let Some(user) = user.filter(|_| !rooms.lock().unwrap().is_empty()) else {
                conn.close(1u32.into(), b"not leading");
                return;
            };
            if let Ok((send, recv)) = conn.accept_bi().await {
                let _ = super::dj_server::peer(tokio::io::join(recv, send), rooms, user).await;
            }
        });
    }
}
/// Clears the room when leadership ends, even if the task is aborted.
struct Leading(Rooms);
impl Drop for Leading {
    fn drop(&mut self) {
        self.0.lock().unwrap().clear();
    }
}
#[allow(clippy::too_many_arguments)]
async fn lead(
    endpoint: &Endpoint,
    room: &str,
    me: &str,
    rooms: &Rooms,
    seed: Session,
    members: &watch::Receiver<Vec<String>>,
    rx: &mut mpsc::Receiver<Action>,
    view: &Arc<Mutex<View>>,
    music: &Music,
    ctx: &egui::Context,
) -> Result<(), String> {
    rooms.lock().unwrap().insert(room.into(), Room::new(seed));
    let _leading = Leading(rooms.clone());
    let (client, server) = tokio::io::duplex(2 * MAX_MESSAGE);
    let mut local = JoinSet::new();
    local.spawn(super::dj_server::peer(server, rooms.clone(), me.into()));
    let (ws, first) = join(Box::new(client), room).await?;
    // Two clients can start leading together; the worse-ranked one steps down.
    let rival = async {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let mine = rooms
                .lock()
                .unwrap()
                .get(room)
                .map(|r| rank(r.session(), me));
            if let Some((theirs, _)) = elect(endpoint, room, others(members, me)).await
                && mine.is_none_or(|mine| theirs < mine)
            {
                return;
            }
        }
    };
    tokio::select! {
        result = connection(ws, first, rx, view, music, ctx) => result,
        () = rival => Err("Outro DJ já estava tocando; reconectando…".into()),
    }
}

async fn connect(
    room: String,
    me: String,
    members: watch::Receiver<Vec<String>>,
    mut rx: mpsc::Receiver<Action>,
    view: Arc<Mutex<View>>,
    music: Music,
    ctx: egui::Context,
) {
    let endpoint = match Endpoint::builder(presets::N0)
        .secret_key(secret(&me))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
    {
        Ok(endpoint) => endpoint,
        Err(error) => {
            view.lock().unwrap().error = Some(format!("Não foi possível iniciar o DJ: {error}"));
            ctx.request_repaint();
            return;
        }
    };
    let rooms = Rooms::default();
    let mut tasks = JoinSet::new();
    tasks.spawn(accept(endpoint.clone(), rooms.clone(), members.clone()));
    // The last session seen, on this clock, so a successor keeps the music.
    let mut seed: Option<Session> = None;
    let mut misses = 0;
    loop {
        let result = match elect(&endpoint, &room, others(&members, &me)).await {
            Some((_, (ws, first))) => {
                misses = 0;
                connection(ws, first, &mut rx, &view, &music, &ctx).await
            }
            None => {
                // The lowest remaining DJ client takes over; the rest wait
                // for it, unless it never shows up.
                let peers = seed.as_ref().map_or(&[][..], |s| &s.peers[..]);
                let successor = members
                    .borrow()
                    .iter()
                    .filter(|u| peers.contains(u))
                    .chain([&me])
                    .min_by_key(|u| u.parse::<u64>().unwrap_or(u64::MAX))
                    .is_some_and(|u| *u == me);
                misses += 1;
                if successor || misses > 2 {
                    misses = 0;
                    let seed = seed.take().unwrap_or_default();
                    lead(
                        &endpoint, &room, &me, &rooms, seed, &members, &mut rx, &view, &music, &ctx,
                    )
                    .await
                } else {
                    Err("Procurando quem está tocando o DJ…".into())
                }
            }
        };
        music.stop();
        while rx.try_recv().is_ok() {} // Never replay stale controls after a disconnect.
        {
            let mut view = view.lock().unwrap();
            if let (Some(mut session), Some(clock)) = (view.session.take(), view.clock.take()) {
                session.rebase(clock.now(), now_ms());
                seed = Some(session);
            }
            view.connected = false;
            view.error = Some(
                result
                    .err()
                    .unwrap_or_else(|| "DJ desconectado; reconectando…".into()),
            );
        }
        ctx.request_repaint();
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
async fn connection(
    ws: Ws,
    first: ServerMessage,
    rx: &mut mpsc::Receiver<Action>,
    view: &Arc<Mutex<View>>,
    music: &Music,
    ctx: &egui::Context,
) -> Result<(), String> {
    let (mut ws, incoming) = ws.split();
    let mut incoming =
        futures_util::stream::iter([Ok(Message::text(serde_json::to_string(&first).unwrap()))])
            .chain(incoming);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
    let mut pending: Option<(u64, Instant)> = None;
    let mut nonce = 0;
    let mut reported_ready = None;
    let mut best_rtt = Duration::MAX;
    let mut best_at = Instant::now();
    let mut last_message = Instant::now();
    loop {
        tokio::select! {
            action = rx.recv() => {
                let Some(action) = action else { return Ok(()); };
                if !view.lock().unwrap().connected { continue; }
                ws.send(Message::text(serde_json::to_string(&ClientMessage::Action { action }).unwrap())).await.map_err(|_| "Falha ao enviar controle DJ")?;
            }
            _ = heartbeat.tick() => {
                if last_message.elapsed() > Duration::from_secs(10) { return Err("O DJ parou de responder".into()); }
                if let Some(ready) = music.preparation() && reported_ready != Some(ready) {
                    ws.send(Message::text(serde_json::to_string(&ClientMessage::Ready { epoch: ready.0, success: ready.1 }).unwrap())).await.map_err(|_| "Falha ao informar download do DJ")?;
                    reported_ready = Some(ready);
                }
                nonce += 1;
                pending = Some((nonce, Instant::now()));
                ws.send(Message::text(serde_json::to_string(&ClientMessage::Ping { nonce }).unwrap())).await.map_err(|_| "Falha no relógio DJ")?;
            }
            message = incoming.next() => {
                let Some(Ok(message)) = message else { return Err("Conexão DJ encerrada".into()); };
                last_message = Instant::now();
                let Message::Text(text) = message else { if message.is_close() { return Ok(()); } continue; };
                let message: ServerMessage = serde_json::from_str(&text).map_err(|_| "Resposta inválida do DJ")?;
                match message {
                    ServerMessage::State { session, server_ms } => {
                        session.validate()?;
                        let mut view = view.lock().unwrap();
                        if !view.connected { view.error = None; }
                        view.connected = true;
                        let clock = view.clock.get_or_insert_with(|| Clock::new(server_ms)).clone();
                        music.apply(&session, &clock);
                        view.session = Some(session);
                    }
                    ServerMessage::Pong { nonce, server_ms } => {
                        if let Some((expected, started)) = pending.take() && expected == nonce {
                            let rtt = started.elapsed();
                            if rtt <= best_rtt || best_at.elapsed() >= Duration::from_secs(30) {
                                best_at = Instant::now();
                                best_rtt = rtt;
                                let clock = Clock::new(server_ms.saturating_add(rtt.as_millis() as u64 / 2));
                                let mut view = view.lock().unwrap();
                                if let Some(session) = &view.session { music.apply(session, &clock); }
                                view.clock = Some(clock);
                            }
                        }
                    }
                    ServerMessage::Error { message } => {
                        if !view.lock().unwrap().connected { return Err(message); }
                        view.lock().unwrap().error = Some(message);
                    }
                }
                ctx.request_repaint();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track() -> Track {
        Track {
            video_id: "abcdefghijk".into(),
            title: "Music".into(),
            duration_ms: 10_000,
        }
    }
    #[test]
    fn timeline_pause_resume_previous_stop_and_auto_next() {
        let mut state = Session::default();
        state.apply(Action::Add(track()), 1000).unwrap();
        state.apply(Action::Add(track()), 1000).unwrap();
        assert!(state.preparing);
        let deadline = state.start_at_ms;
        state.apply(Action::Pause, 2000).unwrap();
        state.apply(Action::Play, 2500).unwrap();
        assert_eq!(state.start_at_ms, deadline);
        assert_eq!(state.position(6000), 0);
        assert!(!state.tick(600_000));
        state.preparing = false;
        state.start_at_ms = 4000;
        assert_eq!(state.position(3000), 0);
        assert_eq!(state.position(6000), 2000);
        state.apply(Action::Pause, 6000).unwrap();
        assert_eq!(state.position(9000), 2000);
        state.apply(Action::Play, 9000).unwrap();
        assert_eq!(state.position(10_000), 2500);
        assert!(state.tick(18_000));
        assert_eq!(state.index, Some(1));
        state.apply(Action::Previous, 18_001).unwrap();
        assert_eq!(state.index, Some(0));
        let epoch = state.epoch;
        state.apply(Action::Stop, 18_002).unwrap();
        assert!(state.current().is_none());
        assert!(state.epoch > epoch);
        assert_eq!(state.tracks.len(), 2);
    }
    #[test]
    fn handover_keeps_position_and_identity_is_stable() {
        let mut state = Session::default();
        state.apply(Action::Add(track()), 1000).unwrap();
        state.preparing = false;
        state.start_at_ms = 1000;
        state.peers = vec!["1".into()];
        // The old leader's clock reads 5000; ours reads 90_000.
        state.rebase(5000, 90_000);
        assert_eq!(state.position(91_000), 5000);
        assert!(state.peers.is_empty());
        assert_eq!(endpoint_id("123"), endpoint_id("123"));
        assert_ne!(endpoint_id("123"), endpoint_id("124"));
        assert!(rank(&state, "9") < rank(&Session::default(), "1"));
    }
    #[test]
    fn rejects_remote_media() {
        let mut media = track();
        media.video_id = "http://localhost".into();
        assert!(media.validate().is_err());
        let mut state = Session::default();
        state.index = Some(1);
        assert!(state.validate().is_err());
    }
}
#[cfg(test)]
mod network {
    use super::*;
    /// Two clients find each other through n0's discovery and share one room.
    /// `cargo test --lib dj::network -- --ignored` (needs internet).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn two_clients_elect_one_leader_over_iroh() {
        let room = format!("1:{}", now_ms());
        let users = [now_ms().to_string(), (now_ms() + 1).to_string()];
        let (_members, rx) = watch::channel(users.to_vec());
        let (views, _controls): (Vec<Arc<Mutex<View>>>, Vec<_>) = users
            .iter()
            .map(|me| {
                let view = Arc::<Mutex<View>>::default();
                let (tx, actions) = mpsc::channel(1);
                tokio::spawn(connect(
                    room.clone(),
                    me.clone(),
                    rx.clone(),
                    actions,
                    view.clone(),
                    Music::default(),
                    egui::Context::default(),
                ));
                (view, tx)
            })
            .unzip();
        tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let peers: Vec<_> = views
                    .iter()
                    .map(|v| v.lock().unwrap().session.as_ref().map(|s| s.peers.len()))
                    .collect();
                if peers == [Some(2), Some(2)] {
                    break;
                }
            }
        })
        .await
        .expect("both clients should join the same leader");
    }
}
