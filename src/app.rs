//! Application state and root UI orchestration: panels, event routing,
//! selection bookkeeping and settings persistence.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::Context;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::backend::audio::{Sound, play_sound};
use crate::backend::voice::{AudioConfig, VoiceCommand};
use crate::backend::{self, Command, UiEvent};
use crate::image_cache::ImageCache;
use crate::model::{Channel, Guild, Message, Relationship, User, VoiceState};
use crate::settings::{Settings, Theme};
use crate::theme;
use crate::ui;

/// Commands the experimental tray can hand to the app.
pub enum TrayCommand {
    Quit,
}

/// What the connection indicator should show right now.
#[derive(Debug, Clone)]
pub(crate) enum ConnState {
    Connecting,
    Connected,
    Disconnected(String, Option<u64>),
    LoginError(String),
}

/// Where the login screen's QR-code session stands.
#[derive(Debug, Clone)]
pub(crate) enum QrState {
    Idle,
    Loading,
    Code(String),
    Scanned {
        username: String,
        user_id: String,
        avatar: Option<String>,
    },
    Failed(String),
}

/// How a channel id maps back to its owner, for unread routing and titles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChannelRef {
    Guild(String),
    Dm,
}

/// The single voice connection a user account can hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VoiceConn {
    Disconnected,
    Connecting {
        guild_id: String,
        channel_id: String,
    },
    Connected {
        guild_id: String,
        channel_id: String,
    },
    Failed {
        error: String,
    },
}

/// One Go Live stream we take part in, as its streamer or a viewer.
pub(crate) struct GoLive {
    pub(crate) key: String,
    /// `(rtc_server_id, rtc_channel_id)` from STREAM_CREATE.
    rtc: Option<(String, u64)>,
    /// The stream connection (docs/SCREENSHARE.md).
    task: Option<JoinHandle<()>>,
    /// Viewer only: the latest decoded frame.
    pub(crate) frames: Arc<Mutex<Option<crate::backend::capture::Frame>>>,
    /// Set when the stream is a FockyTV live watched over WHEP instead.
    pub(crate) fockytv: Option<crate::backend::fockytv::Viewer>,
}

impl GoLive {
    fn new(key: String) -> Self {
        Self {
            key,
            rtc: None,
            task: None,
            frames: Arc::default(),
            fockytv: None,
        }
    }
}

impl Drop for GoLive {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub struct VesktopApp {
    pub(crate) settings: Settings,
    pub(crate) handle: tokio::runtime::Handle,
    event_tx: backend::EventTx,
    pub(crate) event_rx: UnboundedReceiver<UiEvent>,
    pub(crate) cmd_tx: UnboundedSender<Command>,
    backend: Option<JoinHandle<()>>,
    qr_task: Option<JoinHandle<()>>,
    /// Voice driver task; commanded through `voice_cmd_tx` (join/leave).
    voice_task: Option<JoinHandle<()>>,
    voice_cmd_tx: UnboundedSender<backend::voice::VoiceCommand>,
    pub(crate) qr: QrState,
    #[cfg_attr(not(feature = "tray"), allow(dead_code))]
    tray_tx: std::sync::mpsc::Sender<TrayCommand>,
    pub(crate) tray_rx: std::sync::mpsc::Receiver<TrayCommand>,

    pub(crate) conn: ConnState,
    pub(crate) me: Option<User>,
    pub(crate) guilds: Vec<Guild>,
    pub(crate) dm_channels: Vec<Channel>,
    pub(crate) guild_channels: HashMap<String, Vec<Channel>>,
    pub(crate) channel_index: HashMap<String, ChannelRef>,
    pub(crate) messages: HashMap<String, Vec<Message>>,
    pub(crate) has_more: HashMap<String, bool>,
    /// Cached channels that may have missed messages while the gateway was
    /// down, with the last message cached before the drop: caught up with
    /// an `after=` fetch from there when next opened.
    pub(crate) stale_channels: HashMap<String, String>,
    pub(crate) loading_channels: HashSet<String>,
    pub(crate) loading_guilds: HashSet<String>,
    pub(crate) unread: HashMap<String, u64>,
    pub(crate) mentions: HashMap<String, u64>,
    /// First unread message per channel, for the red "NOVAS" divider.
    pub(crate) first_unread: HashMap<String, String>,
    /// Last message the user has seen (timeline was at the bottom).
    pub(crate) last_seen: HashMap<String, String>,
    /// Per-channel, per-user instant of the last TYPING_START.
    pub(crate) typing: HashMap<String, HashMap<String, std::time::Instant>>,
    /// Latest presence status per user id ("online", "idle", "dnd", …).
    pub(crate) presence: HashMap<String, String>,
    /// When the last Disconnected event landed, to count the retry down.
    pub(crate) disconnected_at: Option<std::time::Instant>,
    /// Who is in which voice channel: guild → user → state.
    pub(crate) voice_states: HashMap<String, HashMap<String, VoiceState>>,
    /// The user's own voice connection, for the sidebar panel.
    pub(crate) voice: VoiceConn,
    pub(crate) voice_connected_at: Option<Instant>,
    /// Rejoin timestamps from DAVE desync self-heals (songbird #310):
    /// bounded so a broken session can't loop forever.
    desync_rejoins: Vec<std::time::Instant>,
    /// Users currently producing audio, for the green indicators.
    pub(crate) speaking: HashSet<String>,
    /// Own mic/headphone flags; op 4 sends them, the panel shows them.
    pub(crate) voice_muted: bool,
    /// Screen share capture (docs/SCREENSHARE.md); local preview for now.
    pub(crate) screen: Option<crate::backend::capture::Capture>,
    pub(crate) screen_texture: Option<egui::TextureHandle>,
    /// Our Go Live stream while op 18 is in effect.
    pub(crate) stream: Option<GoLive>,
    /// The stream we watch (op 20), with its decoded frames.
    pub(crate) watch: Option<GoLive>,
    pub(crate) watch_texture: Option<egui::TextureHandle>,
    /// Our WHIP publish when FockyTV is the share backend.
    pub(crate) fockytv: Option<crate::backend::fockytv::Publisher>,
    /// FockyTV lives now: lowercased key (matched to usernames) → key.
    pub(crate) fockytv_live: HashMap<String, String>,
    fockytv_polled: Option<Instant>,
    pub(crate) voice_deaf: bool,
    /// Voice-state user ids we already asked the API for.
    voice_users_pending: HashSet<String>,
    pub(crate) selected_guild: Option<String>,
    pub(crate) selected_channel: Option<String>,
    pub(crate) user_cache: HashMap<String, User>,
    pub(crate) images: ImageCache,
    /// Larger cache for inline image attachments.
    pub(crate) images_large: ImageCache,
    /// The image opened from the chat in its own window.
    pub(crate) viewer: Option<ui::image_viewer::ImageViewer>,

