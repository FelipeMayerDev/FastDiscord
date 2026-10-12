//! In-memory DJ relay: rooms disappear when their last connection leaves.
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, broadcast};
use tokio_tungstenite::tungstenite::Message;

use super::dj::{ClientMessage, ServerMessage, Session, VERSION, now_ms, websocket_config};

struct Room {
    prepared_epoch: u64,
    ready: HashSet<u64>,
    failed: HashSet<u64>,
    session: Session,
    updates: broadcast::Sender<ServerMessage>,
}
type Rooms = Arc<Mutex<HashMap<String, Room>>>;

pub async fn serve(listener: TcpListener, key: String) -> Result<()> {
    if !(16..=256).contains(&key.len()) {
        bail!("FASTDISCORD_DJ_KEY must contain 16–256 characters");
    }
    let rooms: Rooms = Arc::default();
    let capacity = Arc::new(Semaphore::new(256));
    loop {
        let (stream, _) = listener.accept().await.context("accept DJ connection")?;
        let Ok(permit) = capacity.clone().try_acquire_owned() else {
            continue;
        };
        let rooms = rooms.clone();
        let key = key.clone();
        tokio::spawn(async move {
            let _permit = permit;
            // Protocol/authentication errors are deliberately not logged with client input.
            let _ = peer(stream, &key, rooms).await;
        });
    }
}
fn valid_room(room: &str) -> bool {
    let Some((guild, channel)) = room.split_once(':') else {
        return false;
    };
    [guild, channel]
        .iter()
        .all(|id| !id.is_empty() && id.len() <= 20 && id.bytes().all(|c| c.is_ascii_digit()))
}
fn key_matches(a: &str, b: &str) -> bool {
    // Compare all bytes without leaking the first differing character.
    let mut difference = a.len() ^ b.len();
    for (i, byte) in b.bytes().enumerate() {
        difference |= usize::from(byte ^ a.as_bytes().get(i).copied().unwrap_or(0));
    }
    difference == 0
}
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
}
impl Drop for Membership {
    fn drop(&mut self) {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(room) = rooms.get_mut(&self.room) {
            room.session.participants -= 1;
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
async fn peer(stream: TcpStream, key: &str, rooms: Rooms) -> Result<()> {
    stream.set_nodelay(true)?;
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
        ClientMessage::Join { version, .. } if version != VERSION => {
            ws.send(Message::text(serde_json::to_string(
                &ServerMessage::Error {
                    message:
                        "Versão incompatível do DJ; atualize o app e o relay para 0.2.0 ou superior"
                            .into(),
                },
            )?))
            .await?;
            ws.close(None).await?;
            return Ok(());
        }
        ClientMessage::Join {
            version,
            room,
            key: supplied,
        } if version == VERSION && valid_room(&room) && key_matches(&supplied, key) => room,
        _ => {
            ws.send(Message::text(serde_json::to_string(
                &ServerMessage::Error {
                    message: "Chave, sala ou versão do DJ inválida".into(),
                },
            )?))
            .await?;
            ws.close(None).await?;
            return Ok(());
        }
    };
    let peer_id = NEXT_PEER.fetch_add(1, Ordering::Relaxed);
    let mut updates = {
        let mut rooms = rooms.lock().unwrap();
        let room = rooms.entry(room_id.clone()).or_insert_with(|| Room {
            prepared_epoch: 0,
            ready: HashSet::new(),
            failed: HashSet::new(),
            session: Session::default(),
            updates: broadcast::channel(64).0,
        });
        room.session.participants += 1;
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
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
    type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
    async fn join(url: &str, room: &str, key: &str) -> Socket {
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        ws.send(Message::text(
            serde_json::to_string(&ClientMessage::Join {
                version: VERSION,
                room: room.into(),
                key: key.into(),
            })
            .unwrap(),
        ))
        .await
        .unwrap();
        ws
    }
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
    async fn two_clients_share_controls_late_join_and_isolated_rooms() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let key = "test-relay-key-1234";
        let server = tokio::spawn(serve(listener, key.into()));
        let mut a = join(&url, "1:2", key).await;
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
        let mut b = join(&url, "1:2", key).await;
        let joined = state(&mut b, 2, 1).await;
        assert_eq!(joined.epoch, first.epoch);
        assert_eq!(joined.start_at_ms, first.start_at_ms);
        action(&mut b, Action::Pause).await;
        assert!(state(&mut a, 2, 2).await.paused);
        let mut other = join(&url, "1:3", key).await;
        assert!(state(&mut other, 1, 0).await.tracks.is_empty());
        action(&mut a, Action::Stop).await;
        assert!(state(&mut b, 2, 3).await.current().is_none());
        b.close(None).await.unwrap();
        assert_eq!(state(&mut a, 1, 3).await.participants, 1);
        let mut wrong = join(&url, "1:2", "incorrect-key-1234").await;
        let msg = wrong.next().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerMessage>(msg.to_text().unwrap()).unwrap(),
            ServerMessage::Error { .. }
        ));
        server.abort();
    }
    #[test]
    fn playback_waits_for_downloads_and_handles_failure_and_departure() {
        let mut room = Room {
            prepared_epoch: 0,
            ready: HashSet::new(),
            failed: HashSet::new(),
            session: Session::default(),
            updates: broadcast::channel(64).0,
        };
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
    #[test]
    fn room_and_key_validation() {
        assert!(valid_room("123:456"));
        assert!(!valid_room("1:2:3"));
        assert!(!valid_room("../room:1"));
        assert!(key_matches("secret", "secret"));
        assert!(!key_matches("secret1", "secret"));
        assert!(!key_matches("xecret", "secret"));
    }
}
