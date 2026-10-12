//! Shared DJ protocol and client. The relay receives no Discord credentials.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

use super::music::Music;

pub const MAX_MESSAGE: usize = 1024 * 1024;
pub const VERSION: u8 = 2;

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
    pub fn validate(&self) -> Result<(), String> {
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
    Join {
        version: u8,
        room: String,
        key: String,
    },
    Action {
        action: Action,
    },
    Ready {
        epoch: u64,
        success: bool,
    },
    Ping {
        nonce: u64,
    },
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
    connection: Option<(String, String, String)>,
    task: Option<tokio::task::JoinHandle<()>>,
    commands: Option<mpsc::Sender<Action>>,
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
        room: Option<String>,
        url: &str,
        key: &str,
    ) {
        let wanted = room.map(|room| (room, url.trim().to_string(), key.to_string()));
        if wanted == self.connection {
            return;
        }
        self.disconnect();
        self.connection = wanted.clone();
        let Some((room, url, key)) = wanted else {
            return;
        };
        if let Err(error) = validate_endpoint(&url, &key) {
            self.view.lock().unwrap().error = Some(error);
            return;
        }
        let (tx, rx) = mpsc::channel(32);
        self.commands = Some(tx);
        let view = self.view.clone();
        let music = self.music.clone();
        let ctx = ctx.clone();
        self.task = Some(runtime.spawn(async move {
            connect(room, url, key, rx, view, music, ctx).await;
        }));
    }
    pub fn action(&self, action: Action) {
        let result = self
            .commands
            .as_ref()
            .ok_or("Configure o servidor DJ em Voz e áudio")
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

pub fn validate_endpoint(url: &str, key: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("Configure o servidor DJ em Configurações → Voz e áudio".into());
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "Endereço do servidor DJ inválido")?;
    let local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(parsed.scheme() == "wss" || parsed.scheme() == "ws" && local)
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("Use wss:// para o DJ; ws:// é permitido apenas em localhost".into());
    }
    if key.len() < 16 || key.len() > 256 {
        return Err("A chave do servidor DJ deve ter entre 16 e 256 caracteres".into());
    }
    Ok(())
}

pub fn websocket_config() -> WebSocketConfig {
    WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE),
        max_frame_size: Some(MAX_MESSAGE),
        ..Default::default()
    }
}

async fn connect(
    room: String,
    url: String,
    key: String,
    mut rx: mpsc::Receiver<Action>,
    view: Arc<Mutex<View>>,
    music: Music,
    ctx: egui::Context,
) {
    loop {
        let result = connection(&room, &url, &key, &mut rx, &view, &music, &ctx).await;
        music.stop();
        while rx.try_recv().is_ok() {} // Never replay stale controls after a disconnect.
        {
            let mut view = view.lock().unwrap();
            view.connected = false;
            view.session = None;
            view.clock = None;
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
    room: &str,
    url: &str,
    key: &str,
    rx: &mut mpsc::Receiver<Action>,
    view: &Arc<Mutex<View>>,
    music: &Music,
    ctx: &egui::Context,
) -> Result<(), String> {
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(url, Some(websocket_config()), true),
    )
    .await
    .map_err(|_| "Servidor DJ não respondeu")?
    .map_err(|_| "Não foi possível conectar ao servidor DJ")?;
    ws.send(Message::text(
        serde_json::to_string(&ClientMessage::Join {
            version: VERSION,
            room: room.into(),
            key: key.into(),
        })
        .unwrap(),
    ))
    .await
    .map_err(|_| "Falha ao entrar no DJ")?;
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
                if last_message.elapsed() > Duration::from_secs(10) { return Err("Servidor DJ parou de responder".into()); }
                if let Some(ready) = music.preparation() && reported_ready != Some(ready) {
                    ws.send(Message::text(serde_json::to_string(&ClientMessage::Ready { epoch: ready.0, success: ready.1 }).unwrap())).await.map_err(|_| "Falha ao informar download do DJ")?;
                    reported_ready = Some(ready);
                }
                nonce += 1;
                pending = Some((nonce, Instant::now()));
                ws.send(Message::text(serde_json::to_string(&ClientMessage::Ping { nonce }).unwrap())).await.map_err(|_| "Falha no relógio DJ")?;
            }
            message = ws.next() => {
                let Some(Ok(message)) = message else { return Err("Conexão DJ encerrada".into()); };
                last_message = Instant::now();
                let Message::Text(text) = message else { if message.is_close() { return Ok(()); } continue; };
                let message: ServerMessage = serde_json::from_str(&text).map_err(|_| "Resposta inválida do servidor DJ")?;
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
    fn rejects_unsafe_endpoints_and_remote_media() {
        let key = "1234567890123456";
        assert!(validate_endpoint("ws://127.0.0.1:8787", key).is_ok());
        assert!(validate_endpoint("wss://dj.example.com", key).is_ok());
        assert!(validate_endpoint("ws://dj.example.com", key).is_err());
        assert!(validate_endpoint("wss://dj.example.com?token=secret", key).is_err());
        assert!(validate_endpoint("wss://dj.example.com", "short").is_err());
        let mut media = track();
        media.video_id = "http://localhost".into();
        assert!(media.validate().is_err());
        let mut state = Session::default();
        state.index = Some(1);
        assert!(state.validate().is_err());
    }
}