    pub(crate) compose: String,
    pub(crate) compose_error: Option<String>,
    /// Whether the compose box changed since last frame (typing signal).
    pub(crate) last_compose: String,
    pub(crate) last_typing_sent: Option<std::time::Instant>,
    /// Set by "Ir para o presente", consumed inside the timeline's scroll.
    pub(crate) jump_to_present: bool,
    pub(crate) settings_open: bool,
    /// Page the settings window shows (left nav).
    pub(crate) settings_tab: ui::settings_window::SettingsTab,
    pub(crate) login_token: String,
    /// "Encontre ou comece uma conversa" filter for the DM list.
    pub(crate) dm_search: String,
    pub(crate) home_page: ui::friends::HomePage,
    pub(crate) relationships: Vec<Relationship>,
    pub(crate) relationships_loading: bool,
    pub(crate) friend_input: String,
    pub(crate) friend_error: Option<String>,
    pub(crate) attachment_sending: bool,
    pub(crate) file_picker_open: bool,
    pub(crate) applied_theme: Option<Theme>,
    /// Soundboard picker and DJ (ui/voice_extras.rs).
    pub(crate) extras: ui::voice_extras::VoiceExtras,
}

impl VesktopApp {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        settings: Settings,
        handle: tokio::runtime::Handle,
        event_tx: UnboundedSender<UiEvent>,
        event_rx: UnboundedReceiver<UiEvent>,
    ) -> Self {
        let (tray_tx, tray_rx) = std::sync::mpsc::channel();
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        // Placeholder until start_backend wires the real one; its receiver
        // is dropped, so early sends are no-ops.
        let (voice_cmd_tx, _voice_cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        // Backend events also wake egui's loop, which only redraws on input.
        let event_tx = backend::EventTx::new(event_tx, _cc.egui_ctx.clone());

        let mut app = Self {
            conn: if settings.token.is_some() {
                ConnState::Connecting
            } else {
                ConnState::Disconnected("sem token".into(), None)
            },
            me: None,
            guilds: Vec::new(),
            dm_channels: Vec::new(),
            guild_channels: HashMap::new(),
            channel_index: HashMap::new(),
            messages: HashMap::new(),
            has_more: HashMap::new(),
            stale_channels: HashMap::new(),
            loading_channels: HashSet::new(),
            loading_guilds: HashSet::new(),
            unread: HashMap::new(),
            mentions: HashMap::new(),
            first_unread: HashMap::new(),
            last_seen: HashMap::new(),
            typing: HashMap::new(),
            presence: HashMap::new(),
            voice_states: HashMap::new(),
            voice: VoiceConn::Disconnected,
            voice_connected_at: None,
            desync_rejoins: Vec::new(),
            voice_users_pending: HashSet::new(),
            speaking: HashSet::new(),
            voice_muted: false,
            screen: None,
            screen_texture: None,
            stream: None,
            watch: None,
            watch_texture: None,
            fockytv: None,
            fockytv_live: HashMap::new(),
            fockytv_polled: None,
            voice_deaf: false,
            disconnected_at: None,
            selected_guild: None,
            selected_channel: None,
            user_cache: HashMap::new(),
            images: ImageCache::new(128),
            images_large: ImageCache::new(800),
            viewer: None,
            compose: String::new(),
            compose_error: None,
            last_compose: String::new(),
            last_typing_sent: None,
            jump_to_present: false,
            settings_open: false,
            settings_tab: Default::default(),
            login_token: String::new(),
            dm_search: String::new(),
            home_page: ui::friends::HomePage::DirectMessages,
            relationships: Vec::new(),
            relationships_loading: false,
            friend_input: String::new(),
            friend_error: None,
            attachment_sending: false,
            file_picker_open: false,
            // The first update() applies the theme through the live context.
            applied_theme: None,
            extras: Default::default(),
            settings,
            handle,
            event_tx,
            event_rx,
            cmd_tx,
            backend: None,
            qr_task: None,
            voice_task: None,
            voice_cmd_tx,
            qr: QrState::Idle,
            tray_tx,
            tray_rx,
        };

        #[cfg(feature = "tray")]
        if let Err(err) = crate::tray::install(app.tray_tx.clone()) {
            log::warn!("failed to install the system tray: {err}");
        }

        // Image loaders for the animated WebP splash.
        egui_extras::install_image_loaders(&_cc.egui_ctx);
        theme::install_fonts(&_cc.egui_ctx);

        if let Some(token) = app.settings.token.clone() {
            app.start_backend(token);
        }
        app
    }

    pub(crate) fn send(&self, command: Command) {
        let _ = self.cmd_tx.send(command);
    }

    pub(crate) fn refresh_relationships(&mut self) {
        if !self.relationships_loading {
            self.relationships_loading = true;
            self.friend_error = None;
            self.send(Command::LoadRelationships);
        }
    }

    pub(crate) fn request_friend(&mut self, username: String) {
        self.friend_error = None;
        self.send(Command::RequestFriend { username });
    }

    pub(crate) fn accept_friend(&mut self, user_id: String) {
        self.friend_error = None;
        self.send(Command::AcceptFriend { user_id });
    }

    pub(crate) fn remove_friend(&mut self, user_id: String) {
        self.friend_error = None;
        self.send(Command::RemoveRelationship { user_id });
    }

    pub(crate) fn open_dm(&mut self, user_id: String) {
        if user_id.parse::<u64>().is_err() || user_id == "0" {
            self.friend_error = Some("ID de usuário inválido".into());
            return;
        }
        self.friend_error = None;
        self.send(Command::OpenDm { user_id });
    }

    pub(crate) fn choose_attachment(&mut self) {
        if self.file_picker_open {
            return;
        }
        let Some(channel_id) = self.selected_channel.clone() else {
            return;
        };
        self.file_picker_open = true;
        let events = self.event_tx.clone();
        self.handle.spawn(async move {
            let (path, error) = match backend::file_picker::pick_file().await {
                Ok(path) => (path, None),
                Err(error) => (None, Some(error)),
            };
            events.send(UiEvent::AttachmentPickerClosed {
                channel_id,
                path,
                error,
            });
        });
    }

    pub(crate) fn send_attachment(&mut self, path: std::path::PathBuf) {
        if self.attachment_sending {
            return;
        }
        let (Some(channel_id), Some(token)) =
            (self.selected_channel.clone(), self.settings.token.clone())
        else {
            return;
        };
        self.attachment_sending = true;
        self.compose_error = None;
        let content = self.compose.clone();
        let events = self.event_tx.clone();
        self.handle.spawn(async move {
            match backend::api::Api::new(token)
                .send_attachment(&channel_id, &content, &path)
                .await
            {
                Ok(message) => events.send(UiEvent::AttachmentSent { message, content }),
                Err(error) => events.send(UiEvent::SendFailed {
                    channel_id,
                    error: error.to_string(),
                }),
            }
        });
    }

    pub(crate) fn start_backend(&mut self, token: String) {
        if let Some(task) = self.backend.take() {
            task.abort();
        }
        if let Some(task) = self.voice_task.take() {
            task.abort();
        }
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        self.cmd_tx = cmd_tx;
        // Voice: the gateway forwards op-4 payloads over `wire_tx`; the
        // driver task owns the actual connection (docs/VOICE.md).
        let (wire_tx, wire_rx) = tokio::sync::mpsc::unbounded_channel();
        let (voice_cmd_tx, voice_cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        self.voice_cmd_tx = voice_cmd_tx;
        self.voice_task = Some(self.handle.spawn(backend::voice::run(
            voice_cmd_rx,
            wire_rx,
            self.event_tx.clone(),
        )));
        self.backend = Some(self.handle.spawn(backend::gateway::run(
            token,
            cmd_rx,
            self.event_tx.clone(),
            wire_tx,
        )));
        self.disconnected_at = None;
        self.conn = ConnState::Connecting;
    }

    pub(crate) fn connect_from_login(&mut self) {
        let token = self.login_token.trim().to_string();
        if token.is_empty() {
            self.conn = ConnState::LoginError("Digite um token para entrar.".into());
            return;
        }
        self.login_token.clear();
        self.login_with(token);
    }

    fn login_with(&mut self, token: String) {
        self.stop_qr();
        self.settings.token = Some(token.clone());
        self.settings.save();
        self.start_backend(token);
    }

    pub(crate) fn start_qr(&mut self) {
        self.stop_qr();
        self.qr = QrState::Loading;
        self.qr_task = Some(
            self.handle
                .spawn(backend::remote_auth::run(self.event_tx.clone())),
        );
    }

    fn stop_qr(&mut self) {
        if let Some(task) = self.qr_task.take() {
            task.abort();
        }
        self.qr = QrState::Idle;
    }

    pub(crate) fn logout(&mut self) {
        self.finish_screen_share();
        if let Some(task) = self.backend.take() {
            task.abort();
        }
        if let Some(task) = self.voice_task.take() {
            task.abort();
        }
        self.settings.token = None;
        self.settings.selected_guild_id = None;
        self.settings.selected_channel_id = None;
        self.settings.save();
        self.me = None;
        self.guilds.clear();
        self.dm_channels.clear();
        self.guild_channels.clear();
        self.channel_index.clear();
        self.messages.clear();
        self.has_more.clear();
        self.stale_channels.clear();
        self.loading_channels.clear();
        self.loading_guilds.clear();
        self.unread.clear();
        self.mentions.clear();
        self.first_unread.clear();
        self.last_seen.clear();
        self.typing.clear();
        self.presence.clear();
        self.selected_guild = None;
        self.selected_channel = None;
        self.user_cache.clear();
        self.relationships.clear();
        self.relationships_loading = false;
        self.friend_input.clear();
        self.friend_error = None;
        self.attachment_sending = false;
        self.file_picker_open = false;
        self.home_page = ui::friends::HomePage::DirectMessages;
        self.compose.clear();
        self.compose_error = None;
        self.settings_open = false;
        self.login_token.clear();
        self.stop_qr();
        self.conn = ConnState::Disconnected("sessão encerrada".into(), None);
    }

    pub(crate) fn select_guild(&mut self, guild_id: &str) {
        self.selected_guild = Some(guild_id.to_string());
        self.selected_channel = None;
        self.settings.selected_guild_id = Some(guild_id.to_string());
        self.settings.selected_channel_id = None;
        if self.guild_channels.contains_key(guild_id) {
            self.restore_channel_for_guild(guild_id);
        } else {
            self.loading_guilds.insert(guild_id.to_string());
            self.send(Command::LoadGuildChannels {
                guild_id: guild_id.to_string(),
            });
        }
        self.send(Command::SubscribeGuild {
            guild_id: guild_id.to_string(),
        });
        self.settings.save();
    }

    pub(crate) fn select_home(&mut self) {
        self.selected_guild = None;
        self.settings.selected_guild_id = None;
        let current_is_dm = self
            .selected_channel
            .as_ref()
            .map(|id| matches!(self.channel_index.get(id), Some(ChannelRef::Dm)))
            .unwrap_or(false);
        if !current_is_dm {
            self.selected_channel = None;
            self.settings.selected_channel_id = None;
            if let Some(first_dm) = self.dm_channels.first().map(|channel| channel.id.clone()) {
                self.select_channel(first_dm);
            }
        }
        self.settings.save();
    }

    fn restore_channel_for_guild(&mut self, guild_id: &str) {
        let wanted = self.settings.last_channel_by_guild.get(guild_id).cloned();
        let chosen = match (wanted, self.guild_channels.get(guild_id)) {
            (Some(id), Some(list)) if list.iter().any(|channel| channel.id == id) => Some(id),
            _ => self
                .guild_channels
                .get(guild_id)
                .and_then(|list| list.iter().find(|channel| channel.is_selectable()))
                .map(|channel| channel.id.clone()),
        };
        if let Some(id) = chosen {
            self.select_channel(id);
        }
    }

    pub(crate) fn select_channel(&mut self, channel_id: String) {
        self.home_page = ui::friends::HomePage::DirectMessages;
        self.selected_channel = Some(channel_id.clone());
        self.unread.remove(&channel_id);
        self.mentions.remove(&channel_id);
        self.compose_error = None;
        if let Some(ChannelRef::Guild(guild_id)) = self.channel_index.get(&channel_id) {
            self.settings
                .last_channel_by_guild
                .insert(guild_id.clone(), channel_id.clone());
            self.settings.selected_guild_id = Some(guild_id.clone());
            self.selected_guild = Some(guild_id.clone());
        }
        self.settings.selected_channel_id = Some(channel_id.clone());
        self.settings.save();
        self.fetch_channel_if_needed(channel_id);
    }

    fn fetch_channel_if_needed(&mut self, channel_id: String) {
        if self.loading_channels.contains(&channel_id) {
            return;
        }
        // Cached history shows right away; only a channel that may have
        // missed messages during a reconnect asks for what's newer.
        let after = match self.stale_channels.get(&channel_id).cloned() {
            _ if !self.messages.contains_key(&channel_id) => None,
            Some(last) => Some(last),
            None => return,
        };
        self.loading_channels.insert(channel_id.clone());
        self.send(Command::LoadMessages {
            channel_id,
            before: None,
            after,
        });
    }

    pub(crate) fn send_current_message(&mut self) {
        let Some(channel_id) = self.selected_channel.clone() else {
            return;
        };
        let content = self.compose.trim().to_string();
        if content.is_empty() {
            return;
        }
        self.compose.clear();
        self.compose_error = None;
        self.send(Command::SendMessage {
            channel_id,
            content,
        });
    }

    pub(crate) fn load_older_messages(&mut self) {
        let Some(channel_id) = self.selected_channel.clone() else {
            return;
        };
        let Some(oldest) = self
            .messages
            .get(&channel_id)
            .and_then(|list| list.first().map(|message| message.id.clone()))
        else {
            return;
        };
        if self.loading_channels.contains(&channel_id) {
            return;
        }
        self.loading_channels.insert(channel_id.clone());
        self.send(Command::LoadMessages {
            channel_id,
            before: Some(oldest),
            after: None,
        });
    }

    /// A message that should light up the red badge: @everyone or the user.
    fn is_mention(&self, message: &Message) -> bool {
        message.mention_everyone
            || message
                .mentions
                .iter()
                .any(|user| Some(&user.id) == self.me.as_ref().map(|me| &me.id))
    }

    /// The timeline reached the bottom: everything up to `message_id` is read.
    pub(crate) fn mark_channel_read(&mut self, channel_id: &str, message_id: String) {
        self.last_seen.insert(channel_id.to_string(), message_id);
        self.first_unread.remove(channel_id);
        self.unread.remove(channel_id);
        self.mentions.remove(channel_id);
    }

    /// Whether anyone is typing in `channel_id`, pruning stale entries.
    pub(crate) fn typers(&mut self, channel_id: &str) -> Vec<String> {
        let cutoff = std::time::Instant::now() - Duration::from_secs(7);
        if let Some(map) = self.typing.get_mut(channel_id) {
            map.retain(|_, seen| *seen >= cutoff);
        }
        let mut names: Vec<String> = self
            .typing
            .get(channel_id)
            .into_iter()
            .flat_map(|map| map.keys())
            .filter_map(|id| {
                self.user_cache
                    .get(id)
                    .map(|user| user.display_name().to_string())
            })
            .collect();
        names.sort();
        names.truncate(3);
        names
    }

    /// The user typed in the compose box; tell Discord, at most every 8s.
    pub(crate) fn note_typing(&mut self) {
        if self.compose.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        if self
            .last_typing_sent
            .is_some_and(|sent| now.duration_since(sent) < Duration::from_secs(8))
        {
            return;
        }
        if let Some(channel_id) = self.selected_channel.clone() {
            self.last_typing_sent = Some(now);
            self.send(Command::SendTyping { channel_id });
        }
    }

    /// Splash "Tentar de novo": reconnect with the saved token.
    pub(crate) fn retry_connection(&mut self) {
        if let Some(token) = self.settings.token.clone() {
            self.start_backend(token);
        }
    }

    /// Click a voice channel: op 4 goes through the gateway socket and the
    /// driver task waits for Discord's state + server updates to connect.
    pub(crate) fn join_voice(&mut self, guild_id: String, channel_id: String) {
        match &self.voice {
            // A handshake is already in flight.
            VoiceConn::Connecting { .. } => return,
            VoiceConn::Connected {
                guild_id: g,
                channel_id: c,
            } if *g == guild_id && *c == channel_id => {
                return;
            }
            _ => {}
        }
        let Some(me) = &self.me else {
            return;
        };
        let user_id = me.id.clone();
        self.voice_connected_at = None;
        self.voice = VoiceConn::Connecting {
            guild_id: guild_id.clone(),
            channel_id: channel_id.clone(),
        };
        self.send(Command::JoinVoice {
            guild_id: guild_id.clone(),
            channel_id: channel_id.clone(),
            self_mute: self.voice_muted || self.voice_deaf,
            self_deaf: self.voice_deaf,
        });
        self.push_voice_audio_config();
        let _ = self.voice_cmd_tx.send(VoiceCommand::Join {
            guild_id,
            channel_id,
            user_id,
        });
    }

    /// Starts the portal picker + capture and goes live in the connected
    /// voice channel, or stops a running share.
    pub(crate) fn toggle_screen_share(&mut self, ctx: &egui::Context) {
        if self.screen.is_some() {
            self.stop_screen_share();
            return;
        }
        let VoiceConn::Connected {
            guild_id,
            channel_id,
        } = &self.voice
        else {
            return;
        };
        let Some(me) = &self.me else {
            return;
        };
        if self.settings.fockytv_share {
            let nick = self.fockytv_nick();
            let ctx = ctx.clone();
            let capture =
                crate::backend::capture::start(self.handle.clone(), move || ctx.request_repaint());
            play_sound(Sound::StreamStart);
            self.fockytv = Some(crate::backend::fockytv::publish(
                &self.settings.fockytv_url,
                &nick,
                self.settings.fockytv_fps,
                Arc::clone(&capture.frame),
            ));
            self.screen = Some(capture);
            return;
        }
        let stream_key = format!("guild:{guild_id}:{channel_id}:{}", me.id);
        self.send(Command::StartStream {
            guild_id: guild_id.clone(),
            channel_id: channel_id.clone(),
            stream_key: stream_key.clone(),
        });
        self.stream = Some(GoLive::new(stream_key));
        play_sound(Sound::StreamStart);
        let ctx = ctx.clone();
        self.screen = Some(crate::backend::capture::start(
            self.handle.clone(),
            move || ctx.request_repaint(),
        ));
    }

    /// The one way a share ends (button, picker cancelled, source gone,
    /// left voice, deleted by Discord): capture off, op 19 if still live.
    pub(crate) fn stop_screen_share(&mut self) {
        if self.screen.is_some() {
            play_sound(Sound::StreamStop);
        }
        self.screen = None;
        self.screen_texture = None;
        self.fockytv = None;
        if let Some(stream) = self.stream.take() {
            self.send(Command::StopStream {
                stream_key: stream.key.clone(),
            });
        }
    }

    /// Our FockyTV stream key: the configured nick, else the username.
    pub(crate) fn fockytv_nick(&self) -> String {
        self.settings
            .fockytv_nick
            .clone()
            .filter(|nick| !nick.trim().is_empty())
            .or_else(|| self.me.as_ref().map(|me| me.username.clone()))
            .unwrap_or_default()
    }

    /// Whether `user_id` is live, on Discord's Go Live or on FockyTV (by
    /// username, since the stream key is the nickname).
    pub(crate) fn is_live(&self, user_id: &str, discord_stream: bool) -> bool {
        if discord_stream {
            return true;
        }
        if !self.settings.fockytv_share {
            return false;
        }
        if self.me.as_ref().is_some_and(|me| me.id == user_id) {
            return self.fockytv.is_some();
        }
        self.user_cache.get(user_id).is_some_and(|user| {
            self.fockytv_live
                .contains_key(&user.username.to_lowercase())
        })
    }

    /// "Assistir": the member's FockyTV live (WHEP) or Discord's Go Live,
    /// both in the watch view in place of the chat.
    pub(crate) fn watch(&mut self, user_id: &str) {
        let fockytv_key = self
            .settings
            .fockytv_share
            .then(|| self.user_cache.get(user_id))
            .flatten()
            .and_then(|user| self.fockytv_live.get(&user.username.to_lowercase()))
            .cloned();
        let Some(key) = fockytv_key else {
            return self.watch_stream(user_id);
        };
        self.stop_watching();
        let mut watch = GoLive::new(format!("fockytv:{key}"));
        watch.fockytv = Some(crate::backend::fockytv::watch(
            &self.settings.fockytv_url,
            &key,
            &self.fockytv_nick(),
            Arc::clone(&watch.frames),
            self.event_tx.clone(),
        ));
        self.watch = Some(watch);
    }

    /// While in a call, refreshes who's live on FockyTV every 5 s.
    fn poll_fockytv(&mut self) {
        if !self.settings.fockytv_share || !matches!(self.voice, VoiceConn::Connected { .. }) {
            return;
        }
        if self
            .fockytv_polled
            .is_some_and(|at| at.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        self.fockytv_polled = Some(Instant::now());
        let (server, events) = (self.settings.fockytv_url.clone(), self.event_tx.clone());
        self.handle.spawn(async move {
            match crate::backend::fockytv::live_keys(&server).await {
                Ok(keys) => events.send(UiEvent::FockyLive { keys }),
                Err(err) => log::debug!("fockytv: status falhou: {err}"),
            }
        });
    }

    /// Ends any share before the session or process goes away (quit,
    /// logout), waiting for FockyTV's WHIP `DELETE` so the stream doesn't
    /// linger on the server.
    pub(crate) fn finish_screen_share(&mut self) {
        if let Some(publisher) = self.fockytv.take() {
            publisher.stop_and_wait();
        }
        self.stop_screen_share();
    }

    /// Starts watching `user_id`'s stream in the voice channel we're in.
    pub(crate) fn watch_stream(&mut self, user_id: &str) {
        let VoiceConn::Connected {
            guild_id,
            channel_id,
        } = &self.voice
        else {
            return;
        };
        let stream_key = format!("guild:{guild_id}:{channel_id}:{user_id}");
        if self
            .watch
            .as_ref()
            .is_some_and(|watch| watch.key == stream_key)
        {
            return;
        }
        self.stop_watching();
        self.send(Command::WatchStream {
            stream_key: stream_key.clone(),
        });
        self.watch = Some(GoLive::new(stream_key));
    }

    pub(crate) fn stop_watching(&mut self) {
        self.watch_texture = None;
        if let Some(watch) = self.watch.take().filter(|watch| watch.fockytv.is_none()) {
            self.send(Command::StopStream {
                stream_key: watch.key.clone(),
            });
        }
    }

    /// Our stream or the watched one, by key, and whether we're the viewer.
    fn go_live(&mut self, stream_key: &str) -> Option<(&mut GoLive, bool)> {
        if let Some(stream) = self
            .stream
            .as_mut()
            .filter(|stream| stream.key == stream_key)
        {
            return Some((stream, false));
        }
        self.watch
            .as_mut()
            .filter(|watch| watch.key == stream_key)
            .map(|watch| (watch, true))
    }

    /// Mute the microphone (op 4 flags; deafening implies it).
    pub(crate) fn set_voice_mute(&mut self, muted: bool) {
        if matches!(
            self.voice,
            VoiceConn::Disconnected | VoiceConn::Failed { .. }
        ) {
            return;
        }
        self.voice_muted = muted;
        self.send_voice_flags();
        let _ = self.voice_cmd_tx.send(VoiceCommand::SetMute { muted });
    }

    /// Deafen: stops playback locally and implies mute.
    pub(crate) fn set_voice_deaf(&mut self, deafened: bool) {
        if matches!(
            self.voice,
            VoiceConn::Disconnected | VoiceConn::Failed { .. }
        ) {
            return;
        }
        self.voice_deaf = deafened;
        self.send_voice_flags();
        let _ = self.voice_cmd_tx.send(VoiceCommand::SetDeaf { deafened });
    }

    /// op 4 for the current channel with the current flags.
    fn send_voice_flags(&mut self) {
        let Some((guild_id, channel_id)) = (match &self.voice {
            VoiceConn::Connecting {
                guild_id,
                channel_id,
            }
            | VoiceConn::Connected {
                guild_id,
                channel_id,
            } => Some((guild_id.clone(), channel_id.clone())),
            VoiceConn::Failed { .. } | VoiceConn::Disconnected => None,
        }) else {
            return;
        };
        self.send(Command::JoinVoice {
            guild_id,
            channel_id,
            self_mute: self.voice_muted || self.voice_deaf,
            self_deaf: self.voice_deaf,
        });
    }

    /// Voice member menu volume: persists and feeds the mixer.
    pub(crate) fn set_user_volume(&mut self, user_id: String, percent: u8) {
        self.settings.user_volumes.insert(user_id.clone(), percent);
        self.settings.save();
        self.push_user_volume(&user_id);
    }

    /// Local mute: silences one user for us only, kept across sessions.
    pub(crate) fn set_user_muted(&mut self, user_id: String, muted: bool) {
        if muted {
            self.settings.muted_users.insert(user_id.clone());
        } else {
            self.settings.muted_users.remove(&user_id);
        }
        self.settings.save();
        self.push_user_volume(&user_id);
    }

    /// The mixer's gain for one user: their volume, or zero when muted.
    fn push_user_volume(&self, user_id: &str) {
        let percent = self
            .settings
            .user_volumes
            .get(user_id)
            .copied()
            .unwrap_or(100);
        let muted = self.settings.muted_users.contains(user_id);
        let _ = self.voice_cmd_tx.send(VoiceCommand::SetUserVolume {
            user_id: user_id.to_string(),
            volume: if muted {
                0.0
            } else {
                f32::from(percent) / 100.0
            },
        });
    }

    /// The channel's member ids for the DAVE MLS group (the voice server
    /// never tells user accounts who else is in the call — op 11 missing).
    pub(crate) fn push_voice_roster(&mut self, channel_id: &str) {
        let users: Vec<u64> = self
            .voice_states
            .values()
            .flat_map(|roster| roster.values())
            .filter(|state| state.channel_id.as_deref() == Some(channel_id))
            .filter_map(|state| state.user_id.parse().ok())
            .collect();
        let _ = self.voice_cmd_tx.send(VoiceCommand::SetRoster(users));
    }

    /// Device names, sensitivity and noise suppression for the audio path.
    pub(crate) fn push_voice_audio_config(&mut self) {
        log::info!(
            "push config: in={:?} out={:?}",
            self.settings.input_device,
            self.settings.output_device
        );
        let _ = self
            .voice_cmd_tx
            .send(VoiceCommand::ApplyConfig(AudioConfig {
                input_device: self.settings.input_device.clone(),
                output_device: self.settings.output_device.clone(),
                input_volume: self.settings.input_volume,
                output_volume: self.settings.output_volume,
                sensitivity: self.settings.input_sensitivity,
                noise_suppression: self.settings.noise_suppression,
                bitrate_kbps: self.settings.opus_bitrate_kbps,
                auto_gain: self.settings.auto_gain,
                compressor: self.settings.compressor,
                echo_cancellation: self.settings.echo_cancellation,
            }));
        let users: HashSet<&String> = self
            .settings
            .user_volumes
            .keys()
            .chain(&self.settings.muted_users)
            .collect();
        for user_id in users {
            self.push_user_volume(user_id);
        }
    }

    /// The panel's disconnect button, or the server removed us.
    pub(crate) fn leave_voice(&mut self) {
        if matches!(self.voice, VoiceConn::Disconnected) {
            return;
        }
        let guild_id = match &self.voice {
            VoiceConn::Connecting { guild_id, .. } | VoiceConn::Connected { guild_id, .. } => {
                Some(guild_id.clone())
            }
            VoiceConn::Failed { .. } | VoiceConn::Disconnected => None,
        };
        if matches!(self.voice, VoiceConn::Connected { .. }) {
            play_sound(Sound::Disconnect);
        }
        // ponytail: a desync rejoin passes through here too and ends the DJ.
        self.extras.music.stop();
        self.stop_screen_share();
        self.stop_watching();
        self.voice = VoiceConn::Disconnected;
        self.voice_connected_at = None;
        if let Some(guild_id) = guild_id {
            self.send(Command::LeaveVoice { guild_id });
        }
        let _ = self.voice_cmd_tx.send(VoiceCommand::Leave);
    }

    /// Members of a voice channel of the selected guild (cloned; the
    /// sidebar paints them while still mutating the app).
    pub(crate) fn voice_members(&self, channel_id: &str) -> Vec<VoiceState> {
        let Some(guild_id) = self.selected_guild.as_deref() else {
            return Vec::new();
        };
        self.voice_states
            .get(guild_id)
            .map(|states| {
                let mut members: Vec<VoiceState> = states
                    .values()
                    .filter(|state| state.channel_id.as_deref() == Some(channel_id))
                    .cloned()
                    .collect();
                members.sort_by(|a, b| a.user_id.cmp(&b.user_id));
                members
            })
            .unwrap_or_default()
    }

    /// Display name for a voice member, fetching the member record once.
    /// Empty while the fetch is in flight; the UI shows a placeholder.
    pub(crate) fn voice_member_name(&mut self, guild_id: &str, user_id: &str) -> String {
        if let Some(user) = self.user_cache.get(user_id) {
            return user.display_name().to_string();
        }
        if self.voice_users_pending.insert(user_id.to_string()) {
            self.send(Command::LoadVoiceUser {
                guild_id: guild_id.to_string(),
                user_id: user_id.to_string(),
            });
        }
        String::new()
    }

    /// Alt+↑/↓: move through the current context's channel list.
    pub(crate) fn switch_channel(&mut self, delta: i32) {
        let ids: Vec<String> = match &self.selected_guild {
            Some(guild_id) => self
                .guild_channels
                .get(guild_id)
                .map(|channels| {
                    let mut list: Vec<&Channel> = channels
                        .iter()
                        .filter(|channel| channel.is_selectable())
                        .collect();
                    list.sort_by_key(|channel| {
                        (channel.position, channel.name.clone().unwrap_or_default())
                    });
                    list.into_iter().map(|channel| channel.id.clone()).collect()
                })
                .unwrap_or_default(),
            None => self
                .dm_channels
                .iter()
                .map(|channel| channel.id.clone())
                .collect(),
        };
        if ids.is_empty() {
            return;
        }
        let next = match self
            .selected_channel
            .as_ref()
            .and_then(|id| ids.iter().position(|candidate| candidate == id))
        {
            Some(index) => (index as i64 + delta as i64).rem_euclid(ids.len() as i64) as usize,
            None => 0,
        };
        self.select_channel(ids[next].clone());
    }

    pub(crate) fn channel_name(&self, channel_id: &str) -> String {
        for channels in self.guild_channels.values() {
            if let Some(channel) = channels.iter().find(|channel| channel.id == channel_id) {
                return channel.display_name();
            }
        }
        if let Some(channel) = self
            .dm_channels
            .iter()
            .find(|channel| channel.id == channel_id)
        {
            return channel.display_name();
        }
        String::new()
    }

    pub(crate) fn channel_topic(&self, channel_id: &str) -> Option<String> {
        for channels in self.guild_channels.values() {
            if let Some(channel) = channels.iter().find(|channel| channel.id == channel_id) {
                return channel.topic.clone().filter(|topic| !topic.is_empty());
            }
        }
        self.dm_channels
            .iter()
            .find(|channel| channel.id == channel_id)
            .and_then(|channel| channel.topic.clone())
            .filter(|topic| !topic.is_empty())
    }

    /// All known channels as `id → display name`, for mention resolution.
    pub(crate) fn channel_names(&self) -> HashMap<String, String> {
        let mut names = HashMap::new();
        for channels in self.guild_channels.values() {
            for channel in channels {
                names.insert(channel.id.clone(), channel.display_name());
            }
        }
        for channel in &self.dm_channels {
            names.insert(channel.id.clone(), channel.display_name());
        }
        names
    }

    fn poll_events(&mut self, ctx: &Context) {
        while let Ok(event) = self.event_rx.try_recv() {
            if let UiEvent::AttachmentPickerClosed {
                channel_id,
                path: Some(path),
                ..
            } = &event
            {
                ui::chat::attachment_selected(ctx, channel_id, path);
            }
            self.handle_event(event, ctx.input(|input| input.focused));
            ctx.request_repaint();
        }
    }

    fn handle_event(&mut self, event: UiEvent, focused: bool) {
        match event {
            UiEvent::Connected => {
                self.disconnected_at = None;
                self.conn = ConnState::Connected;
            }
            UiEvent::Ready { user } => {
                // A fresh session after a drop: cached channels may have
                // missed messages in between.
                for (channel_id, list) in &self.messages {
                    if let Some(last) = list.last() {
                        self.stale_channels
                            .entry(channel_id.clone())
                            .or_insert_with(|| last.id.clone());
                    }
                }
                self.user_cache.insert(user.id.clone(), user.clone());
                self.me = Some(user);
                self.disconnected_at = None;
                self.conn = ConnState::Connected;
                self.send(Command::LoadGuilds);
                self.send(Command::LoadDmChannels);
                self.refresh_relationships();
                if let Some(channel_id) = self.selected_channel.clone() {
                    self.fetch_channel_if_needed(channel_id);
                }
            }
            UiEvent::Disconnected { reason, retry_in } => {
                self.disconnected_at = Some(std::time::Instant::now());
                self.conn = ConnState::Disconnected(reason, retry_in)
            }
            UiEvent::TokenInvalid => {
                self.logout();
                self.conn = ConnState::LoginError(
                    "Token inválido ou expirado. Cole um novo token para entrar.".into(),
                );
            }
            UiEvent::GuildsLoaded { guilds } => {
                let restored = self
                    .settings
                    .selected_guild_id
                    .clone()
                    .filter(|id| guilds.iter().any(|guild| &guild.id == id));
                self.guilds = guilds;
                if let Some(id) = restored {
                    self.select_guild(&id);
                }
            }
            UiEvent::RelationshipsLoaded { relationships } => {
                for relation in &relationships {
                    self.user_cache
                        .insert(relation.user.id.clone(), relation.user.clone());
                }
                self.relationships = relationships;
                self.relationships_loading = false;
            }
            UiEvent::RelationshipsChanged => {
                self.relationships_loading = false;
                self.refresh_relationships();
            }
            UiEvent::RelationshipError { error } => {
                self.relationships_loading = false;
                self.friend_error = Some(error);
            }
            UiEvent::DmOpened { channel } => {
                self.channel_index
                    .insert(channel.id.clone(), ChannelRef::Dm);
                let id = channel.id.clone();
                if let Some(existing) = self.dm_channels.iter_mut().find(|dm| dm.id == id) {
                    *existing = channel;
                } else {
                    self.dm_channels.insert(0, channel);
                }
                self.selected_guild = None;
                self.select_channel(id);
            }
            UiEvent::AttachmentPickerClosed {
                channel_id, error, ..
            } => {
                self.file_picker_open = false;
                if self.selected_channel.as_deref() == Some(channel_id.as_str()) {
                    if let Some(error) = error {
                        self.compose_error = Some(error);
                    }
                }
            }
            UiEvent::AttachmentSent { message, content } => {
                self.attachment_sending = false;
                if self.selected_channel.as_deref() == Some(message.channel_id.as_str())
                    && self.compose == content
                {
                    self.compose.clear();
                }
                self.handle_event(UiEvent::MessageCreated { message }, focused);
            }
            UiEvent::DmChannelsLoaded { channels } => {
                for channel in &channels {
                    self.channel_index
                        .insert(channel.id.clone(), ChannelRef::Dm);
                }
                let restored = self
                    .settings
                    .selected_channel_id
                    .clone()
                    .filter(|id| channels.iter().any(|channel| &channel.id == id));
                // Discord orders the DM list by most recent conversation;
                // the REST endpoint ignores the user's order.
                let mut channels = channels;
                channels.sort_by_key(|channel| {
                    std::cmp::Reverse(
                        channel
                            .last_message_id
                            .as_deref()
                            .and_then(|id| id.parse::<u64>().ok())
                            .unwrap_or(0),
                    )
                });
                self.dm_channels = channels;
                if self.selected_guild.is_none()
                    && self.selected_channel.is_none()
                    && self.home_page == ui::friends::HomePage::DirectMessages
                {
                    if let Some(id) = restored {
                        self.select_channel(id);
                    }
                }
            }
            UiEvent::GuildChannelsLoaded { guild_id, channels } => {
                for channel in &channels {
                    self.channel_index
                        .insert(channel.id.clone(), ChannelRef::Guild(guild_id.clone()));
                }
                self.guild_channels.insert(guild_id.clone(), channels);
                self.loading_guilds.remove(&guild_id);
                if self.selected_guild.as_deref() == Some(guild_id.as_str())
                    && self.selected_channel.is_none()
                {
                    self.restore_channel_for_guild(&guild_id);
                }
            }
            UiEvent::MessagesLoaded {
                channel_id,
                messages,
                older,
                newer,
            } => {
                self.loading_channels.remove(&channel_id);
                let full_page = messages.len() as u64 >= backend::api::MESSAGES_PER_PAGE;
                let next_after = if newer && full_page {
                    messages.last().map(|m| m.id.clone()).filter(|id| {
                        self.stale_channels
                            .get(&channel_id)
                            .is_none_or(|cursor| snowflake_cmp(id, cursor).is_gt())
                    })
                } else {
                    None
                };
                if !newer {
                    self.has_more.insert(channel_id.clone(), full_page);
                }
                let entry = self.messages.entry(channel_id.clone()).or_default();
                if older {
                    let mut merged = messages;
                    merged.extend(entry.drain(..));
                    *entry = merged;
                } else {
                    // Catch-up pages and gateway echoes racing the REST
                    // response overlap the cache: merge and dedupe.
                    merge_messages(entry, messages);
                }
                if let Some(after) = next_after {
                    self.stale_channels
                        .insert(channel_id.clone(), after.clone());
                    self.loading_channels.insert(channel_id.clone());
                    self.send(Command::LoadMessages {
                        channel_id,
                        before: None,
                        after: Some(after),
                    });
                } else if newer && !full_page {
                    self.stale_channels.remove(&channel_id);
                }
            }
            UiEvent::MessagesFailed { channel_id } => {
                self.loading_channels.remove(&channel_id);
            }
            UiEvent::MessageCreated { message } => {
                let selected =
                    self.selected_channel.as_deref() == Some(message.channel_id.as_str());
                let duplicate = self
                    .messages
                    .get(&message.channel_id)
                    .is_some_and(|list| list.iter().any(|m| m.id == message.id));
                let own = self
                    .me
                    .as_ref()
                    .is_some_and(|me| me.id == message.author.id);
                let dm = self.dm_channels.iter().any(|c| c.id == message.channel_id);
                let dnd = self
                    .me
                    .as_ref()
                    .and_then(|me| self.presence.get(&me.id))
                    .is_some_and(|status| status == "dnd");
                if should_notify_message(
                    selected,
                    focused,
                    duplicate,
                    own,
                    dnd,
                    dm || self.is_mention(&message),
                ) {
                    crate::notifications::notify(message.author.display_name(), &message.content);
                }
                self.user_cache
                    .insert(message.author.id.clone(), message.author.clone());
                if let Some(list) = self.messages.get_mut(&message.channel_id) {
                    if !list.iter().any(|m| m.id == message.id) {
                        // The red divider appears when the message lands while
                        // the timeline is behind the present moment.
                        let was_at_present =
                            self.last_seen.get(&message.channel_id) == list.last().map(|m| &m.id);
                        list.push(message.clone());
                        const MAX_MESSAGES: usize = 400;
                        if list.len() > MAX_MESSAGES {
                            let extra = list.len() - MAX_MESSAGES;
                            list.drain(..extra);
                            self.has_more.insert(message.channel_id.clone(), true);
                        }
                        if selected && !was_at_present && list.len() > 1 {
                            self.first_unread
                                .entry(message.channel_id.clone())
                                .or_insert_with(|| message.id.clone());
                        }
                    }
                }
                if !selected && self.channel_index.contains_key(&message.channel_id) {
                    *self.unread.entry(message.channel_id.clone()).or_insert(0) += 1;
                    if self.is_mention(&message) {
                        *self.mentions.entry(message.channel_id).or_insert(0) += 1;
                    }
                }
            }
            UiEvent::MessageUpdated {
                channel_id,
                message_id,
                content,
            } => {
                if let Some(list) = self.messages.get_mut(&channel_id) {
                    if let Some(message) = list.iter_mut().find(|m| m.id == message_id) {
                        message.content = content;
                    }
                }
            }
            UiEvent::MessageDeleted {
                channel_id,
                message_id,
            } => {
                if let Some(list) = self.messages.get_mut(&channel_id) {
                    list.retain(|message| message.id != message_id);
                }
            }
            UiEvent::TypingStart {
                channel_id,
                user_id,
            } => {
                if Some(&user_id) == self.me.as_ref().map(|me| &me.id) {
                    return;
                }
                self.user_cache
                    .entry(user_id.clone())
                    .or_insert_with(|| User {
                        id: user_id.clone(),
                        username: user_id.clone(),
                        global_name: None,
                        discriminator: None,
                        avatar: None,
                    });
                self.typing
                    .entry(channel_id)
                    .or_default()
                    .insert(user_id, std::time::Instant::now());
            }
            UiEvent::PresenceUpdate { user_id, status } => {
                self.presence.insert(user_id, status);
            }
            UiEvent::SendFailed { channel_id, error } => {
                self.attachment_sending = false;
                if self.selected_channel.as_deref() == Some(channel_id.as_str()) {
                    self.compose_error = Some(error);
                } else {
                    log::warn!("failed to send in {channel_id}: {error}");
                }
            }
            UiEvent::Error { context } => log::warn!("{context}"),
            UiEvent::GuildVoiceStates { guild_id, states } => {
                let mut roster: HashMap<String, VoiceState> = HashMap::new();
                for mut state in states {
                    if state.guild_id.is_none() {
                        state.guild_id = Some(guild_id.clone());
                    }
                    roster.insert(state.user_id.clone(), state);
                }
                // The server kept us in a channel across a gateway
                // reconnect: redo the handshake so the driver comes back.
                if let Some(me) = self.me.clone()
                    && let Some(channel_id) = roster
                        .get(&me.id)
                        .and_then(|state| state.channel_id.clone())
                    && matches!(self.voice, VoiceConn::Disconnected)
                {
                    self.join_voice(guild_id.clone(), channel_id);
                }
                self.voice_states.insert(guild_id.clone(), roster);
                if let VoiceConn::Connecting {
                    guild_id: active_guild,
                    channel_id,
                }
                | VoiceConn::Connected {
                    guild_id: active_guild,
                    channel_id,
                } = &self.voice
                    && *active_guild == guild_id
                {
                    self.push_voice_roster(&channel_id.clone());
                }
            }
            UiEvent::VoiceStateUpdate { state } => {
                let Some(guild_id) = state.guild_id.clone() else {
                    return;
                };
                let is_me = self.me.as_ref().map(|me| &me.id) == Some(&state.user_id);
                // Dragged into a channel from another client: follow along.
                if is_me
                    && matches!(self.voice, VoiceConn::Disconnected)
                    && let Some(channel_id) = state.channel_id.clone()
                {
                    self.join_voice(guild_id.clone(), channel_id);
                }
                let user_id = state.user_id.clone();
                if let VoiceConn::Connected { channel_id, .. } = &self.voice
                    && !is_me
                    && let Some(sound) = ui::voice_extras::roster_sound(
                        channel_id,
                        self.voice_states
                            .get(&guild_id)
                            .and_then(|roster| roster.get(&user_id)),
                        &state,
                    )
                {
                    play_sound(sound);
                }
                self.voice_states
                    .entry(guild_id.clone())
                    .or_default()
                    .insert(user_id, state);
                // Always seed the active call, including departures and moves to another room.
                if let VoiceConn::Connecting {
                    guild_id: active_guild,
                    channel_id,
                }
                | VoiceConn::Connected {
                    guild_id: active_guild,
                    channel_id,
                } = &self.voice
                    && *active_guild == guild_id
                {
                    self.push_voice_roster(&channel_id.clone());
                }
            }
            UiEvent::SoundboardLoaded { guild_id, sounds } => {
                self.extras.soundboard_loading.remove(&guild_id);
                self.extras.soundboard_errors.remove(&guild_id);
                self.extras.sounds.insert(guild_id, sounds);
            }
            UiEvent::SoundboardError { guild_id, error } => {
                if let Some(guild_id) = guild_id {
                    self.extras.soundboard_loading.remove(&guild_id);
                    self.extras.soundboard_errors.insert(guild_id, error);
                } else {
                    self.extras.soundboard_error = Some(error);
                }
            }
            UiEvent::VoiceEffect { sound_id, volume } => {
                if !self.voice_deaf {
                    self.handle
                        .spawn(crate::backend::soundboard::play(sound_id, volume));
                }
            }
            UiEvent::UserResolved { user } => {
                self.voice_users_pending.remove(&user.id);
                self.user_cache.insert(user.id.clone(), user);
            }
            UiEvent::VoiceConnected {
                guild_id,
                channel_id,
            } => {
                if matches!(self.voice, VoiceConn::Connecting { .. }) {
                    play_sound(Sound::Join);
                }
                self.voice_connected_at.get_or_insert_with(Instant::now);
                self.voice = VoiceConn::Connected {
                    guild_id,
                    channel_id: channel_id.clone(),
                };
                self.push_voice_audio_config();
                self.push_voice_roster(&channel_id);
            }
            UiEvent::VoiceFailed { error } => {
                log::warn!("voz falhou: {error}");
                self.voice = VoiceConn::Failed { error };
                self.voice_connected_at = None;
            }
            UiEvent::VoiceSpeaking { user_id, speaking } => {
                if speaking {
                    self.speaking.insert(user_id);
                } else {
                    self.speaking.remove(&user_id);
                }
            }
            UiEvent::VoiceSuspectDesync => {
                // Out-of-sync voice session (DAVE re-key, far-side
                // reconnect): a fresh handshake restores SSRC maps and
                // decryption keys. songbird 0.6 rolls dice per join here
                // (upstream #310), so re-roll — at most 3 times per 2
                // minutes.
                self.desync_rejoins
                    .retain(|t| Instant::now().duration_since(*t) < Duration::from_secs(120));
                if self.desync_rejoins.len() >= 3 {
                    return;
                }
                if let VoiceConn::Connected {
                    guild_id,
                    channel_id,
                } = self.voice.clone()
                {
                    log::warn!("voz fora de sincronia; refazendo a conexão");
                    self.desync_rejoins.push(Instant::now());
                    self.leave_voice();
                    self.join_voice(guild_id, channel_id);
                }
            }
            UiEvent::FockyLive { keys } => {
                self.fockytv_live = keys
                    .into_iter()
                    .map(|key| (key.to_lowercase(), key))
                    .collect();
            }
            UiEvent::StreamCreated {
                stream_key,
                rtc_server_id,
                rtc_channel_id,
            } => {
                if let Some((go_live, _)) = self.go_live(&stream_key) {
                    go_live.rtc = rtc_channel_id
                        .parse()
                        .ok()
                        .map(|channel| (rtc_server_id, channel));
                }
            }
            UiEvent::StreamServer {
                stream_key,
                endpoint,
                token,
                session_id,
            } => {
                let me = self.me.as_ref().and_then(|me| me.id.parse().ok());
                let handle = self.handle.clone();
                let events = self.event_tx.clone();
                let capture = self
                    .screen
                    .as_ref()
                    .map(|capture| Arc::clone(&capture.frame));
                let Some((go_live, viewer)) = self.go_live(&stream_key) else {
                    return;
                };
                // A new server (or none) replaces the old connection.
                if let Some(task) = go_live.task.take() {
                    task.abort();
                }
                let role = if viewer {
                    // The key ends with the streamer's user id.
                    let streamer = stream_key.rsplit(':').next().and_then(|id| id.parse().ok());
                    let Some(streamer) = streamer else {
                        return;
                    };
                    crate::backend::stream::Role::Watch {
                        streamer,
                        frames: Arc::clone(&go_live.frames),
                        events,
                    }
                } else {
                    crate::backend::stream::Role::Stream {
                        frames: capture.unwrap_or_default(),
                    }
                };
                if let (Some(endpoint), Some((rtc_server_id, rtc_channel_id)), Some(user_id)) =
                    (endpoint, go_live.rtc.clone(), me)
                {
                    let info = crate::backend::stream::StreamInfo {
                        endpoint,
                        token,
                        rtc_server_id,
                        rtc_channel_id,
                        session_id,
                        user_id,
                    };
                    go_live.task = Some(handle.spawn(crate::backend::stream::run(info, role)));
                }
            }
            UiEvent::StreamDeleted { stream_key, reason } => {
                // `user_requested` echoes our own op 19, already handled —
                // and since the key repeats per channel, a late echo would
                // otherwise kill the share that was just restarted.
                if reason == "user_requested" {
                    return;
                }
                log::info!("transmissão {stream_key} encerrada pelo Discord: {reason}");
                // Already gone server-side: no op 19.
                if self
                    .stream
                    .as_ref()
                    .is_some_and(|stream| stream.key == stream_key)
                {
                    self.stream = None;
                    self.stop_screen_share();
                }
                if self
                    .watch
                    .as_ref()
                    .is_some_and(|watch| watch.key == stream_key)
                {
                    self.watch = None;
                    self.watch_texture = None;
                }
            }
            UiEvent::VoiceLeft => {
                if matches!(self.voice, VoiceConn::Connected { .. }) {
                    play_sound(Sound::Disconnect);
                }
                self.extras.music.stop();
                self.stop_screen_share();
                self.stop_watching();
                self.voice = VoiceConn::Disconnected;
                self.voice_connected_at = None;
                self.speaking.clear();
            }
            UiEvent::QrReady { url } => self.qr = QrState::Code(url),
            UiEvent::QrScanned {
                username,
                user_id,
                avatar,
            } => {
                self.user_cache
                    .entry(user_id.clone())
                    .or_insert_with(|| User {
                        id: user_id.clone(),
                        username: username.clone(),
                        global_name: None,
                        discriminator: None,
                        avatar: avatar.clone(),
                    });
                self.qr = QrState::Scanned {
                    username,
                    user_id,
                    avatar,
                };
            }
            UiEvent::QrLogin { token } => self.login_with(token),
            UiEvent::QrExpired => {
                self.qr_task = None;
                self.qr = QrState::Idle;
                self.start_qr();
            }
            UiEvent::QrFailed { reason } => {
                self.qr_task = None;
                self.qr = QrState::Failed(reason);
            }
        }
    }
}

impl eframe::App for VesktopApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.finish_screen_share();
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        if self.applied_theme != Some(self.settings.theme) {
            theme::apply(ctx, self.settings.theme);
            self.applied_theme = Some(self.settings.theme);
        }
        let target_scale =
            ctx.native_pixels_per_point().unwrap_or(1.0) * self.settings.zoom.max(0.25);
        if (ctx.pixels_per_point() - target_scale).abs() > 0.01 {
            ctx.set_pixels_per_point(target_scale);
        }

        while let Ok(command) = self.tray_rx.try_recv() {
            if matches!(command, TrayCommand::Quit) {
                self.settings.save();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }

        self.poll_events(ctx);

        ui::login::compact_window(ctx, self.me.is_none());
        if self.me.is_none() {
            if self.settings.token.is_some() {
                // Saved token but no READY yet: Vesktop's own splash, which
                // covers Electron's slow startup inside the main window.
                ui::splash::show(self, root);
                ctx.request_repaint_after(Duration::from_millis(200));
            } else {
                ui::login::show(self, root);
                // Backend and QR events only land on the next frame; keep
                // polling.
                ctx.request_repaint_after(Duration::from_millis(400));
            }
            return;
        }

        // Shortcuts (docs/UI.md §5): Esc closes settings, Alt+↑/↓ moves
        // through the channel list. Ctrl+K quick switcher comes last.
        if self.settings_open && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.settings_open = false;
        }
        if ctx.input(|input| input.modifiers.alt) {
            if ctx.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                self.switch_channel(1);
            } else if ctx.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                self.switch_channel(-1);
            }
        }

