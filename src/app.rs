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
use crate::util;

const STARTUP_CACHE_KEY: &str = "startup_cache_v1";

#[derive(serde::Serialize, serde::Deserialize)]
struct StartupCache {
    session: String,
    me: User,
    guilds: Vec<Guild>,
    dm_channels: Vec<Channel>,
    guild_channels: HashMap<String, Vec<Channel>>,
    relationships: Vec<Relationship>,
}

fn session_key(token: &str) -> String {
    use rsa::sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn load_startup_cache(storage: &dyn eframe::Storage, token: &str) -> Option<StartupCache> {
    let cache: StartupCache = eframe::get_value(storage, STARTUP_CACHE_KEY)?;
    (cache.session == session_key(token)).then_some(cache)
}

/// Actions sent by the native tray menu.
pub enum TrayCommand {
    Show,
    Quit,
    /// One of `settings::STATUSES`.
    Status(&'static str),
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
    pub(crate) fockytv: Option<crate::plugins::fockytv::Viewer>,
    /// When watching began: a Go Live that never shows video gets a
    /// notice (blocked in Brazil).
    pub(crate) started: Instant,
}

impl GoLive {
    fn new(key: String) -> Self {
        Self {
            key,
            rtc: None,
            task: None,
            frames: Arc::default(),
            fockytv: None,
            started: Instant::now(),
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
    pub(crate) event_tx: backend::EventTx,
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
    #[cfg(feature = "tray")]
    tray: Option<crate::tray::Tray>,
    quitting: bool,
    can_hide_window: bool,

    pub(crate) conn: ConnState,
    pub(crate) me: Option<User>,
    pub(crate) guilds: Vec<Guild>,
    pub(crate) dm_channels: Vec<Channel>,
    pub(crate) guild_channels: HashMap<String, Vec<Channel>>,
    pub(crate) channel_index: HashMap<String, ChannelRef>,
    /// Threads of guilds whose channels aren't loaded yet (ui/threads.rs).
    pub(crate) pending_threads: HashMap<String, Vec<Channel>>,
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
    /// Channels opened behind the present whose view should land on the
    /// "NOVAS" divider instead of the bottom, like Discord.
    pub(crate) unread_jump: HashSet<String>,
    /// Last message the user has seen (timeline was at the bottom).
    pub(crate) last_seen: HashMap<String, String>,
    /// Per-channel, per-user instant of the last TYPING_START.
    pub(crate) typing: HashMap<String, HashMap<String, std::time::Instant>>,
    /// Latest presence status per user id ("online", "idle", "dnd", …).
    pub(crate) presence: HashMap<String, String>,
    /// The device behind each user's presence ("mobile" or "desktop"):
    /// the status dot becomes a phone for mobile-only users.
    pub(crate) presence_device: HashMap<String, String>,
    /// Who reacted, by (message, emoji key), for the chips' tooltips;
    /// `None` while the request is in flight.
    pub(crate) reactors: HashMap<(String, String), Option<Vec<User>>>,
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
    /// Soundboard senders, lit as speaking until their sound ends.
    pub(crate) sound_speaking: HashMap<String, Instant>,
    /// Connection-quality samples while in voice, oldest first: (ping ms,
    /// incoming loss 0–1), one every 5 s (backend/voice.rs).
    pub(crate) voice_stats: std::collections::VecDeque<(Option<u32>, Option<f32>)>,
    pub(crate) voice_endpoint: String,
    /// Own mic/headphone flags; op 4 sends them, the panel shows them.
    pub(crate) voice_muted: bool,
    /// Screen share capture (docs/SCREENSHARE.md); local preview for now.
    pub(crate) screen: Option<crate::backend::capture::Capture>,
    pub(crate) screen_texture: Option<egui::TextureHandle>,
    /// Our Go Live stream while op 18 is in effect.
    pub(crate) stream: Option<GoLive>,
    /// The stream we watch (op 20), with its decoded frames.
    pub(crate) watch: Option<GoLive>,
    /// CustomIdle: set once we auto-idled after the configured inactivity,
    /// so activity restores the user's chosen status exactly once.
    auto_idle: bool,
    /// CustomIdle: the last instant of user input.
    last_input: Instant,
    pub(crate) watch_texture: Option<egui::TextureHandle>,
    /// Our WHIP publish when FockyTV is the share backend.
    pub(crate) fockytv: Option<crate::plugins::fockytv::Publisher>,
    /// FockyTV lives now: lowercased key (matched to usernames) → key.
    pub(crate) fockytv_live: HashMap<String, String>,
    fockytv_polled: Option<Instant>,
    /// The screen share picker while it's open; the capture starts only
    /// when it's confirmed.
    pub(crate) share_picker: Option<ui::screen_share::Picker>,
    /// Our webcam and its FockyTV publish under `camera_key(nick)`.
    pub(crate) camera: Option<crate::backend::capture::Capture>,
    pub(crate) camera_texture: Option<egui::TextureHandle>,
    pub(crate) camera_publisher: Option<crate::plugins::fockytv::Publisher>,
    pub(crate) voice_deaf: bool,
    /// Voice-state user ids we already asked the API for.
    voice_users_pending: HashSet<String>,
    pub(crate) selected_guild: Option<String>,
    pub(crate) selected_channel: Option<String>,
    pub(crate) user_cache: HashMap<String, User>,
    /// Our role ids per guild, from `LoadOwnMember` (issue #55): the input
    /// to the channel permission math behind the sidebar's lock icons.
    pub(crate) own_roles: HashMap<String, Vec<String>>,
    own_roles_requested: HashSet<String>,
    /// When the current user joined each guild, in ms (issue #54): the
    /// "NOVO" tag marks channels created after it, as Discord computes it.
    pub(crate) guild_joined_at: HashMap<String, i64>,
    /// Channels and threads opened this session: visiting one clears its
    /// "NOVO" tag, the way it disappears in Discord.
    pub(crate) visited_channels: HashSet<String>,
    pub(crate) images: ImageCache,
    /// Larger cache for inline image attachments.
    pub(crate) images_large: ImageCache,
    /// The image opened from the chat in its own window.
    pub(crate) viewer: Option<ui::image_viewer::ImageViewer>,

    pub(crate) compose: String,
    pub(crate) compose_error: Option<String>,
    /// Custom emojis per guild, for the emoji picker.
    pub(crate) guild_emojis: HashMap<String, Vec<crate::model::GuildEmoji>>,
    /// The GIF picker's latest results, with the query they answer.
    pub(crate) gifs: Option<(String, Result<Vec<crate::model::Gif>, String>)>,
    /// Whether the compose box changed since last frame (typing signal).
    pub(crate) last_compose: String,
    pub(crate) last_typing_sent: Option<std::time::Instant>,
    /// Set by "Ir para o presente", consumed inside the timeline's scroll.
    pub(crate) jump_to_present: bool,
    /// Clicked reply target: scrolled to once painted (`None` = pending),
    /// then highlighted for a moment from that instant.
    pub(crate) reply_jump: Option<(String, Option<Instant>)>,
    /// Channels whose cache is an older slice fetched around a reply
    /// target, cut off from the present until "Ir para o presente".
    pub(crate) detached: HashSet<String>,
    /// Member sidebar (member_list.rs).
    pub(crate) members: crate::member_list::Members,
    pub(crate) settings_open: bool,
    /// Page the settings window shows (left nav).
    pub(crate) settings_tab: ui::settings_window::SettingsTab,
    pub(crate) plugins_ui: ui::plugins::State,
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
    /// Settings → Perfil draft and requests (ui/profile_settings.rs).
    pub(crate) profile: ui::profile_settings::ProfileEditor,
    /// Forum posts' first messages and paging (ui/threads.rs).
    pub(crate) forums: ui::threads::Forums,
    /// Filled once a newer release is on disk (updater.rs).
    update: crate::updater::Slot,
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
            pending_threads: HashMap::new(),
            messages: HashMap::new(),
            has_more: HashMap::new(),
            stale_channels: HashMap::new(),
            loading_channels: HashSet::new(),
            loading_guilds: HashSet::new(),
            unread: HashMap::new(),
            mentions: HashMap::new(),
            first_unread: HashMap::new(),
            unread_jump: HashSet::new(),
            last_seen: HashMap::new(),
            typing: HashMap::new(),
            presence: HashMap::new(),
            presence_device: HashMap::new(),
            reactors: HashMap::new(),
            voice_states: HashMap::new(),
            voice: VoiceConn::Disconnected,
            voice_connected_at: None,
            desync_rejoins: Vec::new(),
            voice_users_pending: HashSet::new(),
            speaking: HashSet::new(),
            sound_speaking: HashMap::new(),
            voice_stats: Default::default(),
            voice_endpoint: String::new(),
            voice_muted: false,
            screen: None,
            screen_texture: None,
            stream: None,
            watch: None,
            auto_idle: false,
            last_input: Instant::now(),
            watch_texture: None,
            fockytv: None,
            fockytv_live: HashMap::new(),
            fockytv_polled: None,
            share_picker: None,
            camera: None,
            camera_texture: None,
            camera_publisher: None,
            voice_deaf: false,
            disconnected_at: None,
            selected_guild: None,
            selected_channel: None,
            user_cache: HashMap::new(),
            own_roles: HashMap::new(),
            own_roles_requested: HashSet::new(),
            guild_joined_at: HashMap::new(),
            visited_channels: HashSet::new(),
            images: ImageCache::new(128),
            images_large: ImageCache::new(800),
            viewer: None,
            compose: String::new(),
            compose_error: None,
            guild_emojis: HashMap::new(),
            gifs: None,
            last_compose: String::new(),
            last_typing_sent: None,
            jump_to_present: false,
            reply_jump: None,
            detached: HashSet::new(),
            members: Default::default(),
            settings_open: false,
            settings_tab: Default::default(),
            plugins_ui: Default::default(),
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
            profile: Default::default(),
            forums: Default::default(),
            update: if settings.check_for_updates {
                crate::updater::spawn(&handle, _cc.egui_ctx.clone())
            } else {
                Default::default()
            },
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
            #[cfg(feature = "tray")]
            tray: None,
            quitting: false,
            can_hide_window: _cc
                .winit_window()
                .is_some_and(|window| window.is_visible().is_some()),
        };

        if _cc.winit_window().is_some() {
            app.configure_tray(&_cc.egui_ctx);
        }

        // Image loaders for the animated WebP splash.
        egui_extras::install_image_loaders(&_cc.egui_ctx);
        theme::install_fonts(&_cc.egui_ctx);

        if let Some(token) = app.settings.token.clone() {
            if let Some(cache) = _cc
                .storage
                .and_then(|storage| load_startup_cache(storage, &token))
            {
                app.me = Some(cache.me);
                app.guilds = cache.guilds;
                app.dm_channels = cache.dm_channels;
                app.guild_channels = cache.guild_channels;
                app.relationships = cache.relationships;
                log::info!(
                    "cache inicial restaurado: {} servidores, {} DMs",
                    app.guilds.len(),
                    app.dm_channels.len()
                );
                for channel in &app.dm_channels {
                    app.channel_index.insert(channel.id.clone(), ChannelRef::Dm);
                    for user in &channel.recipients {
                        app.user_cache.insert(user.id.clone(), user.clone());
                    }
                }
                for (guild_id, channels) in &app.guild_channels {
                    for channel in channels {
                        app.channel_index
                            .insert(channel.id.clone(), ChannelRef::Guild(guild_id.clone()));
                    }
                }
                app.selected_guild = app
                    .settings
                    .selected_guild_id
                    .clone()
                    .filter(|id| app.guilds.iter().any(|guild| &guild.id == id));
                app.selected_channel = app
                    .settings
                    .selected_channel_id
                    .clone()
                    .filter(|id| app.channel_index.contains_key(id));
            }
            app.start_backend(token);
        }
        app
    }

    pub(crate) fn tray_available(&self) -> bool {
        if !self.can_hide_window {
            return false;
        }
        #[cfg(feature = "tray")]
        {
            self.tray
                .as_ref()
                .is_some_and(crate::tray::Tray::is_available)
        }
        #[cfg(not(feature = "tray"))]
        {
            false
        }
    }

    pub(crate) fn configure_tray(&mut self, ctx: &egui::Context) {
        #[cfg(feature = "tray")]
        {
            self.tray = None;
            if self.settings.tray {
                match crate::tray::install(self.tray_tx.clone(), ctx.clone(), &self.handle) {
                    Ok(tray) => self.tray = Some(tray),
                    Err(err) => log::warn!("bandeja indisponível: {err}"),
                }
            }
        }
        #[cfg(not(feature = "tray"))]
        let _ = ctx;
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
        self.extras.dj.disconnect();
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
        self.pending_threads.clear();
        self.forums = Default::default();
        self.messages.clear();
        self.has_more.clear();
        self.stale_channels.clear();
        self.loading_channels.clear();
        self.loading_guilds.clear();
        self.unread.clear();
        self.mentions.clear();
        self.first_unread.clear();
        self.unread_jump.clear();
        self.last_seen.clear();
        self.typing.clear();
        self.presence.clear();
        self.presence_device.clear();
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
        self.profile = Default::default();
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
        }
        if self.loading_guilds.insert(guild_id.to_string()) {
            self.send(Command::LoadGuildChannels {
                guild_id: guild_id.to_string(),
            });
        }
        self.send(Command::SubscribeGuild {
            guild_id: guild_id.to_string(),
            user_ids: Vec::new(),
        });
        self.settings.save();
    }

    /// Issue #36: fetch a guild's channels before it is opened. The rail
    /// calls it on hover, so by the click the sidebar is ready and no
    /// "Carregando canais…" pop-in shows.
    pub(crate) fn prefetch_guild_channels(&mut self, guild_id: &str) {
        if !self.guild_channels.contains_key(guild_id)
            && self.loading_guilds.insert(guild_id.to_string())
        {
            self.send(Command::LoadGuildChannels {
                guild_id: guild_id.to_string(),
            });
        }
    }

    /// Pin (or unpin) a guild at the top of the server rail.
    pub(crate) fn toggle_favorite_guild(&mut self, guild_id: &str) {
        if !self.settings.favorite_guilds.remove(guild_id) {
            self.settings.favorite_guilds.insert(guild_id.to_string());
        }
        self.settings.save();
    }

    /// Mute (or unmute) a guild: the account-wide flag rides to Discord,
    /// the rail stops showing unread pills and mention badges.
    pub(crate) fn toggle_guild_mute(&mut self, guild_id: &str) {
        let muted = !self.settings.muted_guilds.contains(guild_id);
        if muted {
            self.settings.muted_guilds.insert(guild_id.to_string());
        } else {
            self.settings.muted_guilds.remove(guild_id);
        }
        self.settings.save();
        self.send(Command::SetGuildMute {
            guild_id: guild_id.to_string(),
            muted,
        });
    }

    /// Leave a guild: the REST call does the work, GuildLeft drops the
    /// local state.
    pub(crate) fn leave_guild(&mut self, guild_id: &str) {
        self.send(Command::LeaveGuild {
            guild_id: guild_id.to_string(),
        });
    }

    /// Drop every trace of a guild we just left.
    fn forget_guild(&mut self, guild_id: &str) {
        self.guilds.retain(|guild| guild.id != guild_id);
        self.guild_channels.remove(guild_id);
        let gone: HashSet<String> = self
            .channel_index
            .iter()
            .filter(|(_, channel)| matches!(channel, ChannelRef::Guild(owner) if owner == guild_id))
            .map(|(id, _)| id.clone())
            .collect();
        self.channel_index.retain(|id, _| !gone.contains(id));
        self.messages.retain(|id, _| !gone.contains(id));
        self.unread.retain(|id, _| !gone.contains(id));
        self.mentions.retain(|id, _| !gone.contains(id));
        self.first_unread.retain(|id, _| !gone.contains(id));
        self.last_seen.retain(|id, _| !gone.contains(id));
        self.settings.favorite_guilds.remove(guild_id);
        self.settings.muted_guilds.remove(guild_id);
        self.settings.last_channel_by_guild.remove(guild_id);
        if self.selected_guild.as_deref() == Some(guild_id) {
            let fallback = self.guilds.first().map(|guild| guild.id.clone());
            match fallback {
                Some(next) => self.select_guild(&next),
                None => self.select_home(),
            }
        }
        self.settings.save();
    }

    /// Fold a gateway reaction echo into a loaded message: bump or drop
    /// the matching chip, and track whether it is ours.
    pub(crate) fn apply_reaction(
        &mut self,
        channel_id: &str,
        message_id: &str,
        user_id: &str,
        emoji: crate::model::ReactionEmoji,
        added: bool,
    ) {
        let mine = self.me.as_ref().is_some_and(|me| me.id == user_id);
        if let Some(key) = emoji.api_key() {
            self.reactors.remove(&(message_id.to_string(), key));
        }
        let Some(message) = self
            .messages
            .get_mut(channel_id)
            .and_then(|list| list.iter_mut().find(|m| m.id == message_id))
        else {
            return;
        };
        let same = |r: &crate::model::Reaction| match (&r.emoji.id, &emoji.id) {
            (Some(a), Some(b)) => a == b,
            (None, None) => r.emoji.name == emoji.name,
            _ => false,
        };
        match message.reactions.iter().position(same) {
            Some(i) if added => {
                let reaction = &mut message.reactions[i];
                // Our echo after the optimistic flip must not count twice.
                if !(mine && reaction.me) {
                    reaction.count += 1;
                }
                reaction.me |= mine;
            }
            Some(i) => {
                let reaction = &mut message.reactions[i];
                if !(mine && !reaction.me) {
                    reaction.count = reaction.count.saturating_sub(1);
                }
                if mine {
                    reaction.me = false;
                }
                if reaction.count == 0 {
                    message.reactions.remove(i);
                }
            }
            None if added => message.reactions.push(crate::model::Reaction {
                count: 1,
                emoji,
                me: mine,
            }),
            None => {}
        }
    }

    /// A click on a reaction chip: flip ours optimistically, then tell
    /// Discord; `apply_reaction` swallows the gateway echo.
    pub(crate) fn toggle_reaction(
        &mut self,
        channel_id: &str,
        message_id: &str,
        emoji: crate::model::ReactionEmoji,
        add: bool,
    ) {
        let Some(me) = self.me.as_ref().map(|me| me.id.clone()) else {
            return;
        };
        let Some(key) = emoji.api_key() else {
            return;
        };
        self.apply_reaction(channel_id, message_id, &me, emoji, add);
        self.send(Command::ToggleReaction {
            channel_id: channel_id.to_string(),
            message_id: message_id.to_string(),
            emoji: key,
            add,
        });
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
        self.visited_channels.insert(channel_id.clone());
        self.unread.remove(&channel_id);
        self.mentions.remove(&channel_id);
        // A pending divider from earlier in the session lands the view on
        // it, like Discord.
        if self.first_unread.contains_key(&channel_id) {
            self.unread_jump.insert(channel_id.clone());
        }
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
        self.request_channel_presences(&channel_id);
        self.fetch_channel_if_needed(channel_id);
    }

    /// Issue #55: the sidebar's lock, by Discord's permission rules — base
    /// guild permissions (@everyone plus our roles) under the channel's
    /// overwrites, falling back to the category's when the channel is
    /// synced. Private threads we aren't in always lock; public ones ride
    /// on the parent. Without the role data yet nothing locks (fail open).
    pub(crate) fn channel_hidden(&self, channel: &Channel, guild_id: &str) -> bool {
        if channel.is_thread() {
            return matches!(channel.kind, crate::model::CHANNEL_KIND_PRIVATE_THREAD)
                && channel.member.is_none();
        }
        let (roles, own_roles, me) = match (
            self.members.roles.get(guild_id),
            self.own_roles.get(guild_id),
            self.me.as_ref(),
        ) {
            (Some(roles), Some(own_roles), Some(me)) => (roles, own_roles, me),
            _ => return false,
        };
        let mut perms = roles.get(guild_id).map_or(0, |role| role.permissions);
        for id in own_roles {
            if let Some(role) = roles.get(id) {
                perms |= role.permissions;
            }
        }
        if perms & crate::model::PERMISSION_ADMINISTRATOR != 0 {
            return false;
        }
        // Synced channels carry no overwrites of their own: the category's
        // apply instead.
        let has_overwrites =
            |channel: &Channel| !channel.permission_overwrites.is_empty();
        let source = if has_overwrites(channel) {
            Some(channel)
        } else {
            channel
                .parent_id
                .as_deref()
                .and_then(|id| self.find_channel(id))
                .filter(|parent| has_overwrites(parent))
        };
        if let Some(source) = source {
            let (mut allow, mut deny) = (0u64, 0u64);
            let (mut member_allow, mut member_deny) = (0u64, 0u64);
            for overwrite in &source.permission_overwrites {
                if overwrite.kind == 0 {
                    if overwrite.id == guild_id {
                        perms = (perms & !overwrite.deny) | overwrite.allow;
                    } else if own_roles.contains(&overwrite.id) {
                        allow |= overwrite.allow;
                        deny |= overwrite.deny;
                    }
                } else if overwrite.kind == 1 && overwrite.id == me.id {
                    member_allow = overwrite.allow;
                    member_deny = overwrite.deny;
                }
            }
            perms = (perms & !deny) | allow;
            perms = (perms & !member_deny) | member_allow;
        }
        perms & crate::model::PERMISSION_VIEW_CHANNEL == 0
    }

    /// Issue #54: the sidebar's "NOVO" tag, Discord's rule — the channel
    /// was created after we joined the guild (snowflake timestamp against
    /// `joined_at`, both Discord's data) and we haven't opened it yet.
    pub(crate) fn channel_is_new(&self, channel_id: &str, guild_id: &str) -> bool {
        if self.visited_channels.contains(channel_id) {
            return false;
        }
        let Some(joined_at) = self.guild_joined_at.get(guild_id) else {
            return false;
        };
        util::snowflake_ms(channel_id).is_some_and(|created| created > *joined_at)
    }

    fn request_channel_presences(&self, channel_id: &str) {
        let Some(ChannelRef::Guild(guild_id)) = self.channel_index.get(channel_id) else {
            return;
        };
        let mut user_ids: Vec<String> = self
            .messages
            .get(channel_id)
            .into_iter()
            .flatten()
            .map(|message| message.author.id.clone())
            .collect();
        if let Some(roster) = self.voice_states.get(guild_id) {
            user_ids.extend(
                roster
                    .values()
                    .filter(|state| state.channel_id.is_some())
                    .map(|state| state.user_id.clone()),
            );
        }
        user_ids.sort();
        user_ids.dedup();
        if !user_ids.is_empty() {
            self.send(Command::SubscribeGuild {
                guild_id: guild_id.clone(),
                user_ids,
            });
        }
    }

    fn fetch_channel_if_needed(&mut self, channel_id: String) {
        if self.loading_channels.contains(&channel_id) {
            return;
        }
        // A forum has posts, not messages (ui/threads.rs lists them).
        if self
            .find_channel(&channel_id)
            .is_some_and(Channel::is_forum)
        {
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

    /// Scrolls to a replied-to message, fetching the history around it
    /// first when it isn't loaded.
    pub(crate) fn jump_to_message(&mut self, channel_id: &str, message_id: String) {
        let loaded = self
            .messages
            .get(channel_id)
            .is_some_and(|list| list.iter().any(|message| message.id == message_id));
        self.reply_jump = Some((message_id.clone(), None));
        if !loaded {
            self.send(Command::LoadMessagesAround {
                channel_id: channel_id.to_string(),
                message_id,
            });
        }
    }

    /// "Ir para o presente": a slice around an old reply is swapped for the
    /// latest page.
    pub(crate) fn return_to_present(&mut self, channel_id: &str) {
        if self.detached.remove(channel_id) {
            self.messages.remove(channel_id);
            self.stale_channels.remove(channel_id);
            self.fetch_channel_if_needed(channel_id.to_string());
        }
        self.jump_to_present = true;
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
        self.unread_jump.remove(channel_id);
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
                return self.dock_watch();
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

    /// Opens the share picker (source, resolution, fps), or stops a
    /// running share.
    pub(crate) fn toggle_screen_share(&mut self) {
        if self.screen.is_some() {
            self.stop_screen_share();
            return;
        }
        if !matches!(self.voice, VoiceConn::Connected { .. }) || self.me.is_none() {
            return;
        }
        if self.uses_fockytv()
            && crate::plugins::fockytv::validate_stream_key(&self.fockytv_nick()).is_err()
        {
            ui::settings_window::open_at(self, ui::settings_window::SettingsTab::Plugins);
            return;
        }
        self.share_picker = Some(ui::screen_share::Picker::new());
    }

    /// The picker's "Compartilhar": captures `source` (Linux: the portal
    /// dialog picks) and goes live in the connected voice channel.
    pub(crate) fn start_screen_share(
        &mut self,
        ctx: &egui::Context,
        source: Option<crate::backend::capture::Source>,
    ) {
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
        if self.uses_fockytv() {
            let nick = self.fockytv_nick();
            let ctx = ctx.clone();
            let capture = crate::backend::capture::start(self.handle.clone(), source, move || {
                ctx.request_repaint()
            });
            play_sound(Sound::StreamStart);
            self.fockytv = Some(crate::plugins::fockytv::publish(
                &self.settings.plugins.fockytv.url,
                &nick,
                self.settings.plugins.fockytv.fps,
                self.settings.plugins.fockytv.max_height,
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
            source,
            move || ctx.request_repaint(),
        ));
    }

    /// Turns the webcam on (published to FockyTV as `camera_key(nick)`, so
    /// it runs next to a screen share) or off. FockyTV only: Songbird sends
    /// no video, so there's no Discord-native camera.
    pub(crate) fn toggle_camera(&mut self, ctx: &egui::Context) {
        if self.camera.is_some() {
            self.stop_camera();
            return;
        }
        if !matches!(self.voice, VoiceConn::Connected { .. }) {
            return;
        }
        let nick = self.fockytv_nick();
        if !self.uses_fockytv() || crate::plugins::fockytv::validate_stream_key(&nick).is_err() {
            ui::settings_window::open_at(self, ui::settings_window::SettingsTab::Plugins);
            return;
        }
        let ctx = ctx.clone();
        let camera =
            crate::backend::camera::start(self.settings.camera_device.clone(), move || {
                ctx.request_repaint()
            });
        self.camera_publisher = Some(crate::plugins::fockytv::publish(
            &self.settings.plugins.fockytv.url,
            &crate::plugins::fockytv::camera_key(&nick),
            30,
            0,
            Arc::clone(&camera.frame),
        ));
        self.camera = Some(camera);
    }

    /// Every way the camera ends (button, device gone, left voice).
    pub(crate) fn stop_camera(&mut self) {
        self.camera = None;
        self.camera_texture = None;
        self.camera_publisher = None;
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

    pub(crate) fn uses_fockytv(&self) -> bool {
        self.settings
            .plugins
            .backend(crate::plugins::golive_bypass::active())
            == crate::plugins::ShareBackend::FockyTv
    }

    /// Our FockyTV stream key: the configured nick, else the username.
    pub(crate) fn fockytv_nick(&self) -> String {
        self.settings
            .plugins
            .fockytv
            .nick
            .clone()
            .filter(|nick| !nick.trim().is_empty())
            .or_else(|| self.me.as_ref().map(|me| me.username.clone()))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// Green ring: producing audio, or a soundboard sound they sent is
    /// still playing.
    pub(crate) fn is_speaking(&self, user_id: &str) -> bool {
        self.speaking.contains(user_id)
            || self
                .sound_speaking
                .get(user_id)
                .is_some_and(|until| Instant::now() < *until)
    }

    /// Whether `user_id` is live, on Discord's Go Live or on FockyTV (by
    /// username, since the stream key is the nickname).
    pub(crate) fn is_live(&self, user_id: &str, discord_stream: bool) -> bool {
        if discord_stream {
            return true;
        }
        if !self.uses_fockytv() {
            return false;
        }
        if self.me.as_ref().is_some_and(|me| me.id == user_id) {
            return self.fockytv.is_some();
        }
        self.fockytv_key(user_id, false).is_some()
    }

    /// Whether `user_id`'s webcam is live on FockyTV.
    pub(crate) fn has_camera(&self, user_id: &str) -> bool {
        if self.me.as_ref().is_some_and(|me| me.id == user_id) {
            return self.camera_publisher.is_some();
        }
        self.fockytv_key(user_id, true).is_some()
    }

    /// `user_id`'s live FockyTV key: their username for the screen,
    /// `camera_key(username)` for the webcam.
    fn fockytv_key(&self, user_id: &str, camera: bool) -> Option<String> {
        if !self.uses_fockytv() {
            return None;
        }
        let username = &self.user_cache.get(user_id)?.username;
        let key = if camera {
            crate::plugins::fockytv::camera_key(username)
        } else {
            username.clone()
        };
        self.fockytv_live.get(&key.to_lowercase()).cloned()
    }

    /// "Assistir": the member's FockyTV live (WHEP) or Discord's Go Live,
    /// both in the watch view in place of the chat.
    pub(crate) fn watch(&mut self, user_id: &str) {
        match self.fockytv_key(user_id, false) {
            Some(key) => self.watch_fockytv(key),
            None => self.watch_stream(user_id),
        }
    }

    /// The member's webcam, in the same watch view as a live.
    pub(crate) fn watch_camera(&mut self, user_id: &str) {
        if let Some(key) = self.fockytv_key(user_id, true) {
            self.watch_fockytv(key);
        }
    }

    fn watch_fockytv(&mut self, key: String) {
        if crate::plugins::fockytv::validate_stream_key(&self.fockytv_nick()).is_err() {
            ui::settings_window::open_at(self, ui::settings_window::SettingsTab::Plugins);
            return;
        }
        self.stop_watching();
        let mut watch = GoLive::new(format!("fockytv:{key}"));
        watch.fockytv = Some(crate::plugins::fockytv::watch(
            &self.settings.plugins.fockytv.url,
            &key,
            &self.fockytv_nick(),
            Arc::clone(&watch.frames),
            self.event_tx.clone(),
        ));
        self.watch = Some(watch);
        self.dock_watch();
    }

    /// While in a call, refreshes who's live on FockyTV every 5 s.
    fn poll_fockytv(&mut self) {
        if !self.uses_fockytv() || !matches!(self.voice, VoiceConn::Connected { .. }) {
            return;
        }
        if self
            .fockytv_polled
            .is_some_and(|at| at.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        self.fockytv_polled = Some(Instant::now());
        let (server, events) = (
            self.settings.plugins.fockytv.url.clone(),
            self.event_tx.clone(),
        );
        self.handle.spawn(async move {
            match crate::plugins::fockytv::live_keys(&server).await {
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
        if let Some(publisher) = self.camera_publisher.take() {
            publisher.stop_and_wait();
        }
        self.stop_screen_share();
        self.stop_camera();
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
        self.dock_watch();
    }

    pub(crate) fn stop_watching(&mut self) {
        if let Some(watch) = self.close_watch().filter(|watch| watch.fockytv.is_none()) {
            self.send(Command::StopStream {
                stream_key: watch.key.clone(),
            });
        }
    }

    /// Drops the watched live; if it was docked, the guild's last text
    /// channel takes its place.
    fn close_watch(&mut self) -> Option<GoLive> {
        let docked = self.watch_docked();
        self.watch_texture = None;
        let watch = self.watch.take();
        if docked && let Some(guild_id) = self.selected_guild.clone() {
            self.restore_channel_for_guild(&guild_id);
        }
        watch
    }

    /// The voice channel a watched live belongs to: selected, it shows the
    /// live in place of the chat; from anywhere else the live floats.
    fn watch_channel(&self) -> Option<(&String, &String)> {
        match &self.voice {
            VoiceConn::Connected {
                guild_id,
                channel_id,
            } => Some((guild_id, channel_id)),
            _ => None,
        }
    }

    pub(crate) fn watch_docked(&self) -> bool {
        self.watch.is_some()
            && self
                .watch_channel()
                .is_none_or(|(_, channel)| self.selected_channel.as_ref() == Some(channel))
    }

    /// Opens the watched live's voice channel, docking the live. Not saved
    /// as the guild's last channel: that stays a text one.
    pub(crate) fn dock_watch(&mut self) {
        if self.watch.is_none() {
            return;
        }
        let Some((guild_id, channel_id)) = self
            .watch_channel()
            .map(|(guild, channel)| (guild.clone(), channel.clone()))
        else {
            return;
        };
        self.home_page = ui::friends::HomePage::DirectMessages;
        self.selected_guild = Some(guild_id);
        self.selected_channel = Some(channel_id);
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

    /// Own status from the user panel or the tray: kept in settings.json,
    /// sent over the gateway and saved in the account settings. An explicit
    /// choice cancels the CustomIdle auto-idle.
    pub(crate) fn set_own_status(
        &mut self,
        status: &str,
        custom_status: crate::settings::CustomStatus,
    ) {
        self.settings.status = status.to_owned();
        self.settings.custom_status = custom_status;
        self.auto_idle = false;
        self.last_input = Instant::now();
        self.settings.save();
        self.send_presence(true);
    }

    /// op 3 with the saved status; the own dot follows at once, without
    /// waiting for Discord to echo it.
    fn send_presence(&mut self, save: bool) {
        if let Some(me) = &self.me {
            self.presence
                .insert(me.id.clone(), self.settings.status.clone());
        }
        self.send(Command::SetPresence {
            status: self.settings.status.clone(),
            custom_status: self.settings.custom_status.clone(),
            save,
        });
    }

    /// CustomIdle, the Vencord plugin natively: after
    /// `settings.idle_timeout_minutes` without input, appear idle; any
    /// input restores the chosen status. Never overrides an explicit
    /// "Não perturbe" or "Invisível".
    fn poll_custom_idle(&mut self, ctx: &egui::Context) {
        let timeout = u64::from(self.settings.idle_timeout_minutes);
        if timeout == 0 {
            if self.auto_idle {
                // The feature was just turned off: restore the status.
                self.auto_idle = false;
                self.send_presence(false);
            }
            self.last_input = Instant::now();
            return;
        }
        let active = ctx.input(|input| !input.events.is_empty());
        if active {
            self.last_input = Instant::now();
            if self.auto_idle {
                self.auto_idle = false;
                self.send_presence(false);
            }
        } else if !self.auto_idle
            && !matches!(self.settings.status.as_str(), "dnd" | "invisible")
            && self.last_input.elapsed() >= Duration::from_secs(timeout * 60)
        {
            self.auto_idle = true;
            if let Some(me) = &self.me {
                self.presence.insert(me.id.clone(), "idle".into());
            }
            self.send(Command::SetPresence {
                status: "idle".into(),
                custom_status: self.settings.custom_status.clone(),
                save: false,
            });
        }
        // Wake exactly at the deadline; egui keeps the earliest repaint
        // request of the frame.
        let remaining = (self.last_input + Duration::from_secs(timeout * 60))
            .saturating_duration_since(Instant::now());
        ctx.request_repaint_after(remaining.max(Duration::from_millis(250)));
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
        // A desync rejoin leaves the shared DJ locally; other listeners keep playing.
        self.extras.dj.disconnect();
        self.stop_screen_share();
        self.stop_camera();
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
            self.handle_event(
                event,
                ctx.input(|input| input.focused && input.viewport().visible().unwrap_or(true)),
            );
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
                // Slices around old replies would catch up page by page:
                // drop them, reopening loads the present.
                self.messages.retain(|id, _| !self.detached.contains(id));
                self.detached.clear();
                self.members = Default::default();
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
                self.send_presence(false);
                self.refresh_relationships();
                if let Some(channel_id) = self.selected_channel.clone() {
                    self.fetch_channel_if_needed(channel_id);
                }
            }
            UiEvent::ReadState { markers } => {
                // Discord's snapshot seeds the read markers; anything the
                // user already read in this session wins.
                for (channel_id, message_id) in markers {
                    self.last_seen.entry(channel_id).or_insert(message_id);
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
                self.guild_channels
                    .retain(|id, _| guilds.iter().any(|guild| &guild.id == id));
                self.channel_index.retain(|_, channel| match channel {
                    ChannelRef::Guild(id) => guilds.iter().any(|guild| &guild.id == id),
                    ChannelRef::Dm => true,
                });
                if self
                    .selected_guild
                    .as_ref()
                    .is_some_and(|id| !guilds.iter().any(|guild| &guild.id == id))
                {
                    self.selected_guild = None;
                    self.selected_channel = None;
                }
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
            UiEvent::GuildLeft { guild_id } => self.forget_guild(&guild_id),
            UiEvent::ReactionChanged {
                channel_id,
                message_id,
                user_id,
                emoji,
                added,
            } => self.apply_reaction(&channel_id, &message_id, &user_id, emoji, added),
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
                self.channel_index
                    .retain(|_, channel| !matches!(channel, ChannelRef::Dm));
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
                    && self
                        .selected_channel
                        .as_ref()
                        .is_some_and(|id| !self.channel_index.contains_key(id))
                {
                    self.selected_channel = None;
                }
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
                // REST lists no threads; keep the ones the gateway gave us.
                let mut channels = channels;
                channels.extend(
                    self.guild_channels
                        .get(&guild_id)
                        .into_iter()
                        .flatten()
                        .filter(|channel| channel.is_thread())
                        .cloned(),
                );
                channels.extend(self.pending_threads.remove(&guild_id).unwrap_or_default());
                self.channel_index.retain(
                    |_, channel| !matches!(channel, ChannelRef::Guild(id) if id == &guild_id),
                );
                for channel in &channels {
                    self.channel_index
                        .insert(channel.id.clone(), ChannelRef::Guild(guild_id.clone()));
                }
                self.guild_channels.insert(guild_id.clone(), channels);
                self.loading_guilds.remove(&guild_id);
                // The lock icons need our role ids in the guild (issue #55).
                if let Some(me) = self.me.as_ref()
                    && !self.own_roles.contains_key(&guild_id)
                    && self.own_roles_requested.insert(guild_id.clone())
                {
                    self.send(Command::LoadOwnMember {
                        guild_id: guild_id.clone(),
                        user_id: me.id.clone(),
                    });
                }
                if self.selected_guild.as_deref() == Some(guild_id.as_str())
                    && self
                        .selected_channel
                        .as_ref()
                        .is_none_or(|id| !self.channel_index.contains_key(id))
                {
                    self.selected_channel = None;
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
                // Opening a channel behind the present: the "NOVAS" divider
                // goes at the first message after the read marker and the
                // view lands on it, like Discord's open-at-unread.
                if !older
                    && !self.detached.contains(&channel_id)
                    && let Some(marker) = self.last_seen.get(&channel_id).cloned()
                {
                    let first_after_marker = entry
                        .iter()
                        .find(|message| snowflake_cmp(&message.id, &marker).is_gt())
                        .map(|message| message.id.clone());
                    let behind_present = entry
                        .last()
                        .is_some_and(|last| snowflake_cmp(&last.id, &marker).is_gt());
                    if behind_present
                        && let Some(id) = first_after_marker
                        && self
                            .first_unread
                            .entry(channel_id.clone())
                            .or_insert(id.clone())
                            == &id
                    {
                        self.unread_jump.insert(channel_id.clone());
                    }
                }
                if self.selected_channel.as_deref() == Some(channel_id.as_str()) {
                    self.request_channel_presences(&channel_id);
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
                self.note_thread_message(&message);
                let selected =
                    self.selected_channel.as_deref() == Some(message.channel_id.as_str());
                let new_author = self
                    .messages
                    .get(&message.channel_id)
                    .is_none_or(|list| !list.iter().any(|old| old.author.id == message.author.id));
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
                let detached = self.detached.contains(&message.channel_id);
                if let Some(list) = self.messages.get_mut(&message.channel_id) {
                    // A slice around an old reply doesn't reach the present.
                    if !detached && !list.iter().any(|m| m.id == message.id) {
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
                if selected && new_author {
                    self.request_channel_presences(&message.channel_id);
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
                embeds,
            } => {
                if let Some(list) = self.messages.get_mut(&channel_id)
                    && let Some(message) = list.iter_mut().find(|m| m.id == message_id)
                {
                    if let Some(content) = content {
                        message.content = content;
                    }
                    if let Some(embeds) = embeds {
                        message.embeds = embeds;
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
                        clan: None,
                    });
                self.typing
                    .entry(channel_id)
                    .or_default()
                    .insert(user_id, std::time::Instant::now());
            }
            UiEvent::PresenceUpdate {
                user_id,
                status,
                device,
            } => {
                self.presence.insert(user_id.clone(), status);
                match device {
                    Some(device) => {
                        self.presence_device.insert(user_id, device);
                    }
                    None => {
                        self.presence_device.remove(&user_id);
                    }
                }
            }
            UiEvent::Reactors {
                message_id,
                emoji,
                users,
            } => {
                self.reactors.insert((message_id, emoji), Some(users));
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
                if self.selected_guild.as_deref() == Some(guild_id.as_str())
                    && let Some(channel_id) = self.selected_channel.as_deref()
                {
                    self.request_channel_presences(channel_id);
                }
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
                let joined = state.channel_id.is_some()
                    && self
                        .voice_states
                        .get(&guild_id)
                        .and_then(|roster| roster.get(&user_id))
                        .is_none_or(|previous| previous.channel_id.is_none());
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
                if joined
                    && self.selected_guild.as_deref() == Some(guild_id.as_str())
                    && let Some(channel_id) = self.selected_channel.as_deref()
                {
                    self.request_channel_presences(channel_id);
                }
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
            UiEvent::VoiceEffect {
                user_id,
                sound_id,
                volume,
            } => {
                // Deafened: silent, but the sender still lights up.
                let volume = if self.voice_deaf { 0.0 } else { volume };
                let events = self.event_tx.clone();
                self.handle.spawn(async move {
                    let Some(duration) =
                        crate::backend::soundboard::play(sound_id.clone(), volume).await
                    else {
                        return;
                    };
                    events.send(UiEvent::SoundboardPlaying {
                        user_id,
                        sound_id,
                        duration,
                    });
                    // Repaint once it ends so the ring goes off.
                    tokio::time::sleep(duration).await;
                    events.repaint();
                });
            }
            UiEvent::SoundboardPlaying {
                user_id,
                sound_id,
                duration,
            } => {
                let now = Instant::now();
                self.sound_speaking.insert(user_id, now + duration);
                self.extras.sound_playing(sound_id, now, duration);
            }
            UiEvent::GifsLoaded { query, result } => self.gifs = Some((query, result)),
            UiEvent::GuildEmojis { guild_id, emojis } => {
                self.guild_emojis.insert(guild_id, emojis);
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
                self.extras.dj.disconnect();
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
                    self.close_watch();
                }
            }
            UiEvent::VoiceLeft => {
                if matches!(self.voice, VoiceConn::Connected { .. }) {
                    play_sound(Sound::Disconnect);
                }
                self.extras.dj.disconnect();
                self.stop_screen_share();
                self.stop_camera();
                self.stop_watching();
                self.voice = VoiceConn::Disconnected;
                self.voice_connected_at = None;
                self.speaking.clear();
                self.sound_speaking.clear();
                self.voice_stats.clear();
                self.voice_endpoint.clear();
            }
            UiEvent::VoiceStats {
                ping_ms,
                loss,
                endpoint,
            } => {
                // Five minutes of history.
                if self.voice_stats.len() == 60 {
                    self.voice_stats.pop_front();
                }
                self.voice_stats.push_back((ping_ms, loss));
                self.voice_endpoint = endpoint;
            }
            UiEvent::GuildRoles { guild_id, roles } => {
                let roles = roles.into_iter().map(|role| (role.id.clone(), role));
                self.members.roles.insert(guild_id, roles.collect());
            }
            UiEvent::OwnMemberLoaded { guild_id, roles } => {
                self.own_roles.insert(guild_id, roles);
            }
            UiEvent::GuildJoinedAt {
                guild_id,
                joined_at_ms,
            } => {
                self.guild_joined_at.insert(guild_id, joined_at_ms);
            }
            UiEvent::MemberListUpdate { update } => {
                for member in update.members() {
                    let status = member.presence.as_ref().map(|p| p.status.clone());
                    match member
                        .presence
                        .as_ref()
                        .and_then(|p| crate::model::presence_device(p.client_status.as_ref()))
                    {
                        Some(device) => self.presence_device.insert(member.user.id.clone(), device),
                        None => self.presence_device.remove(&member.user.id),
                    };
                    self.presence.insert(
                        member.user.id.clone(),
                        status.unwrap_or_else(|| "offline".into()),
                    );
                    self.user_cache
                        .insert(member.user.id.clone(), member.user.clone());
                }
                let guild_id = update.guild_id.clone();
                self.members
                    .lists
                    .entry(guild_id)
                    .or_default()
                    .apply(update);
            }
            UiEvent::MessagesAround {
                channel_id,
                message_id,
                messages,
            } => {
                let wanted = self
                    .reply_jump
                    .as_ref()
                    .is_some_and(|(id, _)| *id == message_id);
                if !wanted {
                    return;
                }
                // Deleted (or the fetch failed): nothing to jump to.
                if !messages.iter().any(|message| message.id == message_id) {
                    self.reply_jump = None;
                    return;
                }
                let entry = self.messages.entry(channel_id.clone()).or_default();
                let overlaps = match (
                    entry.first(),
                    entry.last(),
                    messages.first(),
                    messages.last(),
                ) {
                    (Some(first), Some(last), Some(page_first), Some(page_last)) => {
                        snowflake_cmp(&page_last.id, &first.id).is_ge()
                            && snowflake_cmp(&page_first.id, &last.id).is_le()
                    }
                    _ => false,
                };
                if overlaps {
                    merge_messages(entry, messages);
                } else {
                    *entry = messages;
                    self.has_more.insert(channel_id.clone(), true);
                    self.first_unread.remove(&channel_id);
                    self.detached.insert(channel_id);
                }
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
                        clan: None,
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
            UiEvent::Profile(event) => ui::profile_settings::handle_event(self, event),
            event @ (UiEvent::ThreadsSynced { .. }
            | UiEvent::ThreadsUpserted { .. }
            | UiEvent::ThreadCreated { .. }
            | UiEvent::ThreadDeleted { .. }
            | UiEvent::ThreadMembers { .. }
            | UiEvent::ForumPosts { .. }
            | UiEvent::ForumFirstMessages { .. }) => self.handle_thread_event(event),
        }
    }
}

impl eframe::App for VesktopApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_custom_idle(ctx);
        if ui::plugins::poll(self, ctx) {
            self.quitting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        while let Ok(command) = self.tray_rx.try_recv() {
            match command {
                TrayCommand::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayCommand::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                TrayCommand::Status(status) => {
                    let custom = self.settings.custom_status.clone();
                    self.set_own_status(status, custom);
                }
            }
        }
        if ctx.input(|input| input.viewport().close_requested())
            && !self.quitting
            && self.settings.minimize_to_tray
            && self.tray_available()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        self.poll_events(ctx);
        self.poll_fockytv();
        let (dj_room, dj_members) = match (&self.voice, &self.me) {
            (
                VoiceConn::Connected {
                    guild_id,
                    channel_id,
                },
                Some(me),
            ) => (
                Some((format!("{guild_id}:{channel_id}"), me.id.clone())),
                self.voice_states
                    .get(guild_id)
                    .into_iter()
                    .flat_map(|roster| roster.values())
                    .filter(|state| state.channel_id.as_ref() == Some(channel_id))
                    .map(|state| state.user_id.clone())
                    .collect(),
            ),
            _ => (None, Vec::new()),
        };
        self.extras
            .dj
            .configure(&self.handle, ctx, dj_room, dj_members);
        self.extras.dj.music.set_volume(if self.voice_deaf {
            0
        } else {
            self.settings.dj_volume
        });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let (Some(token), Some(me)) = (&self.settings.token, &self.me) {
            eframe::set_value(
                storage,
                STARTUP_CACHE_KEY,
                &StartupCache {
                    session: session_key(token),
                    me: me.clone(),
                    guilds: self.guilds.clone(),
                    dm_channels: self.dm_channels.clone(),
                    // Threads come back with READY; cached ones would go stale.
                    guild_channels: self
                        .guild_channels
                        .iter()
                        .map(|(id, channels)| {
                            let channels = channels.iter().filter(|c| !c.is_thread());
                            (id.clone(), channels.cloned().collect())
                        })
                        .collect(),
                    relationships: self.relationships.clone(),
                },
            );
        } else {
            storage.remove_string(STARTUP_CACHE_KEY);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.extras.dj.disconnect();
        self.finish_screen_share();
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("reduce_motion"), self.settings.reduce_motion)
        });
        if self.applied_theme != Some(self.settings.theme) {
            theme::apply(ctx, self.settings.theme);
            self.applied_theme = Some(self.settings.theme);
        }
        let target_scale =
            ctx.native_pixels_per_point().unwrap_or(1.0) * self.settings.zoom.max(0.25);
        if (ctx.pixels_per_point() - target_scale).abs() > 0.01 {
            ctx.set_pixels_per_point(target_scale);
        }

        if self.quitting {
            return;
        }

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

        // Alt+↑/↓ moves through channels when no settings modal is open.
        if !self.settings_open && ctx.input(|input| input.modifiers.alt) {
            if ctx.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                self.switch_channel(1);
            } else if ctx.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                self.switch_channel(-1);
            }
        }

        let update = self
            .update
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(update) = update {
            egui::Panel::top("update_banner").show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("FastDiscord {} foi baixado.", update.version));
                    if ui.button("Reiniciar para atualizar").clicked() {
                        crate::updater::restart(&update);
                        self.quitting = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        }

        // The user card floats over the bottom of both left columns, as in
        // Discord; each column stops above it.
        let card_margin = ui::channel_sidebar::CARD_MARGIN;
        let card_h = ui::channel_sidebar::card_height(self, 72.0 + 240.0 - 2.0 * card_margin);
        let reserved = card_h + 2.0 * card_margin;
        let rail = egui::Panel::left("server_rail")
            .exact_size(72.0)
            .resizable(false)
            .show(root, |ui| {
                ui::server_rail::paint(self, &mut above_card(ui, reserved));
            });
        let sidebar = egui::Panel::left("channel_sidebar")
            .exact_size(240.0)
            .resizable(false)
            .show(root, |ui| {
                ui::channel_sidebar::paint(self, &mut above_card(ui, reserved));
            });
        let bottom = sidebar.response.rect.bottom() - card_margin;
        let card = egui::Rect::from_min_max(
            egui::pos2(rail.response.rect.left() + card_margin, bottom - card_h),
            egui::pos2(sidebar.response.rect.right() - card_margin, bottom),
        );
        ui::channel_sidebar::paint_card(self, root, card);

        egui::CentralPanel::default().show(root, |ui| {
            if self.watch_docked() {
                ui::screen_share::paint_watch(self, ui);
            } else if self.selected_guild.is_none()
                && self.home_page != ui::friends::HomePage::DirectMessages
            {
                ui::friends::paint(self, ui);
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
        ui::screen_share::poll_watch(self, ctx);
        ui::screen_share::picker(self, ctx);
        if self.watch.is_some() && !self.watch_docked() {
            ui::screen_share::paint_floating(self, ctx);
        }
        if self.uses_fockytv() && matches!(self.voice, VoiceConn::Connected { .. }) {
            ctx.request_repaint_after(Duration::from_secs(5));
        }
        if matches!(self.conn, ConnState::Connecting) {
            ctx.request_repaint_after(Duration::from_millis(400));
        }
    }
}

/// A left column's content area, ending `reserved` px above the column's
/// bottom so nothing scrolls under the floating user card.
fn above_card(ui: &mut egui::Ui, reserved: f32) -> egui::Ui {
    ui.painter().rect_filled(ui.max_rect(), 0.0, theme::RAIL);
    let mut rect = ui.max_rect();
    rect.max.y -= reserved;
    ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    )
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
    #[test]
    fn tray_actions_work_without_painting_and_quit_does_not_hide_again() {
        let (_rt, mut app, _, _) = test_app();
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        app.tray_tx.send(TrayCommand::Show).unwrap();
        let output = ctx.run_logic(&egui::RawInput::default(), |ctx| {
            eframe::App::logic(&mut app, ctx, &mut frame)
        });
        let commands = &output.viewport_commands[&egui::ViewportId::ROOT];
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Visible(true)))
        );
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Focus))
        );
        app.tray_tx.send(TrayCommand::Quit).unwrap();
        let output = ctx.run_logic(&egui::RawInput::default(), |ctx| {
            eframe::App::logic(&mut app, ctx, &mut frame)
        });
        assert!(app.quitting);
        assert!(
            output.viewport_commands[&egui::ViewportId::ROOT]
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );
    }

    #[test]
    fn closing_without_a_tray_does_not_hide_the_window() {
        let (_rt, mut app, _, _) = test_app();
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let mut frame = eframe::Frame::_new_kittest();
        let output = ctx.run_logic(&input, |ctx| eframe::App::logic(&mut app, ctx, &mut frame));
        assert!(
            output
                .viewport_commands
                .values()
                .flatten()
                .all(|command| !matches!(
                    command,
                    egui::ViewportCommand::CancelClose | egui::ViewportCommand::Visible(false)
                ))
        );
    }

    #[test]
    fn settings_modal_stays_centered_and_fits_smaller_windows() {
        let (_rt, mut app, _, _) = test_app();
        let ctx = egui::Context::default();
        app.settings_open = true;
        for size in [egui::vec2(1280.0, 800.0), egui::vec2(480.0, 420.0)] {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            for tab in [
                ui::settings_window::SettingsTab::Account,
                ui::settings_window::SettingsTab::Appearance,
                ui::settings_window::SettingsTab::Voice,
                ui::settings_window::SettingsTab::Plugins,
                ui::settings_window::SettingsTab::Behavior,
            ] {
                app.settings_tab = tab;
                for _ in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(screen),
                            ..Default::default()
                        },
                        |root| {
                            ui::settings_window::show(&mut app, root.ctx());
                        },
                    );
                    output.textures_delta.clear();
                }
                let rect = ctx
                    .memory(|memory| memory.area_rect(egui::Id::new("settings_modal")))
                    .unwrap();
                assert!(rect.center().distance(screen.center()) <= 1.0, "{rect:?}");
                assert!(screen.contains_rect(rect), "{tab:?}: {rect:?}");
                assert!(app.settings_open);
            }
        }
    }

    #[test]
    fn dj_player_fits_sidebar_and_playlist_is_centered() {
        let (_rt, mut app, _, _) = test_app();
        app.voice = VoiceConn::Connected {
            guild_id: "1".into(),
            channel_id: "2".into(),
        };
        let mut session = crate::backend::dj::Session::default();
        session
            .apply(
                crate::backend::dj::Action::Add(crate::backend::dj::Track {
                    video_id: "abcdefghijk".into(),
                    title: "A very long title ".repeat(25),
                    duration_ms: 180_000,
                }),
                1000,
            )
            .unwrap();
        session.participants = 12;
        {
            let mut view = app.extras.dj.view.lock().unwrap();
            view.session = Some(session);
            view.connected = true;
            view.clock = Some(crate::backend::dj::Clock::new(1000));
        }
        let ctx = Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 420.0));
        for collapsed in [false, true] {
            app.extras.dj_collapsed = collapsed;
            let expected = ui::voice_extras::music_height(&app);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |root| {
                    let response = root.allocate_ui_with_layout(
                        egui::vec2(242.0, expected),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| ui::voice_extras::music_panel(&mut app, ui),
                    );
                    assert!(
                        response.response.rect.height() <= expected + 1.0,
                        "{:?}",
                        response.response.rect
                    );
                    assert!(
                        response.response.rect.width() <= 243.0,
                        "{:?}",
                        response.response.rect
                    );
                },
            );
            output.textures_delta.clear();
        }
        app.extras.music_open = true;
        app.extras.dj.view.lock().unwrap().search_results = (0..10)
            .map(|_| crate::backend::dj::Track {
                video_id: "abcdefghijk".into(),
                title: "Long search result ".repeat(30),
                duration_ms: 180_000,
            })
            .collect();
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |root| ui::voice_extras::show(&mut app, root.ctx()),
            );
            output.textures_delta.clear();
        }
        let rect = ctx
            .memory(|memory| memory.area_rect(egui::Id::new("dj_playlist")))
            .unwrap();
        assert!(screen.contains_rect(rect), "{rect:?}");
        assert!(rect.center().distance(screen.center()) <= 1.0, "{rect:?}");
    }

    #[test]
    fn startup_cache_is_scoped_to_the_session_and_ignores_corruption() {
        #[derive(Default)]
        struct Storage(std::collections::HashMap<String, String>);
        impl eframe::Storage for Storage {
            fn get_string(&self, key: &str) -> Option<String> {
                self.0.get(key).cloned()
            }
            fn set_string(&mut self, key: &str, value: String) {
                self.0.insert(key.into(), value);
            }
            fn remove_string(&mut self, key: &str) {
                self.0.remove(key);
            }
            fn flush(&mut self) {}
        }
        let mut storage = Storage::default();
        assert!(load_startup_cache(&storage, "session-a").is_none());
        let cache = StartupCache {
            session: session_key("session-a"),
            me: serde_json::from_value(serde_json::json!({"id": "1", "username": "cached"}))
                .unwrap(),
            guilds: vec![crate::model::Guild {
                id: "2".into(),
                name: "Server".into(),
                icon: None,
            }],
            dm_channels: Vec::new(),
            guild_channels: std::collections::HashMap::new(),
            relationships: Vec::new(),
        };
        eframe::set_value(&mut storage, STARTUP_CACHE_KEY, &cache);
        let restored = load_startup_cache(&storage, "session-a").unwrap();
        assert_eq!(restored.me.id, "1");
        assert_eq!(restored.guilds[0].name, "Server");
        assert!(load_startup_cache(&storage, "session-b").is_none());
        assert!(!storage.0[STARTUP_CACHE_KEY].contains("session-a"));
        eframe::Storage::set_string(&mut storage, STARTUP_CACHE_KEY, "broken".into());
        assert!(load_startup_cache(&storage, "session-a").is_none());
        let (_rt, mut app, _, _) = test_app();
        app.settings.token = Some("session-a".into());
        app.me = Some(cache.me);
        app.guilds = cache.guilds;
        eframe::App::save(&mut app, &mut storage);
        assert!(load_startup_cache(&storage, "session-a").is_some());
        app.settings.token = None;
        eframe::App::save(&mut app, &mut storage);
        assert!(!storage.0.contains_key(STARTUP_CACHE_KEY));
    }

    #[test]
    fn refresh_discards_channels_of_removed_cached_guilds() {
        let (_rt, mut app, mut commands, _) = test_app();
        app.selected_guild = Some("old-guild".into());
        app.selected_channel = Some("old-channel".into());
        app.channel_index
            .insert("old-channel".into(), ChannelRef::Guild("old-guild".into()));
        app.guild_channels.insert("old-guild".into(), Vec::new());
        app.handle_event(UiEvent::GuildsLoaded { guilds: Vec::new() }, true);
        assert!(app.selected_guild.is_none());
        assert!(app.selected_channel.is_none());
        assert!(app.guild_channels.is_empty());
        assert!(!app.channel_index.contains_key("old-channel"));
        assert!(commands.try_recv().is_err());
    }

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
    fn reactions_flip_optimistically_and_swallow_our_echo() {
        let (_rt, mut app, mut commands, _) = test_app();
        app.me = Some(
            serde_json::from_value(serde_json::json!({ "id": "me", "username": "eu" })).unwrap(),
        );
        app.messages
            .insert("channel".into(), vec![sample_message(1)]);
        let thumbs = || crate::model::ReactionEmoji {
            id: None,
            name: Some("👍".into()),
            animated: false,
        };
        let echo = |app: &mut VesktopApp, user: &str, added| {
            app.handle_event(
                UiEvent::ReactionChanged {
                    channel_id: "channel".into(),
                    message_id: "1".into(),
                    user_id: user.into(),
                    emoji: thumbs(),
                    added,
                },
                true,
            )
        };
        let chip = |app: &VesktopApp| {
            app.messages["channel"][0]
                .reactions
                .first()
                .map(|r| (r.count, r.me))
        };

        echo(&mut app, "other", true);
        assert_eq!(chip(&app), Some((1, false)));
        // Our click counts once, the gateway's echo of it doesn't.
        app.toggle_reaction("channel", "1", thumbs(), true);
        assert!(
            matches!(commands.try_recv().unwrap(), Command::ToggleReaction { emoji, add: true, .. } if emoji == "👍")
        );
        assert_eq!(chip(&app), Some((2, true)));
        echo(&mut app, "me", true);
        assert_eq!(chip(&app), Some((2, true)));
        app.toggle_reaction("channel", "1", thumbs(), false);
        echo(&mut app, "me", false);
        assert_eq!(chip(&app), Some((1, false)));
        // The last reactor leaving drops the chip.
        echo(&mut app, "other", false);
        assert_eq!(chip(&app), None);
    }

    #[test]
    fn read_state_seeds_markers_and_session_reads_win() {
        let (_rt, mut app, _, _) = test_app();
        app.handle_event(
            UiEvent::ReadState {
                markers: vec![("channel".into(), "100".into())],
            },
            true,
        );
        assert_eq!(app.last_seen.get("channel").unwrap(), "100");
        // The READY snapshot must not clobber what was read this session.
        app.mark_channel_read("channel", "200".into());
        app.handle_event(
            UiEvent::ReadState {
                markers: vec![("channel".into(), "50".into())],
            },
            true,
        );
        assert_eq!(app.last_seen.get("channel").unwrap(), "200");
    }

    #[test]
    fn opening_a_channel_behind_the_present_seeds_the_unread_divider() {
        let (_rt, mut app, _, _) = test_app();
        app.selected_channel = Some("channel".into());
        app.handle_event(
            UiEvent::ReadState {
                markers: vec![("channel".into(), "40".into())],
            },
            true,
        );
        app.handle_event(
            UiEvent::MessagesLoaded {
                channel_id: "channel".into(),
                messages: (30..=50).map(sample_message).collect(),
                older: false,
                newer: false,
            },
            true,
        );
        // The divider sits at the first message after the read marker and
        // the view lands on it.
        assert_eq!(app.first_unread.get("channel").unwrap(), "41");
        assert!(app.unread_jump.contains("channel"));

        // Reloading a page at the present seeds nothing.
        app.mark_channel_read("channel", "60".into());
        app.handle_event(
            UiEvent::MessagesLoaded {
                channel_id: "channel".into(),
                messages: (51..=60).map(sample_message).collect(),
                older: false,
                newer: false,
            },
            true,
        );
        assert!(app.first_unread.get("channel").is_none());
        assert!(!app.unread_jump.contains("channel"));
    }

    #[test]
    fn reaching_the_present_clears_the_unread_divider() {
        let (_rt, mut app, _, _) = test_app();
        app.first_unread.insert("channel".into(), "41".into());
        app.unread_jump.insert("channel".into());
        app.mark_channel_read("channel", "50".into());
        assert!(app.first_unread.get("channel").is_none());
        assert!(!app.unread_jump.contains("channel"));
    }

    #[test]
    fn favorite_mute_and_leave_manage_guilds_from_the_rail() {
        let (_rt, mut app, mut commands, _) = test_app();
        app.guilds = vec![
            crate::model::Guild {
                id: "1".into(),
                name: "Primeiro".into(),
                icon: None,
            },
            crate::model::Guild {
                id: "2".into(),
                name: "Segundo".into(),
                icon: None,
            },
        ];
        app.channel_index
            .insert("10".into(), ChannelRef::Guild("1".into()));
        app.unread.insert("10".into(), 3);
        app.mentions.insert("10".into(), 1);

        // Favorites flip in the settings…
        app.toggle_favorite_guild("2");
        assert!(app.settings.favorite_guilds.contains("2"));
        app.toggle_favorite_guild("2");
        assert!(!app.settings.favorite_guilds.contains("2"));

        // …and muting rides to Discord as a REST command.
        app.toggle_guild_mute("1");
        assert!(app.settings.muted_guilds.contains("1"));
        assert!(
            matches!(commands.try_recv().unwrap(), Command::SetGuildMute { guild_id, muted: true } if guild_id == "1")
        );
        app.toggle_guild_mute("1");
        assert!(!app.settings.muted_guilds.contains("1"));
        assert!(
            matches!(commands.try_recv().unwrap(), Command::SetGuildMute { guild_id, muted: false } if guild_id == "1")
        );

        // Leaving the selected guild drops its state and falls back to
        // the next guild in the rail.
        app.selected_guild = Some("1".into());
        app.leave_guild("1");
        assert!(
            matches!(commands.try_recv().unwrap(), Command::LeaveGuild { guild_id } if guild_id == "1")
        );
        app.handle_event(
            UiEvent::GuildLeft {
                guild_id: "1".into(),
            },
            true,
        );
        assert!(app.guilds.iter().all(|guild| guild.id != "1"));
        assert!(app.channel_index.get("10").is_none());
        assert!(app.unread.get("10").is_none());
        assert!(app.mentions.get("10").is_none());
        assert_eq!(app.selected_guild.as_deref(), Some("2"));
    }

    #[test]
    fn custom_idle_appears_idle_after_silence_and_restores_on_activity() {
        let (_rt, mut app, mut commands, _) = test_app();
        let ctx = egui::Context::default();
        app.me = Some(
            serde_json::from_value(serde_json::json!({ "id": "9", "username": "member" })).unwrap(),
        );
        app.settings.idle_timeout_minutes = 10;
        let idle_frame = |app: &mut VesktopApp, ctx: &egui::Context| {
            ctx.run_ui(egui::RawInput::default(), |_| {})
                .textures_delta
                .clear();
            app.poll_custom_idle(ctx);
        };

        // Long silence: we auto-idle without saving the status.
        app.last_input = Instant::now() - Duration::from_secs(11 * 60);
        idle_frame(&mut app, &ctx);
        assert!(app.auto_idle);
        assert_eq!(app.presence.get("9").map(String::as_str), Some("idle"));
        assert!(
            matches!(commands.try_recv().unwrap(), Command::SetPresence { status, save: false, .. } if status == "idle")
        );

        // Any input restores the chosen status exactly once.
        ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::PointerMoved(egui::pos2(4.0, 4.0))],
                ..Default::default()
            },
            |_| {},
        )
        .textures_delta
        .clear();
        app.poll_custom_idle(&ctx);
        assert!(!app.auto_idle);
        assert_eq!(app.presence.get("9").map(String::as_str), Some("online"));
        assert!(
            matches!(commands.try_recv().unwrap(), Command::SetPresence { status, save: false, .. } if status == "online")
        );

        // An explicit "Não perturbe" is never overridden.
        app.set_own_status("dnd", Default::default());
        let _ = commands.try_recv();
        app.last_input = Instant::now() - Duration::from_secs(11 * 60);
        idle_frame(&mut app, &ctx);
        assert!(!app.auto_idle);
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn stream_stats_show_resolution_and_fps_while_hovering() {
        let (_rt, mut app, _, _) = test_app();
        let ctx = egui::Context::default();
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 360.0));
        assert!(ui::screen_share::stream_stats_text(&app, &ctx).is_none());
        assert!(!ui::screen_share::stream_stats_visible(&app, &ctx, canvas));

        app.watch = Some(GoLive {
            key: "stream".into(),
            rtc: None,
            task: None,
            frames: Default::default(),
            fockytv: None,
            started: Instant::now(),
        });
        app.watch_texture = Some(ctx.load_texture(
            "watch",
            egui::ColorImage::new([320, 180], vec![egui::Color32::BLACK; 320 * 180]),
            egui::TextureOptions::LINEAR,
        ));
        // No frame counter yet: only the resolution shows.
        assert_eq!(
            ui::screen_share::stream_stats_text(&app, &ctx).unwrap(),
            "320×180"
        );
        // poll_watch's counter: 60 frames over the last 2 s.
        let watch = app.watch.as_ref().unwrap();
        let fps_id = egui::Id::new(("watch_fps", &watch.key, watch.started));
        ctx.data_mut(|data| {
            data.insert_temp(fps_id, (Instant::now() - Duration::from_secs(2), 60u64));
        });
        assert_eq!(
            ui::screen_share::stream_stats_text(&app, &ctx).unwrap(),
            "320×180 · 30 fps"
        );

        // Stats stay hidden until the canvas is hovered…
        run_pointer_frame(&ctx, egui::pos2(1200.0, 700.0));
        assert!(!ui::screen_share::stream_stats_visible(&app, &ctx, canvas));
        // …visible while hovering…
        run_pointer_frame(&ctx, egui::pos2(320.0, 180.0));
        assert!(ui::screen_share::stream_stats_visible(&app, &ctx, canvas));
        // …and held a few seconds after the pointer leaves.
        run_pointer_frame(&ctx, egui::pos2(1200.0, 700.0));
        assert!(ui::screen_share::stream_stats_visible(&app, &ctx, canvas));
        let hover_id = egui::Id::new(("watch_stats_hover", &watch.key, watch.started));
        ctx.data_mut(|data| {
            data.insert_temp(hover_id, Instant::now() - ui::screen_share::STATS_HOLD);
        });
        assert!(!ui::screen_share::stream_stats_visible(&app, &ctx, canvas));
    }

    fn run_pointer_frame(ctx: &egui::Context, pos: egui::Pos2) {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                events: vec![egui::Event::PointerMoved(pos)],
                ..Default::default()
            },
            |_| {},
        )
        .textures_delta
        .clear();
    }

    #[test]
    fn invalid_share_nick_opens_settings_before_capture() {
        let (_rt, mut app, mut commands, _) = test_app();
        app.me = Some(
            serde_json::from_value(serde_json::json!({"id":"9", "username":"member"})).unwrap(),
        );
        app.voice = VoiceConn::Connected {
            guild_id: "guild".into(),
            channel_id: "channel".into(),
        };
        app.settings.plugins.fockytv.nick = Some("Discord RUST".into());
        app.toggle_screen_share();
        assert!(app.screen.is_none());
        assert!(app.fockytv.is_none());
        assert!(app.settings_open);
        assert_eq!(app.settings_tab, ui::settings_window::SettingsTab::Plugins);
        assert!(commands.try_recv().is_err());
        app.settings.plugins.fockytv.nick = Some(" Discord-RUST ".into());
        assert_eq!(app.fockytv_nick(), "Discord-RUST");
        // A valid nick opens the picker; nothing is captured until it's
        // confirmed.
        app.settings_open = false;
        app.toggle_screen_share();
        assert!(app.share_picker.is_some());
        assert!(app.screen.is_none());
        assert!(!app.settings_open);
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

    #[test]
    fn reply_jump_fetches_around_detaches_old_slices_and_ignores_deleted() {
        let (_rt, mut app, mut commands, _) = test_app();
        app.messages
            .insert("channel".into(), (100..=105).map(sample_message).collect());
        app.jump_to_message("channel", "103".into());
        assert!(commands.try_recv().is_err());

        app.jump_to_message("channel", "20".into());
        assert!(
            matches!(commands.try_recv().unwrap(), Command::LoadMessagesAround { message_id, .. } if message_id == "20")
        );
        let around = |ids: std::ops::RangeInclusive<u64>| UiEvent::MessagesAround {
            channel_id: "channel".into(),
            message_id: "20".into(),
            messages: ids.map(sample_message).collect(),
        };
        // Deleted: the page around it lacks it, nothing changes.
        app.handle_event(around(21..=30), true);
        assert!(app.reply_jump.is_none());
        assert_eq!(app.messages["channel"].len(), 6);

        // Far older: the slice replaces the cache, cut off from live messages.
        app.jump_to_message("channel", "20".into());
        assert!(commands.try_recv().is_ok());
        app.handle_event(around(10..=30), true);
        assert!(app.detached.contains("channel"));
        assert_eq!(app.messages["channel"].first().unwrap().id, "10");
        app.handle_event(
            UiEvent::MessageCreated {
                message: sample_message(200),
            },
            true,
        );
        assert_eq!(app.messages["channel"].last().unwrap().id, "30");

        app.return_to_present("channel");
        assert!(!app.detached.contains("channel") && !app.messages.contains_key("channel"));
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::LoadMessages {
                before: None,
                after: None,
                ..
            }
        ));
    }

    fn test_channel(id: &str, kind: i64, extra: serde_json::Value) -> Channel {
        let mut value = serde_json::json!({ "id": id, "type": kind, "name": id, "guild_id": "g" });
        let extra = match extra {
            serde_json::Value::Null => serde_json::json!({}),
            extra => extra,
        };
        if let (Some(base), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            base.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        serde_json::from_value(value).unwrap()
    }

    fn role(id: &str, permissions: u64) -> crate::member_list::Role {
        serde_json::from_value(serde_json::json!({ "id": id, "name": id, "permissions": permissions.to_string() }))
            .unwrap()
    }

    /// Issue #55: the sidebar lock follows Discord's permission algorithm.
    #[test]
    fn channel_lock_follows_discord_permission_rules() {
        const VIEW: u64 = crate::model::PERMISSION_VIEW_CHANNEL;
        const ADMIN: u64 = crate::model::PERMISSION_ADMINISTRATOR;
        const CONNECT: u64 = 1 << 20;
        let (_rt, mut app, _, _) = test_app();
        app.me = Some(
            serde_json::from_value(serde_json::json!({ "id": "me", "username": "eu" })).unwrap(),
        );
        // @everyone can't view; role "10" can; role "11" can connect.
        app.members.roles.insert(
            "g".into(),
            [
                role("g", CONNECT),
                role("10", VIEW),
                role("11", CONNECT | ADMIN),
            ]
            .into_iter()
            .map(|role| (role.id.clone(), role))
            .collect(),
        );
        app.own_roles.insert("g".into(), vec!["10".into()]);
        app.channel_index.insert("c".into(), ChannelRef::Guild("g".into()));
        app.channel_index
            .insert("cat".into(), ChannelRef::Guild("g".into()));
        // Missing role data fails open: no lock.
        assert!(!app.channel_hidden(&test_channel("c", 0, serde_json::json!({})), "g"));

        // A role overwrite denying VIEW hides, the member overwrite wins back.
        let deny_role = serde_json::json!({ "permission_overwrites": [
            { "id": "10", "type": 0, "allow": 0, "deny": VIEW }
        ] });
        assert!(app.channel_hidden(&test_channel("c", 0, deny_role.clone()), "g"));
        let member_saves = serde_json::json!({ "permission_overwrites": [
            { "id": "10", "type": 0, "allow": 0, "deny": VIEW },
            { "id": "me", "type": 1, "allow": VIEW, "deny": 0 }
        ] });
        assert!(!app.channel_hidden(&test_channel("c", 0, member_saves), "g"));

        // Synced with the category: the parent's overwrites apply, unless
        // the channel carries its own.
        app.guild_channels.insert(
            "g".into(),
            vec![
                test_channel("cat", crate::model::CHANNEL_KIND_CATEGORY, deny_role.clone()),
                test_channel("c", 0, serde_json::json!({})),
                test_channel("open", 0, serde_json::json!({ "permission_overwrites": [
                    { "id": "g", "type": 0, "allow": VIEW, "deny": 0 }
                ] })),
            ],
        );
        app.channel_index.insert("open".into(), ChannelRef::Guild("g".into()));
        let synced = test_channel("c", 0, serde_json::json!({ "parent_id": "cat" }));
        assert!(app.channel_hidden(&synced, "g"));
        let detached = test_channel(
            "c",
            0,
            serde_json::json!({ "parent_id": "cat", "permission_overwrites": [
                { "id": "g", "type": 0, "allow": VIEW, "deny": 0 }
            ] }),
        );
        assert!(!app.channel_hidden(&detached, "g"));

        // Administrator rides over every deny.
        app.own_roles.insert("g".into(), vec!["11".into()]);
        assert!(!app.channel_hidden(&test_channel("c", 0, deny_role), "g"));

        // A private thread we're not in always locks; joined ones don't.
        let private = |joined: bool| {
            let extra = joined
                .then(|| serde_json::json!({ "member": { "id": "t" } }))
                .unwrap_or(serde_json::json!({}));
            test_channel("t", crate::model::CHANNEL_KIND_PRIVATE_THREAD, extra)
        };
        assert!(app.channel_hidden(&private(false), "g"));
        assert!(!app.channel_hidden(&private(true), "g"));
    }

    /// Issue #54: "NOVO" marks channels created after the guild was joined
    /// and clears once the channel is opened.
    #[test]
    fn new_tag_tracks_join_date_and_visits() {
        let (_rt, mut app, _, _) = test_app();
        // Joined 2026-01-01; the test channels encode 2024-01-01 and the
        // join instant itself.
        app.guild_joined_at
            .insert("g".into(), 1_767_225_600_000);
        let channel_at = |ms: i64| ((ms - 1_420_070_400_000_i64) << 22).to_string();
        let old_channel = channel_at(1_704_067_200_000);
        assert!(!app.channel_is_new(&old_channel, "g"));
        let new_channel = channel_at(1_767_225_601_000);
        assert!(app.channel_is_new(&new_channel, "g"));
        app.select_channel(new_channel.clone());
        assert!(!app.channel_is_new(&new_channel, "g"));
        // No join date known: never new.
        assert!(!app.channel_is_new(&new_channel, "other"));
    }

    /// Issue #36: hovering a server prefetched its channels once — not
    /// again while loading, and not for one already loaded.
    #[test]
    fn guild_hover_prefetches_channels_exactly_once() {
        let (_rt, mut app, mut commands, _) = test_app();
        app.prefetch_guild_channels("g");
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::LoadGuildChannels { ref guild_id } if guild_id == "g"
        ));
        // Still in flight: the repeated hover sends nothing.
        app.prefetch_guild_channels("g");
        assert!(commands.try_recv().is_err());
        // Loaded: no refetch from hover either.
        app.handle_event(
            UiEvent::GuildChannelsLoaded {
                guild_id: "g".into(),
                channels: Vec::new(),
            },
            true,
        );
        app.prefetch_guild_channels("g");
        assert!(commands.try_recv().is_err());
    }
}
