//! The DJ room, hosted in-process by whichever client leads (docs/DJ.md).
//! Peers reach it over an iroh stream; the leader's own client over a duplex.
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

use super::dj::{ClientMessage, ServerMessage, Session, VERSION, now_ms, websocket_config};

pub struct Room {
    prepared_epoch: u64,
    ready: HashSet<u64>,
    failed: HashSet<u64>,
    session: Session,
    updates: broadcast::Sender<ServerMessage>,
}
impl Room {
    pub fn new(session: Session) -> Self {
        Self {
            prepared_epoch: session.epoch,
            ready: HashSet::new(),
            failed: HashSet::new(),
            session,
            updates: broadcast::channel(64).0,
        }
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
}
/// Empty while this client is not leading; clearing it disconnects every peer.
pub type Rooms = Arc<Mutex<HashMap<String, Room>>>;

fn snapshot(room: &Room) -> ServerMessage {
    ServerMessage::State {
        session: room.session.clone(),
        server_ms: now_ms(),
    }
}
fn broadcast(room: &Room) {
    let _ = room.updates.send(snapshot(room));
}

static NEXT_PEER: AtomicU64 = AtomicU64::new(1);
fn readiness(room: &mut Room, now: u64) {
    if room.prepared_epoch != room.session.epoch {
        room.ready.clear();
        room.failed.clear();
        room.prepared_epoch = room.session.epoch;
    }
    room.session.ready_count = room.ready.len();
    let expired = now >= room.session.start_at_ms;
    if room.session.preparing
        && (expired || room.ready.len() + room.failed.len() >= room.session.participants)
    {
        if room.ready.is_empty() {
            let _ = room.session.apply(super::dj::Action::Stop, now);
            let _ = room.updates.send(ServerMessage::Error {
                message: "Nenhum participante conseguiu baixar a música".into(),
            });
        } else {
            room.session.preparing = false;
            room.session.start_at_ms = now + 3000;
            room.session.revision += 1;
            if expired {
                let _ = room.updates.send(ServerMessage::Error {
                    message: "Download excedeu o tempo limite para alguns participantes".into(),
                });
            }
        }
    }
}

// Drop also runs when a socket/task fails, so stale participants cannot keep a room alive.
struct Membership {
    rooms: Rooms,
    room: String,
    peer_id: u64,
    user: String,
}
impl Drop for Membership {
    fn drop(&mut self) {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(room) = rooms.get_mut(&self.room) {
            room.session.participants -= 1;
            if let Some(i) = room.session.peers.iter().position(|u| *u == self.user) {
                room.session.peers.remove(i);
            }
            room.ready.remove(&self.peer_id);
            room.failed.remove(&self.peer_id);
            if room.session.participants == 0 {
                rooms.remove(&self.room);
            } else {
                readiness(room, now_ms());
                broadcast(room);
            }
        }
    }
}
/// Serves one participant; `user` is the Discord id its endpoint proved.
pub async fn peer<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    rooms: Rooms,
    user: String,
) -> Result<()> {
    let mut ws = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::accept_async_with_config(stream, Some(websocket_config())),
    )
    .await??;
    let first = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await?
        .context("missing join")??;
    let join: ClientMessage = serde_json::from_str(first.to_text()?)?;
    let room_id = match join {
        ClientMessage::Join { version, room }
            if version == VERSION && rooms.lock().unwrap().contains_key(&room) =>
        {
            room
        }
        ClientMessage::Join { version, .. } if version != VERSION => {
            ws.send(Message::text(serde_json::to_string(
                &ServerMessage::Error {
                    message: "Versão incompatível do DJ; todos precisam atualizar o app".into(),
                },
            )?))
            .await?;
            ws.close(None).await?;
            return Ok(());
        }
        _ => bail!("not leading this room"),
    };
    let peer_id = NEXT_PEER.fetch_add(1, Ordering::Relaxed);
    let mut updates = {
        let mut rooms = rooms.lock().unwrap();
        let room = rooms.get_mut(&room_id).context("stopped leading")?;
        room.session.participants += 1;
        room.session.peers.push(user.clone());
        if room.session.current().is_some()
            && !room.session.preparing
            && !room.session.paused
            && now_ms() < room.session.start_at_ms
            && room.session.position_ms == 0
        {
            room.session.preparing = true;
            room.session.start_at_ms = now_ms() + 300_000;
        }
        let updates = room.updates.subscribe();
        broadcast(room);
        updates
    };
    let _membership = Membership {
        rooms: rooms.clone(),
        room: room_id.clone(),
        peer_id,
        user,
    };
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    let mut last_message = Instant::now();
    let mut window = Instant::now();
    let mut actions = 0;
    loop {
        tokio::select! {
            update = updates.recv() => {
                let update = update.context("slow DJ client")?;
                tokio::time::timeout(Duration::from_secs(5), ws.send(Message::text(serde_json::to_string(&update)?))).await??;
            }
            _ = ticker.tick() => {
                if last_message.elapsed() > Duration::from_secs(15) { bail!("DJ heartbeat timeout"); }
                let mut rooms = rooms.lock().unwrap();
                if let Some(room) = rooms.get_mut(&room_id) {
                    let preparing = room.session.preparing;
                    let changed = room.session.tick(now_ms());
                    readiness(room, now_ms());
                    if changed || preparing != room.session.preparing { broadcast(room); }
                }
            }
            message = ws.next() => {
                let Some(message) = message else { break; };
                let message = message?;
                if message.is_close() { break; }
                let Message::Text(text) = message else { continue; };
                last_message = Instant::now();
                if window.elapsed() >= Duration::from_secs(1) { window = Instant::now(); actions = 0; }
                actions += 1;
                if actions > 20 { bail!("DJ rate limit"); }
                match serde_json::from_str::<ClientMessage>(&text)? {
                    ClientMessage::Ping { nonce } => ws.send(Message::text(serde_json::to_string(&ServerMessage::Pong { nonce, server_ms: now_ms() })?)).await?,
                    ClientMessage::Action { action } => {
                        let error = {
                            let mut rooms = rooms.lock().unwrap();
                            let room = rooms.get_mut(&room_id).context("missing room")?;
                            match room.session.apply(action, now_ms()) { Ok(()) => { readiness(room, now_ms()); broadcast(room); None }, Err(error) => Some(error) }
                        };
                        if let Some(message) = error { ws.send(Message::text(serde_json::to_string(&ServerMessage::Error { message })?)).await?; }
                    }
                    ClientMessage::Ready { epoch, success } => {
                        let mut rooms = rooms.lock().unwrap();
                        let room = rooms.get_mut(&room_id).context("missing room")?;
                        readiness(room, now_ms());
                        if room.session.epoch == epoch && room.session.current().is_some() {
                            if success { room.failed.remove(&peer_id); room.ready.insert(peer_id); }
                            else { room.ready.remove(&peer_id); room.failed.insert(peer_id); }
                            readiness(room, now_ms());
                            broadcast(room);
                        }
                    }
                    ClientMessage::Join { .. } => bail!("already joined"),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::dj::{Action, Track};
    use tokio::io::DuplexStream;
    use tokio_tungstenite::WebSocketStream;
    type Socket = WebSocketStream<DuplexStream>;
    async fn join(rooms: &Rooms, room: &str, user: &str) -> Socket {
        let (client, server) = tokio::io::duplex(MAX);
        tokio::spawn(peer(server, rooms.clone(), user.into()));
        let (mut ws, _) = tokio_tungstenite::client_async("ws://dj/", client)
            .await
            .unwrap();
        ws.send(Message::text(
            serde_json::to_string(&ClientMessage::Join {
                version: VERSION,
                room: room.into(),
            })
            .unwrap(),
        ))
        .await
        .unwrap();
        ws
    }
    const MAX: usize = 64 * 1024;
    async fn state(ws: &mut Socket, count: usize, revision: u64) -> Session {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let ServerMessage::State { session, .. } =
                serde_json::from_str(msg.to_text().unwrap()).unwrap()
                && session.participants == count
                && session.revision >= revision
            {
                return session;
            }
        }
    }
    async fn action(ws: &mut Socket, action: Action) {
        ws.send(Message::text(
            serde_json::to_string(&ClientMessage::Action { action }).unwrap(),
        ))
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn peers_share_controls_late_join_and_leader_handover() {
        let rooms = Rooms::default();
        rooms
            .lock()
            .unwrap()
            .insert("1:2".into(), Room::new(Session::default()));
        let mut a = join(&rooms, "1:2", "10").await;
        state(&mut a, 1, 0).await;
        action(
            &mut a,
            Action::Add(Track {
                video_id: "abcdefghijk".into(),
                title: "Test".into(),
                duration_ms: 100_000,
            }),
        )
        .await;
        let first = state(&mut a, 1, 1).await;
        let mut b = join(&rooms, "1:2", "20").await;
        let joined = state(&mut b, 2, 1).await;
        assert_eq!(joined.epoch, first.epoch);
        assert_eq!(joined.start_at_ms, first.start_at_ms);
        assert_eq!(joined.peers, ["10", "20"]);
        action(&mut b, Action::Pause).await;
        assert!(state(&mut a, 2, 2).await.paused);
        // Only the room this client leads is served.
        let mut other = join(&rooms, "1:3", "30").await;
        assert!(
            tokio::time::timeout(Duration::from_secs(2), other.next())
                .await
                .unwrap()
                .is_none_or(|m| m.is_err())
        );
        b.close(None).await.unwrap();
        assert_eq!(state(&mut a, 1, 2).await.peers, ["10"]);
        // Stepping down disconnects everyone so they elect the new leader.
        rooms.lock().unwrap().clear();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(_)) = a.next().await {}
        })
        .await
        .unwrap();
    }
    #[test]
    fn playback_waits_for_downloads_and_handles_failure_and_departure() {
        let mut room = Room::new(Session::default());
        room.session.participants = 2;
        room.session
            .apply(
                Action::Add(Track {
                    video_id: "abcdefghijk".into(),
                    title: "Music".into(),
                    duration_ms: 100_000,
                }),
                1000,
            )
            .unwrap();
        readiness(&mut room, 1001);
        room.ready.insert(1);
        readiness(&mut room, 2000);
        assert!(room.session.preparing);
        assert_eq!(room.session.position(9000), 0);
        room.failed.insert(2);
        readiness(&mut room, 10_000);
        assert!(!room.session.preparing);
        assert_eq!(room.session.start_at_ms, 13_000);
        room.session.apply(Action::Previous, 20_000).unwrap();
        readiness(&mut room, 20_001);
        assert!(room.ready.is_empty());
        room.session.participants = 1;
        room.ready.insert(1);
        readiness(&mut room, 21_000);
        assert!(!room.session.preparing);
        room.session.apply(Action::Previous, 25_000).unwrap();
        readiness(&mut room, 25_001);
        room.failed.insert(1);
        readiness(&mut room, 25_002);
        assert!(room.session.current().is_none());
    }
}