        egui::Panel::left("server_rail")
            .exact_size(72.0)
            .resizable(false)
            .show(root, |ui| {
                ui::server_rail::paint(self, ui);
            });

        egui::Panel::left("channel_sidebar")
            .exact_size(240.0)
            .resizable(false)
            .show(root, |ui| {
                ui::channel_sidebar::paint(self, ui);
            });

        egui::CentralPanel::default().show(root, |ui| {
            if self.selected_guild.is_none()
                && self.home_page != ui::friends::HomePage::DirectMessages
            {
                ui::friends::paint(self, ui);
            } else if self.watch.is_some() {
                ui::screen_share::paint_watch(self, ui);
            } else {
                ui::chat::paint(self, ui);
            }
        });

        if self.settings_open {
            ui::settings_window::show(self, ctx);
        }
        ui::image_viewer::show(self, ctx);
        ui::voice_extras::show(self, ctx);
        ui::screen_share::poll(self, ctx);
        self.poll_fockytv();
        if self.settings.fockytv_share && matches!(self.voice, VoiceConn::Connected { .. }) {
            ctx.request_repaint_after(Duration::from_secs(5));
        }
        if matches!(self.conn, ConnState::Connecting) {
            ctx.request_repaint_after(Duration::from_millis(400));
        }
    }
}

