//! Discord Gateway: a persistent WebSocket session with heartbeats,
//! dispatch handling and reconnect-with-backoff. Sessions always re-IDENTIFY
//! after a drop — RESUME is tracked in docs/PORT.md.

use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message as WsMessage,
};

use crate::model::{Channel, Embed, GuildEmoji, Message, ThreadList, User, VoiceState};
use crate::settings::CustomStatus;

use super::api::{Api, ApiError};
use super::events::{Command, EventTx, UiEvent};
use super::voice::Wire;

const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// The write half of the live gateway socket, for protocol-level sends
/// like op 4.
type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, WsMessage>;

pub enum SessionEnd {
    /// The UI is gone; stop the whole task.
    Shutdown,
    /// Token rejected; stop and return to the login screen.
    Fatal(String),
    /// Retry with backoff.
    Reconnect(String),
}

/// Keeps gateway sessions alive until the process or UI goes away.
pub async fn run(
    token: String,
    mut cmd_rx: UnboundedReceiver<Command>,
    event_tx: EventTx,
    voice_tx: UnboundedSender<Wire>,
) {
    let api = std::sync::Arc::new(Api::new(token));
    let (rest_tx, mut rest_rx) = tokio::sync::mpsc::unbounded_channel();
    let rest_api = api.clone();
    let rest_events = event_tx.clone();
    // HTTP stays ordered, but cannot block voice commands or gateway heartbeats.
    let rest_task = tokio::spawn(async move {
        while let Some(command) = rest_rx.recv().await {
            handle_rest_command(&rest_api, command, &rest_events).await;
        }
    });
    let mut backoff = Duration::from_secs(3);

    loop {
        // A session that died before READY usually means a rejected token;
        // confirm against the REST API before scheduling another retry.
        let mut ready_seen = false;
        let end = session(
            &api,
            &mut cmd_rx,
            &event_tx,
            &voice_tx,
            &rest_tx,
            &mut ready_seen,
        )
        .await;
        match end {
            SessionEnd::Shutdown => break,
            SessionEnd::Fatal(why) => {
                log::error!("gateway session ended fatally: {why}");
                let _ = event_tx.send(UiEvent::Disconnected {
                    reason: why,
                    retry_in: None,
                });
                break;
            }
            SessionEnd::Reconnect(why) => {
                if !ready_seen && matches!(api.me().await, Err(ApiError::Unauthorized)) {
                    log::error!("token rejeitado, voltando ao login");
                    let _ = event_tx.send(UiEvent::TokenInvalid);
                    break;
                }
                log::warn!("gateway session ended: {why}");
                let _ = event_tx.send(UiEvent::Disconnected {
                    reason: why,
                    retry_in: Some(backoff.as_secs()),
                });
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
    rest_task.abort();
}

async fn session(
    api: &Api,
    cmd_rx: &mut UnboundedReceiver<Command>,
    event_tx: &EventTx,
    voice_tx: &UnboundedSender<Wire>,
    rest_tx: &UnboundedSender<Command>,
    ready: &mut bool,
) -> SessionEnd {
    // The gateway session id, from READY — user accounts get no
    // VOICE_STATE_UPDATE for their own op 4, so this is where the voice
    // handshake's session id comes from (docs/VOICE.md).
    let mut session_id = String::new();
    let gateway = match api.gateway_url().await {
        Ok(url) => format!("{}/?v=10&encoding=json", url.trim_end_matches('/')),
        Err(ApiError::Unauthorized) => {
            let _ = event_tx.send(UiEvent::TokenInvalid);
            return SessionEnd::Fatal("token rejeitado pela API".into());
        }
        Err(err) => return SessionEnd::Reconnect(format!("falha ao obter o gateway: {err}")),
    };

    let (ws, _) = match connect_async(gateway.as_str()).await {
        Ok(pair) => pair,
        Err(err) => return SessionEnd::Reconnect(format!("falha ao conectar: {err}")),
    };
    let (mut write, mut read) = ws.split();
    log::info!("gateway connected");
    let _ = event_tx.send(UiEvent::Connected);

    // Discord opens with HELLO (op 10), which carries the heartbeat interval.
    let hello = match read.next().await {
        Some(Ok(msg)) => msg,
        Some(Err(err)) => return SessionEnd::Reconnect(format!("erro no handshake: {err}")),
        None => return SessionEnd::Reconnect("gateway fechou durante o handshake".into()),
    };
    let hello: Value = match hello
        .to_text()
        .map_err(|err| err.to_string())
        .and_then(|text| serde_json::from_str(text).map_err(|err| err.to_string()))
    {
        Ok(value) => value,
        Err(err) => return SessionEnd::Reconnect(format!("HELLO inválido: {err}")),
    };
    let heartbeat_ms = hello
        .pointer("/d/heartbeat_interval")
        .and_then(Value::as_u64)
        .unwrap_or(41_250);

    let mut heartbeat = tokio::time::interval(Duration::from_millis(heartbeat_ms));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seq: Option<u64> = None;
    let mut acked = true;

    let token = api.token();
    if let Err(err) = write.send(WsMessage::text(identify_payload(&token))).await {
        return SessionEnd::Reconnect(format!("falha ao identificar: {err}"));
    }

    loop {
        tokio::select! {
            command = cmd_rx.recv() => match command {
                Some(command) => handle_command(command, rest_tx, &mut write).await,
                None => {
                    let _ = write.send(WsMessage::Close(None)).await;
                    return SessionEnd::Shutdown;
                }
            },
            _ = heartbeat.tick() => {
                if !acked {
                    return SessionEnd::Reconnect("heartbeat sem ACK".into());
                }
                acked = false;
                if let Err(err) = write.send(WsMessage::text(json!({ "op": 1, "d": seq }).to_string())).await {
                    return SessionEnd::Reconnect(format!("falha ao enviar heartbeat: {err}"));
                }
            }
            frame = read.next() => match frame {
                Some(Ok(msg)) => {
                    let text = match msg.to_text() {
                        Ok(text) => text,
                        Err(_) => continue,
                    };
                    let value: Value = match serde_json::from_str(text) {
                        Ok(value) => value,
                        Err(_) => continue,
                    };
                    let op = value.get("op").and_then(Value::as_u64).unwrap_or(0);
                    match op {
                        1 => {
                            // Server-requested heartbeat; answer right away.
                            if let Err(err) = write.send(WsMessage::text(json!({ "op": 1, "d": seq }).to_string())).await {
                                return SessionEnd::Reconnect(format!("falha ao responder heartbeat: {err}"));
                            }
                        }
                        11 => acked = true,
                        7 => return SessionEnd::Reconnect("gateway pediu reconexão".into()),
                        9 => return SessionEnd::Reconnect("sessão inválida, reconectando".into()),
                        0 => {
                            if let Some(s) = value.get("s").and_then(Value::as_u64) {
                                seq = Some(s);
                            }
                            if value.get("t").and_then(Value::as_str) == Some("READY") {
                                *ready = true;
                            }
                            if value.get("t").and_then(Value::as_str) == Some("READY") {
                                if let Some(id) = value.pointer("/d/session_id").and_then(Value::as_str) {
                                    session_id = id.to_string();
                                }
                            }
                            dispatch(value, event_tx, voice_tx, &session_id);
                        }
                        _ => {
                            log::debug!(
                                "gateway op inesperado {op}: {:?}",
                                value.get("t").or_else(|| value.get("d")).map(|v| v.to_string()).unwrap_or_default().chars().take(200).collect::<String>()
                            );
                        }
                    }
                }
                Some(Err(err)) => return SessionEnd::Reconnect(format!("erro no gateway: {err}")),
                None => return SessionEnd::Reconnect("gateway fechou a conexão".into()),
            }
        }
    }
}

// Supplemental guild presences are nested arrays; normal presences are flat.
/// A guild's custom emojis, skipping entries that don't parse.
fn guild_emojis(value: &Value) -> Vec<GuildEmoji> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|emoji| serde::Deserialize::deserialize(emoji).ok())
        .collect()
}

fn seed_presences(value: &Value, event_tx: &EventTx) {
    if let Some(entries) = value.as_array() {
        for entry in entries {
            seed_presences(entry, event_tx);
        }
    } else if let (Some(user_id), Some(status)) = (
        value
            .pointer("/user/id")
            .or_else(|| value.get("user_id"))
            .and_then(Value::as_str),
        value.get("status").and_then(Value::as_str),
    ) {
        // The status dot reflects the device: a phone when the mobile
        // client is the only one online, the usual dot for web/desktop.
        let client_status = value.get("client_status");
        let mobile = client_status
            .and_then(|cs| cs.get("mobile"))
            .and_then(Value::as_str)
            .is_some();
        let on_computer =
            client_status.is_some_and(|cs| cs.get("desktop").is_some() || cs.get("web").is_some());
        let device = if mobile && !on_computer {
            Some("mobile".to_string())
        } else if on_computer {
            Some("desktop".to_string())
        } else {
            None
        };
        event_tx.send(UiEvent::PresenceUpdate {
            user_id: user_id.to_owned(),
            status: status.to_owned(),
            device,
        });
    }
}

fn dispatch(value: Value, event_tx: &EventTx, voice_tx: &UnboundedSender<Wire>, session_id: &str) {
    let kind = value.get("t").and_then(Value::as_str).unwrap_or("");
    log::debug!("gateway dispatch: {kind}");
    let Some(data) = value.get("d").cloned() else {
        return;
    };
    match kind {
        "READY" => {
            seed_presences(&data["presences"], event_tx);
            seed_presences(&data["merged_presences"]["friends"], event_tx);
            seed_presences(&data["merged_presences"]["guilds"], event_tx);
            // Server-side read markers for the "NOVAS" divider. Discord has
            // shipped READY read_state both as a bare array and wrapped in
            // `entries`; accept either. "0" means "never read".
            let read_state = &data["read_state"];
            let entries = read_state
                .as_array()
                .or_else(|| read_state.get("entries").and_then(Value::as_array));
            let markers: Vec<(String, String)> = entries
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let channel_id = entry.get("id")?.as_str()?.to_string();
                    let last_message_id = entry.get("last_message_id")?.as_str()?.to_string();
                    (last_message_id != "0").then_some((channel_id, last_message_id))
                })
                .collect();
            if !markers.is_empty() {
                event_tx.send(UiEvent::ReadState { markers });
            }
            let user = data.pointer("/user").cloned().unwrap_or(Value::Null);
            if let Ok(user) = serde_json::from_value::<User>(user) {
                event_tx.send(UiEvent::Ready { user });
            }
            // User-account READY carries per-guild voice states; seed the
            // sidebar roster with them (empty list clears stale state).
            if let Some(guilds) = data.get("guilds").and_then(Value::as_array) {
                for guild in guilds {
                    seed_presences(&guild["presences"], event_tx);
                    let Some(id) = guild.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    send_roles(id, guild, event_tx);
                    let states = guild
                        .get("voice_states")
                        .cloned()
                        .and_then(|states| serde_json::from_value::<Vec<VoiceState>>(states).ok())
                        .unwrap_or_default();
                    let _ = event_tx.send(UiEvent::GuildVoiceStates {
                        guild_id: id.to_string(),
                        states,
                    });
                    sync_guild_threads(guild, id, event_tx);
                    event_tx.send(UiEvent::GuildEmojis {
                        guild_id: id.to_string(),
                        emojis: guild_emojis(&guild["emojis"]),
                    });
                }
            }
        }
        // A guild came back from an outage or was just joined (full guild,
        // with its voice states), or went away (roster cleared).
        "READY_SUPPLEMENTAL" => {
            seed_presences(&data["merged_presences"]["friends"], event_tx);
            seed_presences(&data["merged_presences"]["guilds"], event_tx);
        }
        "GUILD_CREATE" | "GUILD_DELETE" => {
            seed_presences(&data["presences"], event_tx);
            let Some(id) = data.get("id").and_then(Value::as_str) else {
                return;
            };
            send_roles(id, &data, event_tx);
            let states = data
                .get("voice_states")
                .cloned()
                .and_then(|states| serde_json::from_value::<Vec<VoiceState>>(states).ok())
                .unwrap_or_default();
            let _ = event_tx.send(UiEvent::GuildVoiceStates {
                guild_id: id.to_string(),
                states,
            });
            sync_guild_threads(&data, id, event_tx);
            event_tx.send(UiEvent::GuildEmojis {
                guild_id: id.to_string(),
                emojis: guild_emojis(&data["emojis"]),
            });
        }
        // Threads (docs: Gateway Events → Threads). Active ones arrive with
        // the guild and in THREAD_LIST_SYNC; archived ones only over REST.
        "THREAD_LIST_SYNC" => {
            let Some(guild_id) = data.get("guild_id").and_then(Value::as_str) else {
                return;
            };
            let parent_ids = data
                .get("channel_ids")
                .and_then(|ids| serde_json::from_value::<Vec<String>>(ids.clone()).ok());
            let threads = serde_json::from_value::<ThreadList>(data.clone())
                .unwrap_or_default()
                .into_threads();
            event_tx.send(UiEvent::ThreadsSynced {
                guild_id: guild_id.to_string(),
                parent_ids,
                threads,
            });
        }
        "THREAD_CREATE" | "THREAD_UPDATE" => match serde_json::from_value::<Channel>(data) {
            Ok(thread) => event_tx.send(UiEvent::ThreadsUpserted {
                threads: vec![thread],
            }),
            Err(err) => log::warn!("{kind} não parseou: {err}"),
        },
        "THREAD_DELETE" => {
            if let Some(id) = data.get("id").and_then(Value::as_str) {
                event_tx.send(UiEvent::ThreadDeleted {
                    thread_id: id.to_string(),
                });
            }
        }
        // Our own membership: a thread member object plus guild_id.
        "THREAD_MEMBER_UPDATE" => {
            let id = data.get("id").and_then(Value::as_str);
            let user_id = data.get("user_id").and_then(Value::as_str);
            if let (Some(id), Some(user_id)) = (id, user_id) {
                event_tx.send(UiEvent::ThreadMembers {
                    thread_id: id.to_string(),
                    added: vec![user_id.to_string()],
                    removed: Vec::new(),
                });
            }
        }
        "THREAD_MEMBERS_UPDATE" => {
            let Some(id) = data.get("id").and_then(Value::as_str) else {
                return;
            };
            let ids = |values: Option<&Value>, pointer: &str| -> Vec<String> {
                values
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.pointer(pointer).and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            };
            event_tx.send(UiEvent::ThreadMembers {
                thread_id: id.to_string(),
                added: ids(data.get("added_members"), "/user_id"),
                removed: ids(data.get("removed_member_ids"), ""),
            });
        }
        "GUILD_EMOJIS_UPDATE" => {
            if let Some(id) = data.get("guild_id").and_then(Value::as_str) {
                event_tx.send(UiEvent::GuildEmojis {
                    guild_id: id.to_string(),
                    emojis: guild_emojis(&data["emojis"]),
                });
            }
        }
        "RELATIONSHIP_ADD" | "RELATIONSHIP_UPDATE" | "RELATIONSHIP_REMOVE" => {
            event_tx.send(UiEvent::RelationshipsChanged);
        }
        "MESSAGE_CREATE" => {
            if let Ok(message) = serde_json::from_value::<Message>(data) {
                let _ = event_tx.send(UiEvent::MessageCreated { message });
            }
        }
        "MESSAGE_UPDATE" => {
            if let Ok(patch) = serde_json::from_value::<MessagePatch>(data)
                && (patch.content.is_some() || patch.embeds.is_some())
            {
                let _ = event_tx.send(UiEvent::MessageUpdated {
                    channel_id: patch.channel_id,
                    message_id: patch.id,
                    content: patch.content,
                    embeds: patch.embeds,
                });
            }
        }
        "MESSAGE_DELETE" => {
            let channel_id = data.get("channel_id").and_then(Value::as_str);
            let message_id = data.get("id").and_then(Value::as_str);
            if let (Some(channel_id), Some(message_id)) = (channel_id, message_id) {
                let _ = event_tx.send(UiEvent::MessageDeleted {
                    channel_id: channel_id.to_string(),
                    message_id: message_id.to_string(),
                });
            }
        }
        "TYPING_START" => {
            let channel_id = data.get("channel_id").and_then(Value::as_str);
            let user_id = data.get("user_id").and_then(Value::as_str);
            if let (Some(channel_id), Some(user_id)) = (channel_id, user_id) {
                let _ = event_tx.send(UiEvent::TypingStart {
                    channel_id: channel_id.to_string(),
                    user_id: user_id.to_string(),
                });
            }
        }
        "GUILD_MEMBERS_CHUNK" => {
            // Offline members can be omitted from the presence array.
            if let Some(members) = data["members"].as_array() {
                for member in members {
                    if let Some(user_id) = member.pointer("/user/id").and_then(Value::as_str) {
                        event_tx.send(UiEvent::PresenceUpdate {
                            user_id: user_id.to_owned(),
                            status: "offline".into(),
                            device: None,
                        });
                    }
                }
            }
            seed_presences(&data["presences"], event_tx);
        }
        "PRESENCE_UPDATE" => seed_presences(&data, event_tx),
        "MESSAGE_REACTION_ADD" | "MESSAGE_REACTION_REMOVE" => {
            if let (Some(channel_id), Some(message_id), Some(user_id), Ok(emoji)) = (
                data.get("channel_id").and_then(Value::as_str),
                data.get("message_id").and_then(Value::as_str),
                data.get("user_id").and_then(Value::as_str),
                serde_json::from_value::<crate::model::ReactionEmoji>(
                    data.get("emoji").cloned().unwrap_or(Value::Null),
                ),
            ) {
                let _ = event_tx.send(UiEvent::ReactionChanged {
                    channel_id: channel_id.to_string(),
                    message_id: message_id.to_string(),
                    user_id: user_id.to_string(),
                    emoji,
                    added: kind == "MESSAGE_REACTION_ADD",
                });
            }
        }
        "GUILD_MEMBER_LIST_UPDATE" => {
            match serde_json::from_value::<crate::member_list::MemberListUpdate>(data) {
                Ok(update) => event_tx.send(UiEvent::MemberListUpdate { update }),
                Err(err) => log::warn!("GUILD_MEMBER_LIST_UPDATE não parseou: {err}"),
            }
        }
        "VOICE_STATE_UPDATE" => match serde_json::from_value::<VoiceStatePayload>(data) {
            Ok(payload) => {
                log::info!(
                    "VOICE_STATE_UPDATE: user={} canal={:?} guild={:?} sessão={:?}",
                    payload.state.user_id,
                    payload.state.channel_id,
                    payload.state.guild_id,
                    payload.state.session_id
                );
                // Guild voice states embed the member; harvest the user for
                // the roster names.
                if let Some(user) = payload.member.map(|member| member.user) {
                    let _ = event_tx.send(UiEvent::UserResolved { user });
                }
                let _ = event_tx.send(UiEvent::VoiceStateUpdate {
                    state: payload.state.clone(),
                });
                let _ = voice_tx.send(Wire::State(payload.state));
            }
            Err(err) => log::warn!("VOICE_STATE_UPDATE não parseou: {err}"),
        },
        // Go Live (docs/SCREENSHARE.md). The rtc_* fields and the stream
        // server's endpoint/token feed the stream connection in phase 3.
        "STREAM_CREATE" => {
            let field = |name| {
                data.get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            log::info!(
                "STREAM_CREATE: {} rtc_server={} rtc_channel={} região={}",
                field("stream_key"),
                field("rtc_server_id"),
                field("rtc_channel_id"),
                field("region")
            );
            event_tx.send(UiEvent::StreamCreated {
                stream_key: field("stream_key"),
                rtc_server_id: field("rtc_server_id"),
                rtc_channel_id: field("rtc_channel_id"),
            });
        }
        "STREAM_SERVER_UPDATE" => {
            let field = |name| data.get(name).and_then(Value::as_str).map(str::to_string);
            log::info!(
                "STREAM_SERVER_UPDATE: {:?} endpoint={:?}",
                field("stream_key"),
                field("endpoint")
            );
            event_tx.send(UiEvent::StreamServer {
                stream_key: field("stream_key").unwrap_or_default(),
                endpoint: field("endpoint"),
                token: field("token").unwrap_or_default(),
                session_id: session_id.to_string(),
            });
        }
        "STREAM_DELETE" => {
            let field = |name| {
                data.get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            log::info!("STREAM_DELETE: {data}");
            event_tx.send(UiEvent::StreamDeleted {
                stream_key: field("stream_key"),
                reason: field("reason"),
            });
        }
        // Soundboard sounds; the emoji-only reactions are ignored.
        "VOICE_CHANNEL_EFFECT_SEND" => {
            if let Some(sound_id) = data.get("sound_id").filter(|id| !id.is_null()) {
                event_tx.send(UiEvent::VoiceEffect {
                    user_id: data
                        .get("user_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    sound_id: sound_id
                        .as_str()
                        .map_or_else(|| sound_id.to_string(), str::to_string),
                    volume: data
                        .get("sound_volume")
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0) as f32,
                });
            }
        }
        "VOICE_SERVER_UPDATE" => {
            let guild_id = data.get("guild_id").and_then(Value::as_str);
            let endpoint = data.get("endpoint").and_then(Value::as_str);
            let token = data.get("token").and_then(Value::as_str);
            log::info!(
                "VOICE_SERVER_UPDATE: guild={guild_id:?} endpoint={endpoint:?} token={}",
                token.is_some()
            );
            if let (Some(guild_id), Some(endpoint), Some(token)) = (guild_id, endpoint, token) {
                let _ = voice_tx.send(Wire::Server {
                    guild_id: guild_id.to_string(),
                    endpoint: endpoint.to_string(),
                    token: token.to_string(),
                    session_id: session_id.to_string(),
                });
            }
        }
        _ => {}
    }
}

/// The active threads a guild payload carries (READY, GUILD_CREATE); READY
/// entries omit `guild_id`.
fn sync_guild_threads(guild: &Value, guild_id: &str, event_tx: &EventTx) {
    let Some(threads) = guild.get("threads") else {
        return;
    };
    let mut threads = serde_json::from_value::<Vec<Channel>>(threads.clone()).unwrap_or_default();
    for thread in &mut threads {
        thread.guild_id.get_or_insert_with(|| guild_id.to_string());
    }
    event_tx.send(UiEvent::ThreadsSynced {
        guild_id: guild_id.to_string(),
        parent_ids: None,
        threads,
    });
}

/// Full guild objects (READY, GUILD_CREATE) carry the roles the member
/// list names its groups and colors names with.
fn send_roles(guild_id: &str, guild: &Value, event_tx: &EventTx) {
    if let Ok(roles) = serde_json::from_value(guild["roles"].clone()) {
        event_tx.send(UiEvent::GuildRoles {
            guild_id: guild_id.to_string(),
            roles,
        });
    }
}

#[derive(serde::Deserialize)]
struct MessagePatch {
    id: String,
    channel_id: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    embeds: Option<Vec<Embed>>,
}

/// VOICE_STATE_UPDATE as Discord sends it for guilds, with the embedded
/// member (user account payloads only).
#[derive(serde::Deserialize)]
struct VoiceStatePayload {
    #[serde(flatten)]
    state: VoiceState,
    #[serde(default)]
    member: Option<VoiceMember>,
}

#[derive(serde::Deserialize)]
struct VoiceMember {
    user: User,
}

/// op 37 subscribes to future changes; op 8 fetches the current status.
fn subscribe_payload(guild_id: &str, user_ids: &[String]) -> String {
    let mut subscription = json!({ "typing": true, "activities": true, "threads": true });
    if !user_ids.is_empty() {
        subscription["members"] = json!(user_ids);
    }
    json!({
        "op": 37,
        "d": {
            "subscriptions": {
                guild_id: subscription
            }
        }
    })
    .to_string()
}

fn presence_requests(guild_id: &str, user_ids: &[String]) -> Vec<String> {
    user_ids
        .chunks(100)
        .map(|ids| {
            json!({
                "op": 8,
                "d": { "guild_id": guild_id, "user_ids": ids, "presences": true }
            })
            .to_string()
        })
        .collect()
}

fn identify_payload(token: &str) -> String {
    // No `intents`: like the official client. On a user account they act
    // as a filter, and with them Discord stopped delivering
    // VOICE_STATE_UPDATE (channel rosters and our own session).
    json!({
        "op": 2,
        "d": {
            "token": token,
            "properties": {
                "os": std::env::consts::OS,
                "browser": "FastDiscord",
                "device": "FastDiscord"
            },
            "presence": { "status": "online", "afk": false }
        }
    })
    .to_string()
}

/// op 3 PRESENCE_UPDATE; a custom status is the type 4 activity, with
/// the text in `state` as the official client sends it.
fn presence_payload(status: &str, custom: &CustomStatus) -> String {
    let activities = if custom.is_empty() {
        vec![]
    } else {
        let emoji = custom.emoji.trim();
        vec![json!({
            "type": 4,
            "name": "Custom Status",
            "state": custom.text.trim(),
            "emoji": (!emoji.is_empty()).then(|| json!({ "name": emoji })),
        })]
    };
    json!({
        "op": 3,
        "d": { "status": status, "since": 0, "activities": activities, "afk": false }
    })
    .to_string()
}

async fn handle_command(command: Command, rest_tx: &UnboundedSender<Command>, write: &mut WsSink) {
    match command {
        Command::SetPresence {
            status,
            custom_status,
            save,
        } => {
            let payload = presence_payload(&status, &custom_status);
            if let Err(err) = write.send(WsMessage::text(payload)).await {
                log::warn!("falha ao atualizar o status: {err}");
            }
            if save {
                let _ = rest_tx.send(Command::SetPresence {
                    status,
                    custom_status,
                    save,
                });
            }
        }
        Command::JoinVoice {
            guild_id,
            channel_id,
            self_mute,
            self_deaf,
        } => {
            log::info!("enviando op 4: guild={guild_id} canal={channel_id}");
            let payload = json!({
                "op": 4,
                "d": {
                    "guild_id": guild_id,
                    "channel_id": channel_id,
                    "self_mute": self_mute,
                    "self_deaf": self_deaf,
                }
            });
            if let Err(err) = write.send(WsMessage::text(payload.to_string())).await {
                log::warn!("falha ao pedir entrada no canal de voz: {err}");
            }
        }
        Command::LeaveVoice { guild_id } => {
            let payload = json!({
                "op": 4,
                "d": {
                    "guild_id": guild_id,
                    "channel_id": Value::Null,
                    "self_mute": false,
                    "self_deaf": false,
                }
            });
            if let Err(err) = write.send(WsMessage::text(payload.to_string())).await {
                log::warn!("falha ao sair do canal de voz: {err}");
            }
        }
        Command::StartStream {
            guild_id,
            channel_id,
            stream_key,
        } => {
            log::info!("enviando op 18: guild={guild_id} canal={channel_id}");
            let create = json!({
                "op": 18,
                "d": {
                    "type": "guild",
                    "guild_id": guild_id,
                    "channel_id": channel_id,
                    "preferred_region": Value::Null,
                }
            });
            // Discord-video-stream unpauses right after creating.
            let unpause = json!({ "op": 22, "d": { "stream_key": stream_key, "paused": false } });
            for payload in [create, unpause] {
                if let Err(err) = write.send(WsMessage::text(payload.to_string())).await {
                    log::warn!("falha ao criar a transmissão: {err}");
                }
            }
        }
        Command::WatchStream { stream_key } => {
            log::info!("enviando op 20: {stream_key}");
            let payload = json!({ "op": 20, "d": { "stream_key": stream_key } });
            if let Err(err) = write.send(WsMessage::text(payload.to_string())).await {
                log::warn!("falha ao assistir a transmissão: {err}");
            }
        }
        Command::StopStream { stream_key } => {
            let payload = json!({ "op": 19, "d": { "stream_key": stream_key } });
            if let Err(err) = write.send(WsMessage::text(payload.to_string())).await {
                log::warn!("falha ao encerrar a transmissão: {err}");
            }
        }
        Command::SubscribeGuild { guild_id, user_ids } => {
            if let Err(err) = write
                .send(WsMessage::text(subscribe_payload(&guild_id, &user_ids)))
                .await
            {
                log::warn!("falha ao assinar a guild {guild_id}: {err}");
            }
            for payload in presence_requests(&guild_id, &user_ids) {
                if let Err(err) = write.send(WsMessage::text(payload)).await {
                    log::warn!("falha ao consultar presenças da guild {guild_id}: {err}");
                    break;
                }
            }
        }
        Command::SubscribeMemberList {
            guild_id,
            channel_id,
            ranges,
        } => {
            let payload = crate::member_list::subscribe_payload(&guild_id, &channel_id, ranges);
            if let Err(err) = write.send(WsMessage::text(payload)).await {
                log::warn!("falha ao assinar a lista de membros de {channel_id}: {err}");
            }
        }
        command => {
            let _ = rest_tx.send(command);
        }
    }
}

async fn handle_rest_command(api: &Api, command: Command, event_tx: &EventTx) {
    match command {
        Command::SetPresence {
            status,
            custom_status,
            save,
        } => {
            if save && let Err(err) = api.save_status(&status, &custom_status).await {
                log::warn!("falha ao salvar o status na conta: {err}");
            }
        }
        Command::JoinVoice { .. }
        | Command::LeaveVoice { .. }
        | Command::StartStream { .. }
        | Command::WatchStream { .. }
        | Command::StopStream { .. }
        | Command::SubscribeGuild { .. }
        | Command::SubscribeMemberList { .. } => unreachable!("gateway command routed to REST"),

        Command::LoadGuilds => match api.guilds().await {
            Ok(guilds) => {
                let _ = event_tx.send(UiEvent::GuildsLoaded { guilds });
            }
            Err(err) => report(api, err, event_tx, "carregar servidores").await,
        },
        Command::LoadDmChannels => match api.dm_channels().await {
            Ok(channels) => {
                let _ = event_tx.send(UiEvent::DmChannelsLoaded { channels });
            }
            Err(err) => report(api, err, event_tx, "carregar mensagens diretas").await,
        },
        Command::LoadGuildChannels { guild_id } => match api.guild_channels(&guild_id).await {
            Ok(channels) => {
                let _ = event_tx.send(UiEvent::GuildChannelsLoaded { guild_id, channels });
            }
            Err(err) => report(api, err, event_tx, "carregar canais").await,
        },
        Command::LoadRelationships => match api.relationships().await {
            Ok(relationships) => event_tx.send(UiEvent::RelationshipsLoaded { relationships }),
            Err(error) => event_tx.send(UiEvent::RelationshipError {
                error: error.to_string(),
            }),
        },
        Command::RequestFriend { username } => match api.request_friend(&username).await {
            Ok(()) => event_tx.send(UiEvent::RelationshipsChanged),
            Err(error) => event_tx.send(UiEvent::RelationshipError {
                error: error.to_string(),
            }),
        },
        Command::AcceptFriend { user_id } => match api.accept_friend(&user_id).await {
            Ok(()) => event_tx.send(UiEvent::RelationshipsChanged),
            Err(error) => event_tx.send(UiEvent::RelationshipError {
                error: error.to_string(),
            }),
        },
        Command::RemoveRelationship { user_id } => match api.remove_relationship(&user_id).await {
            Ok(()) => event_tx.send(UiEvent::RelationshipsChanged),
            Err(error) => event_tx.send(UiEvent::RelationshipError {
                error: error.to_string(),
            }),
        },
        Command::SetGuildMute { guild_id, muted } => {
            if let Err(error) = api.set_guild_mute(&guild_id, muted).await {
                log::warn!(
                    "falha ao {} o servidor: {error}",
                    if muted { "silenciar" } else { "dessilenciar" }
                );
            }
        }
        Command::LeaveGuild { guild_id } => match api.leave_guild(&guild_id).await {
            Ok(()) => event_tx.send(UiEvent::GuildLeft { guild_id }),
            Err(error) => log::warn!("falha ao sair do servidor: {error}"),
        },
        Command::OpenDm { user_id } => match api.open_dm(&user_id).await {
            Ok(channel) => event_tx.send(UiEvent::DmOpened { channel }),
            Err(error) => event_tx.send(UiEvent::RelationshipError {
                error: error.to_string(),
            }),
        },
        Command::LoadMessages {
            channel_id,
            before,
            after,
        } => {
            match api
                .messages(&channel_id, before.as_deref(), after.as_deref())
                .await
            {
                Ok(messages) => {
                    let _ = event_tx.send(UiEvent::MessagesLoaded {
                        older: before.is_some(),
                        newer: after.is_some(),
                        channel_id,
                        messages,
                    });
                }
                Err(err) => {
                    let _ = event_tx.send(UiEvent::MessagesFailed { channel_id });
                    report(api, err, event_tx, "carregar mensagens").await
                }
            }
        }
        Command::SendMessage {
            channel_id,
            content,
        } => match api.send_message(&channel_id, &content).await {
            Ok(message) => {
                let _ = event_tx.send(UiEvent::MessageCreated { message });
            }
            Err(err) => {
                let _ = event_tx.send(UiEvent::SendFailed {
                    channel_id,
                    error: err.to_string(),
                });
            }
        },
        Command::EditMessage {
            channel_id,
            message_id,
            content,
        } => match api.edit_message(&channel_id, &message_id, &content).await {
            Ok(message) => event_tx.send(UiEvent::MessageUpdated {
                channel_id,
                message_id,
                content: Some(message.content),
                embeds: Some(message.embeds),
            }),
            Err(err) => event_tx.send(UiEvent::SendFailed {
                channel_id,
                error: format!("não consegui editar a mensagem: {err}"),
            }),
        },
        Command::DeleteMessage {
            channel_id,
            message_id,
        } => match api.delete_message(&channel_id, &message_id).await {
            // MESSAGE_DELETE follows; removing it now feels instant.
            Ok(()) => event_tx.send(UiEvent::MessageDeleted {
                channel_id,
                message_id,
            }),
            Err(err) => event_tx.send(UiEvent::SendFailed {
                channel_id,
                error: format!("não consegui apagar a mensagem: {err}"),
            }),
        },
        Command::ToggleReaction {
            channel_id,
            message_id,
            emoji,
            add,
        } => {
            if let Err(err) = api
                .toggle_reaction(&channel_id, &message_id, &emoji, add)
                .await
            {
                log::warn!("falha ao reagir com {emoji}: {err}");
            }
        }
        Command::SendTyping { channel_id } => {
            if let Err(err) = api.send_typing(&channel_id).await {
                log::warn!("falha ao avisar digitação: {err}");
            }
        }
        Command::LoadVoiceUser { guild_id, user_id } => {
            match api.guild_member(&guild_id, &user_id).await {
                Ok(member) => {
                    let _ = event_tx.send(UiEvent::UserResolved { user: member.user });
                }
                Err(err) => log::warn!("falha ao carregar membro {user_id}: {err}"),
            }
        }
        Command::LoadThreads { channel_id } => match api.archived_threads(&channel_id).await {
            Ok(threads) => event_tx.send(UiEvent::ThreadsUpserted { threads }),
            Err(err) => report(api, err, event_tx, "carregar tópicos").await,
        },
        Command::SearchForumPosts {
            forum_id,
            sort_by,
            offset,
        } => match api.search_forum_posts(&forum_id, sort_by, offset).await {
            Ok(page) => event_tx.send(UiEvent::ForumPosts {
                forum_id,
                sort_by,
                threads: page.list.into_threads(),
                first_messages: page.first_messages,
                has_more: page.has_more,
            }),
            Err(err) => {
                // Unblocks the forum's paging, then says why.
                event_tx.send(UiEvent::ForumPosts {
                    forum_id,
                    sort_by,
                    threads: Vec::new(),
                    first_messages: Vec::new(),
                    has_more: false,
                });
                report(api, err, event_tx, "carregar postagens").await
            }
        },
        Command::LoadForumPostData {
            forum_id,
            thread_ids,
        } => match api.forum_post_data(&forum_id, &thread_ids).await {
            Ok(messages) => event_tx.send(UiEvent::ForumFirstMessages { messages }),
            Err(err) => log::warn!("falha ao carregar prévias do fórum {forum_id}: {err}"),
        },
        Command::CreateThread {
            channel_id,
            message_id,
            name,
            content,
            kind,
            forum,
        } => {
            let body = if forum {
                json!({ "name": name, "message": { "content": content } })
            } else if message_id.is_some() {
                json!({ "name": name })
            } else {
                // v10 defaults a standalone thread to private; ask for the
                // public kind the browser offers.
                json!({ "name": name, "type": kind })
            };
            let created = api
                .create_thread(&channel_id, message_id.as_deref(), &body)
                .await;
            match created {
                Ok(thread) => {
                    let thread_id = thread.id.clone();
                    event_tx.send(UiEvent::ThreadCreated { thread });
                    if !forum && message_id.is_none() && !content.is_empty() {
                        match api.send_message(&thread_id, &content).await {
                            Ok(message) => event_tx.send(UiEvent::MessageCreated { message }),
                            Err(err) => event_tx.send(UiEvent::SendFailed {
                                channel_id: thread_id,
                                error: err.to_string(),
                            }),
                        }
                    }
                }
                Err(err) => event_tx.send(UiEvent::SendFailed {
                    channel_id,
                    error: format!("não consegui criar o tópico: {err}"),
                }),
            }
        }
        Command::LoadMessagesAround {
            channel_id,
            message_id,
        } => {
            let messages = match api.messages_around(&channel_id, &message_id).await {
                Ok(messages) => messages,
                Err(err) => {
                    log::warn!("falha ao buscar a mensagem {message_id}: {err}");
                    Vec::new()
                }
            };
            event_tx.send(UiEvent::MessagesAround {
                channel_id,
                message_id,
                messages,
            });
        }
        Command::SearchGifs { query } => {
            let result = api.gifs(&query).await.map_err(|err| err.to_string());
            event_tx.send(UiEvent::GifsLoaded { query, result });
        }
        Command::LoadSoundboard { guild_id } => match api.soundboard_sounds(&guild_id).await {
            Ok(sounds) => event_tx.send(UiEvent::SoundboardLoaded { guild_id, sounds }),
            Err(err) => event_tx.send(UiEvent::SoundboardError {
                guild_id: Some(guild_id),
                error: format!("não consegui carregar os sons: {err}"),
            }),
        },
        Command::SendSoundboard {
            channel_id,
            sound_id,
            source_guild_id,
        } => {
            let sent = api
                .send_soundboard_sound(&channel_id, &sound_id, source_guild_id.as_deref())
                .await;
            if let Err(err) = sent {
                event_tx.send(UiEvent::SoundboardError {
                    guild_id: None,
                    error: format!("não consegui tocar o som: {err}"),
                });
            }
        }
    }
}

/// Surfaces REST failures to the UI, treating an expired token specially.
async fn report(api: &Api, err: ApiError, event_tx: &EventTx, context: &str) {
    if matches!(err, ApiError::Unauthorized)
        && matches!(api.me().await, Err(ApiError::Unauthorized))
    {
        let _ = event_tx.send(UiEvent::TokenInvalid);
    }
    let _ = event_tx.send(UiEvent::Error {
        context: format!("{context}: {err}"),
    });
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn queued_http_does_not_delay_voice_join() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::protocol::Role;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
            tokio_tungstenite::MaybeTlsStream::Plain(client),
            Role::Client,
            None,
        )
        .await;
        let (mut write, _) = socket.split();
        let mut server =
            tokio_tungstenite::WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        let (rest_tx, mut rest_rx) = tokio::sync::mpsc::unbounded_channel();

        super::handle_command(super::Command::LoadGuilds, &rest_tx, &mut write).await;
        // The HTTP worker has not completed (or even consumed) the queued request.
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            super::handle_command(
                super::Command::JoinVoice {
                    guild_id: "1".into(),
                    channel_id: "2".into(),
                    self_mute: false,
                    self_deaf: false,
                },
                &rest_tx,
                &mut write,
            )
            .await;
            let frame = server.next().await.unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(value["op"], 4);
            assert_eq!(value["d"]["channel_id"], "2");
        })
        .await
        .unwrap();
        assert!(matches!(rest_rx.try_recv(), Ok(super::Command::LoadGuilds)));
        assert!(rest_rx.try_recv().is_err());
    }

    #[test]
    fn presence_payload_carries_custom_status_as_activity() {
        let custom = CustomStatus {
            text: " no ar ".into(),
            emoji: "🎧".into(),
        };
        let value: Value = serde_json::from_str(&presence_payload("dnd", &custom)).unwrap();
        assert_eq!(value["op"], 3);
        assert_eq!(value["d"]["status"], "dnd");
        let activity = &value["d"]["activities"][0];
        assert_eq!(activity["type"], 4);
        assert_eq!(activity["state"], "no ar");
        assert_eq!(activity["emoji"]["name"], "🎧");

        let plain: Value =
            serde_json::from_str(&presence_payload("idle", &CustomStatus::default())).unwrap();
        assert_eq!(plain["d"]["activities"], json!([]));
    }

    #[test]
    fn subscriptions_request_current_and_future_member_presences() {
        let ids: Vec<String> = (0..101).map(|id| id.to_string()).collect();
        let subscription: Value = serde_json::from_str(&subscribe_payload("guild", &ids)).unwrap();
        assert_eq!(
            subscription["d"]["subscriptions"]["guild"]["members"],
            json!(ids)
        );
        assert_eq!(subscription["d"]["subscriptions"]["guild"]["typing"], true);
        let requests: Vec<Value> = presence_requests("guild", &ids)
            .iter()
            .map(|payload| serde_json::from_str(payload).unwrap())
            .collect();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["d"]["user_ids"].as_array().unwrap().len(), 100);
        assert_eq!(requests[1]["d"]["user_ids"], json!(["100"]));
        for request in requests {
            assert_eq!(request["op"], 8);
            assert_eq!(request["d"]["guild_id"], "guild");
            assert_eq!(request["d"]["presences"], true);
        }
        assert!(presence_requests("guild", &[]).is_empty());
        let subscription: Value = serde_json::from_str(&subscribe_payload("guild", &[])).unwrap();
        assert!(
            subscription["d"]["subscriptions"]["guild"]
                .get("members")
                .is_none()
        );
    }

    #[test]
    fn ready_and_member_chunks_seed_statuses_including_offline_members() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventTx::new(tx, egui::Context::default());
        let (voice_tx, _) = tokio::sync::mpsc::unbounded_channel();
        dispatch(
            json!({"t": "READY", "d": {"merged_presences": {
                "friends": [{"user_id": "1", "status": "online"}],
                "guilds": [[{"user_id": "2", "status": "idle"}]]
            }}}),
            &events,
            &voice_tx,
            "",
        );
        dispatch(
            json!({"t": "GUILD_MEMBERS_CHUNK", "d": {
                "members": [{"user": {"id": "2"}}, {"user": {"id": "3"}}],
                "presences": [{"user": {"id": "2"}, "status": "dnd"}]
            }}),
            &events,
            &voice_tx,
            "",
        );
        let mut statuses = std::collections::HashMap::new();
        while let Ok(UiEvent::PresenceUpdate {
            user_id, status, ..
        }) = rx.try_recv()
        {
            statuses.insert(user_id, status);
        }
        assert_eq!(statuses.get("1").map(String::as_str), Some("online"));
        assert_eq!(statuses.get("2").map(String::as_str), Some("dnd"));
        assert_eq!(statuses.get("3").map(String::as_str), Some("offline"));
    }

    #[test]
    fn initial_presence_handles_flat_and_supplemental_lists() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = super::EventTx::new(tx, egui::Context::default());
        super::seed_presences(
            &serde_json::json!([
                {"user": {"id": "1"}, "status": "online"},
                [{"user_id": "2", "status": "idle"}],
                {"user_id": "3"}
            ]),
            &events,
        );
        assert!(
            matches!(rx.try_recv().unwrap(), super::UiEvent::PresenceUpdate { user_id, status, .. } if user_id == "1" && status == "online")
        );
        assert!(
            matches!(rx.try_recv().unwrap(), super::UiEvent::PresenceUpdate { user_id, status, .. } if user_id == "2" && status == "idle")
        );
        assert!(rx.try_recv().is_err());
    }

    /// The status dot reflects the device: a phone only when the mobile
    /// client is the sole one online; web/desktop keep the dot.
    #[test]
    fn presence_picks_the_device_behind_the_status() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = super::EventTx::new(tx, egui::Context::default());
        super::seed_presences(
            &serde_json::json!([
                {"user": {"id": "1"}, "status": "online", "client_status": {"mobile": "online"}},
                {"user": {"id": "2"}, "status": "online", "client_status": {"mobile": "online", "desktop": "idle"}},
                {"user": {"id": "3"}, "status": "online", "client_status": {"web": "online"}},
                {"user": {"id": "4"}, "status": "offline"},
                {"user": {"id": "5"}, "status": "online"}
            ]),
            &events,
        );
        let device =
            |rx: &mut tokio::sync::mpsc::UnboundedReceiver<UiEvent>| match rx.try_recv().unwrap() {
                UiEvent::PresenceUpdate {
                    user_id, device, ..
                } => (user_id, device),
                _ => panic!("expected a presence update"),
            };
        assert_eq!(device(&mut rx), ("1".into(), Some("mobile".into())));
        assert_eq!(device(&mut rx), ("2".into(), Some("desktop".into())));
        assert_eq!(device(&mut rx), ("3".into(), Some("desktop".into())));
        assert_eq!(device(&mut rx), ("4".into(), None));
        assert_eq!(device(&mut rx), ("5".into(), None));
    }

    use super::*;

    /// VOICE_STATE_UPDATE real shape: flattened state + embedded member.
    #[test]
    fn voice_state_update_parses_with_member() {
        let data = serde_json::json!({
            "guild_id": "914890968354918401",
            "channel_id": "914890968354918404",
            "user_id": "111111111111111111",
            "session_id": "e4b3f5a1c9d2",
            "deaf": false,
            "mute": false,
            "self_deaf": false,
            "self_mute": false,
            "suppress": false,
            "member": {
                "user": { "id": "111111111111111111", "username": "fulano" },
                "roles": ["914890968354918400"]
            }
        });
        let payload: VoiceStatePayload = serde_json::from_value(data).expect("deve parsear");
        assert_eq!(payload.state.user_id, "111111111111111111");
        assert_eq!(payload.state.session_id, "e4b3f5a1c9d2");
        assert_eq!(
            payload.state.guild_id.as_deref(),
            Some("914890968354918401")
        );
    }
}