fn should_notify_message(
    selected: bool,
    focused: bool,
    duplicate: bool,
    own: bool,
    dnd: bool,
    relevant: bool,
) -> bool {
    relevant && !duplicate && !own && !dnd && (!selected || !focused)
}

/// Merges a fetched page into a channel's cached history: chronological by
/// snowflake (numeric, so a shorter id sorts first) without duplicates,
/// keeping the fetched copy of a message over the cached one.
fn snowflake_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    (a.len(), a).cmp(&(b.len(), b))
}

fn merge_messages(cache: &mut Vec<Message>, page: Vec<Message>) {
    let mut merged = page;
    merged.append(cache);
    merged.sort_by(|a, b| snowflake_cmp(&a.id, &b.id));
    merged.dedup_by(|a, b| a.id == b.id);
    *cache = merged;
}

#[cfg(test)]
mod tests {
    fn test_app() -> (
        tokio::runtime::Runtime,
        VesktopApp,
        UnboundedReceiver<Command>,
        UnboundedReceiver<VoiceCommand>,
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let cc = eframe::CreationContext::_new_kittest(Context::default());
        let (events, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = VesktopApp::new(&cc, Settings::default(), rt.handle().clone(), events, rx);
        let (tx, commands) = tokio::sync::mpsc::unbounded_channel();
        let (voice_tx, voice_commands) = tokio::sync::mpsc::unbounded_channel();
        app.cmd_tx = tx;
        app.voice_cmd_tx = voice_tx;
        (rt, app, commands, voice_commands)
    }

    fn sample_message(id: u64) -> Message {
        serde_json::from_value(serde_json::json!({ "id": id.to_string(), "channel_id": "channel", "author": { "id": "9", "username": "member" } })).unwrap()
    }

    #[test]
    fn catchup_preserves_history_paginates_and_retries_after_failure() {
        let (_rt, mut app, mut commands, _) = test_app();
        app.messages
            .insert("channel".into(), (1..=5).map(sample_message).collect());
        app.stale_channels.insert("channel".into(), "5".into());
        app.fetch_channel_if_needed("channel".into());
        assert!(
            matches!(commands.try_recv().unwrap(), Command::LoadMessages { after: Some(id), .. } if id == "5")
        );
        app.handle_event(
            UiEvent::MessagesFailed {
                channel_id: "channel".into(),
            },
            true,
        );
        app.fetch_channel_if_needed("channel".into());
        assert!(
            matches!(commands.try_recv().unwrap(), Command::LoadMessages { after: Some(id), .. } if id == "5")
        );
        for (first, last) in [(6, 55), (56, 105)] {
            app.handle_event(
                UiEvent::MessagesLoaded {
                    channel_id: "channel".into(),
                    messages: (first..=last).map(sample_message).collect(),
                    older: false,
                    newer: true,
                },
                true,
            );
            assert!(
                matches!(commands.try_recv().unwrap(), Command::LoadMessages { after: Some(id), .. } if id == last.to_string())
            );
        }
        app.handle_event(
            UiEvent::MessagesLoaded {
                channel_id: "channel".into(),
                messages: vec![sample_message(106)],
                older: false,
                newer: true,
            },
            true,
        );
        let history = &app.messages["channel"];
        assert_eq!(history.len(), 106);
        assert_eq!(history.first().unwrap().id, "1");
        assert_eq!(history.last().unwrap().id, "106");
        assert!(!app.stale_channels.contains_key("channel"));
        app.fetch_channel_if_needed("channel".into());
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn voice_changes_keep_roster_on_active_call_including_departures() {
        let (_rt, mut app, _, mut voice_commands) = test_app();
        app.voice = VoiceConn::Connected {
            guild_id: "guild".into(),
            channel_id: "current".into(),
        };
        let voice = |id: &str, channel: Option<&str>| {
            serde_json::from_value::<VoiceState>(serde_json::json!({ "user_id": id, "guild_id": "guild", "channel_id": channel, "session_id": "session" })).unwrap()
        };
        for state in [
            voice("10", Some("current")),
            voice("20", Some("current")),
            voice("30", Some("other")),
            voice("20", None),
            voice("10", Some("other")),
        ] {
            app.handle_event(UiEvent::VoiceStateUpdate { state }, true);
            let VoiceCommand::SetRoster(mut actual) = voice_commands.try_recv().unwrap() else {
                panic!("expected roster");
            };
            actual.sort();
            let mut expected: Vec<_> = app.voice_states["guild"]
                .values()
                .filter(|state| state.channel_id.as_deref() == Some("current"))
                .map(|state| state.user_id.parse::<u64>().unwrap())
                .collect();
            expected.sort();
            assert_eq!(actual, expected);
        }
        assert!(
            app.voice_states["guild"]
                .values()
                .all(|state| state.channel_id.as_deref() != Some("current"))
        );
    }

    #[test]
    fn dm_load_does_not_replace_friends_page() {
        let (_rt, mut app, mut commands, _) = test_app();
        app.home_page = ui::friends::HomePage::Friends;
        app.settings.selected_channel_id = Some("channel".into());
        let channel =
            serde_json::from_value(serde_json::json!({ "id": "channel", "type": 1 })).unwrap();
        app.handle_event(
            UiEvent::DmChannelsLoaded {
                channels: vec![channel],
            },
            true,
        );
        assert!(app.home_page == ui::friends::HomePage::Friends);
        assert!(app.selected_channel.is_none());
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn message_alerts_respect_focus_duplicates_and_dnd() {
        assert!(should_notify_message(
            false, true, false, false, false, true
        ));
        assert!(should_notify_message(
            true, false, false, false, false, true
        ));
        assert!(!should_notify_message(
            true, true, false, false, false, true
        ));
        assert!(!should_notify_message(
            false, false, true, false, false, true
        ));
        assert!(!should_notify_message(
            false, false, false, true, false, true
        ));
        assert!(!should_notify_message(
            false, false, false, false, true, true
        ));
        assert!(!should_notify_message(
            false, false, false, false, false, false
        ));
    }

    use super::*;

    #[test]
    fn merge_keeps_order_and_drops_duplicates() {
        let message = |id: &str| -> Message {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "author": { "id": "1", "username": "fulano" }
            }))
            .unwrap()
        };
        let mut cache = vec![
            message("999999999999999999"),
            message("1000000000000000001"),
        ];
        merge_messages(
            &mut cache,
            vec![
                message("1000000000000000002"),
                message("1000000000000000001"),
            ],
        );
        let ids: Vec<&str> = cache.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "999999999999999999",
                "1000000000000000001",
                "1000000000000000002"
            ]
        );
    }
}
