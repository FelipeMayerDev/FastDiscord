//! Typed messages between the UI thread and the backend task.

use crate::model::{Channel, Embed, Gif, Guild, GuildEmoji, Message, User, VoiceState};

/// Sends [`UiEvent`]s to the UI thread and wakes egui's loop right after.
/// The loop only redraws on input otherwise, so background events (guild
/// lists, messages, typing) would sit in the channel until the next mouse
/// move. `egui::Context` is `Send + Sync` and its `request_repaint` is
/// wired by eframe to the native event loop.
#[derive(Clone)]
pub struct EventTx {
    tx: tokio::sync::mpsc::UnboundedSender<UiEvent>,
    ctx: egui::Context,
}

impl EventTx {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<UiEvent>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    /// Wakes the UI without an event (new video frame).
    pub fn repaint(&self) {
        self.ctx.request_repaint();
    }

    pub fn send(&self, event: UiEvent) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }
}

/// Requests from the UI thread to the backend task.
#[derive(Debug, Clone)]
pub enum Command {
    LoadGuilds,
    LoadDmChannels,
    LoadRelationships,
    RequestFriend {
        username: String,
    },
    AcceptFriend {
        user_id: String,
    },
    RemoveRelationship {
        user_id: String,
    },
    /// PATCH /users/@me/guilds/{id}: mute (or unmute) the guild, so
    /// @everyone/@here stop pinging and notifications quiet down.
    SetGuildMute {
        guild_id: String,
        muted: bool,
    },
    /// DELETE /users/@me/guilds/{id}: leave the guild.
    LeaveGuild {
        guild_id: String,
    },
    OpenDm {
        user_id: String,
    },
    LoadGuildChannels {
        guild_id: String,
    },
    /// A page of history: the latest (no cursor), older than `before`, or
    /// newer than `after` (catching a cached channel up after a reconnect).
    LoadMessages {
        channel_id: String,
        before: Option<String>,
        after: Option<String>,
    },
    SendMessage {
        channel_id: String,
        content: String,
    },
    /// Edit or delete one of our own messages.
    EditMessage {
        channel_id: String,
        message_id: String,
        content: String,
    },
    DeleteMessage {
        channel_id: String,
        message_id: String,
    },
    /// React (or unreact) as ourselves; the change comes back through the
    /// gateway's MESSAGE_REACTION echo. The emoji key is `name` for
    /// Unicode and `name:id` for custom ones.
    ToggleReaction {
        channel_id: String,
        message_id: String,
        emoji: String,
        add: bool,
    },
    /// Who reacted with an emoji, for the chip's tooltip.
    FetchReactors {
        channel_id: String,
        message_id: String,
        emoji: String,
    },
    /// Signal the user is typing, at most once every few seconds.
    SendTyping {
        channel_id: String,
    },
    /// op 3 with our status and custom status; `save` also stores them in
    /// the account settings so the other clients follow.
    SetPresence {
        status: String,
        custom_status: crate::settings::CustomStatus,
        save: bool,
    },
    /// Join a guild voice channel: op 4 over the live socket. Re-sent with
    /// the same channel and new flags for mute/deafen toggles.
    JoinVoice {
        guild_id: String,
        channel_id: String,
        self_mute: bool,
        self_deaf: bool,
    },
    LeaveVoice {
        guild_id: String,
    },
    /// Go Live (docs/SCREENSHARE.md): op 18 CREATE_STREAM + op 22 unpause,
    /// for the voice channel we're connected to.
    StartStream {
        guild_id: String,
        channel_id: String,
        stream_key: String,
    },
    /// op 20 WATCH_STREAM: join someone's stream as a viewer.
    WatchStream {
        stream_key: String,
    },
    /// op 19 DELETE_STREAM: ends our stream, or leaves one we watch.
    StopStream {
        stream_key: String,
    },
    /// Subscribe to guild events and request the listed members' presences.
    SubscribeGuild {
        guild_id: String,
        user_ids: Vec<String>,
    },
    /// The soundboard picker opened in a guild.
    LoadSoundboard {
        guild_id: String,
    },
    SendSoundboard {
        channel_id: String,
        sound_id: String,
        source_guild_id: Option<String>,
    },
    /// GIF picker: trending for an empty query, search otherwise.
    SearchGifs {
        query: String,
    },
    /// Fetch a guild member (name/avatar) for a voice state that only
    /// carries a user id, as in READY.
    LoadVoiceUser {
        guild_id: String,
        user_id: String,
    },
    /// Our own member in a guild, for its role list — the input to the
    /// channel permission math behind the sidebar's lock icons.
    LoadOwnMember {
        guild_id: String,
        user_id: String,
    },
    /// Another user's profile with mutuals, for the DM profile card.
    LoadDmProfile {
        user_id: String,
    },
    /// Confirm the channel read up to `message_id` (REST ack; the gateway
    /// op 13 with the returned version token goes out from the backend).
    AckChannel {
        channel_id: String,
        message_id: String,
    },
    /// The thread browser or a forum opened: its archived public threads.
    LoadThreads {
        channel_id: String,
    },
    /// A page of a forum's archived posts, from `offset`.
    SearchForumPosts {
        forum_id: String,
        sort_by: &'static str,
        offset: usize,
    },
    /// First messages of a forum's active posts.
    LoadForumPostData {
        forum_id: String,
        thread_ids: Vec<String>,
    },
    /// Start a thread in `channel_id`: from `message_id` when set, else a
    /// standalone one (`kind` 10/11) or, in a forum, a post whose first
    /// message is `content`. Outside forums `content` follows as the
    /// thread's first message.
    CreateThread {
        channel_id: String,
        message_id: Option<String>,
        name: String,
        content: String,
        kind: i64,
        forum: bool,
    },
    /// Op 37: rows `0..ranges * 100` of a channel's member list.
    SubscribeMemberList {
        guild_id: String,
        channel_id: String,
        ranges: usize,
    },
    /// The history page around a replied-to message that isn't loaded.
    LoadMessagesAround {
        channel_id: String,
        message_id: String,
    },
}

/// Updates from the backend task to the UI thread.
#[derive(Debug)]
pub enum UiEvent {
    Connected,
    Ready {
        user: User,
    },
    Disconnected {
        reason: String,
        retry_in: Option<u64>,
    },
    TokenInvalid,
    GuildsLoaded {
        guilds: Vec<Guild>,
    },
    RelationshipsLoaded {
        relationships: Vec<crate::model::Relationship>,
    },
    RelationshipsChanged,
    RelationshipError {
        error: String,
    },
    /// The REST call to leave a guild succeeded; drop its local state.
    GuildLeft {
        guild_id: String,
    },
    DmOpened {
        channel: Channel,
    },
    DmChannelsLoaded {
        channels: Vec<Channel>,
    },
    GuildChannelsLoaded {
        guild_id: String,
        channels: Vec<Channel>,
    },
    MessagesLoaded {
        channel_id: String,
        messages: Vec<Message>,
        older: bool,
        newer: bool,
    },
    /// Discord's server-side read markers from READY (partial: only
    /// channels Discord tracks), as (channel id, last read message id).
    /// Seeds the red "NOVAS" divider for channels opened behind the
    /// present.
    ReadState {
        markers: Vec<(String, String)>,
    },
    /// A message fetch failed; frees the channel to be fetched again.
    MessagesFailed {
        channel_id: String,
    },
    AttachmentPickerClosed {
        channel_id: String,
        path: Option<std::path::PathBuf>,
        error: Option<String>,
    },
    AttachmentSent {
        message: Message,
        content: String,
    },
    MessageCreated {
        message: Message,
    },
    /// An edit, or Discord unfurling links: embeds (a Tenor link's GIF)
    /// arrive in a MESSAGE_UPDATE after the message itself.
    MessageUpdated {
        channel_id: String,
        message_id: String,
        content: Option<String>,
        embeds: Option<Vec<Embed>>,
    },
    MessageDeleted {
        channel_id: String,
        message_id: String,
    },
    /// A reaction echoed by the gateway, on a loaded message — ours after
    /// the REST toggle, or anyone else's.
    ReactionChanged {
        channel_id: String,
        message_id: String,
        user_id: String,
        emoji: crate::model::ReactionEmoji,
        added: bool,
    },
    Reactors {
        message_id: String,
        emoji: String,
        users: Vec<crate::model::User>,
    },
    SendFailed {
        channel_id: String,
        error: String,
    },
    TypingStart {
        channel_id: String,
        user_id: String,
    },
    PresenceUpdate {
        user_id: String,
        status: String,
        /// The platform behind the presence, when any client is online:
        /// "mobile" when the phone is the only client, "desktop" for
        /// web/desktop. The status dot becomes a phone for the former.
        device: Option<String>,
    },
    /// A guild's full voice roster, from READY or GUILD_CREATE (empty on
    /// GUILD_DELETE), replacing whatever the UI had.
    GuildVoiceStates {
        guild_id: String,
        states: Vec<VoiceState>,
    },
    /// Any user's voice state changed (who is in which channel).
    VoiceStateUpdate {
        state: VoiceState,
    },
    SoundboardLoaded {
        guild_id: String,
        sounds: Vec<crate::backend::soundboard::SoundboardSound>,
    },
    SoundboardError {
        guild_id: Option<String>,
        error: String,
    },
    /// Someone (us included) played a soundboard sound in our channel.
    VoiceEffect {
        user_id: String,
        sound_id: String,
        volume: f32,
    },
    /// A soundboard sound started playing: its sender lights up as
    /// speaking for `duration`.
    SoundboardPlaying {
        user_id: String,
        sound_id: String,
        duration: std::time::Duration,
    },
    GifsLoaded {
        query: String,
        result: Result<Vec<Gif>, String>,
    },
    /// A guild's custom emojis, replacing what the UI had.
    GuildEmojis {
        guild_id: String,
        emojis: Vec<GuildEmoji>,
    },
    /// A member lookup resolved to a user, for the voice roster.
    UserResolved {
        user: User,
    },
    /// The driver finished the voice handshake (DAVE included).
    VoiceConnected {
        guild_id: String,
        channel_id: String,
    },
    VoiceFailed {
        error: String,
    },
    VoiceLeft,
    /// Connection quality every few seconds while in voice: voice-gateway
    /// ping, incoming packet loss (0–1; `None` with no audio received) and
    /// the voice server.
    VoiceStats {
        ping_ms: Option<u32>,
        loss: Option<f32>,
        endpoint: String,
    },
    /// A user started or stopped producing audio (speaking indicator).
    VoiceSpeaking {
        user_id: String,
        speaking: bool,
    },
    /// FockyTV `/api/status`: the stream keys live right now.
    FockyLive {
        keys: Vec<String>,
    },
    /// Discord allocated a stream server for a Go Live stream.
    StreamCreated {
        stream_key: String,
        rtc_server_id: String,
        rtc_channel_id: String,
    },
    /// The stream server to connect to; `endpoint: None` = it went away.
    StreamServer {
        stream_key: String,
        endpoint: Option<String>,
        token: String,
        session_id: String,
    },
    /// A stream ended server-side (`reason`: user_requested, stream_full,
    /// unauthorized, …).
    StreamDeleted {
        stream_key: String,
        reason: String,
    },
    /// An unmapped SSRC kept speaking for seconds: the voice session lost
    /// sync (DAVE re-key, reconnect) and needs a fresh handshake.
    VoiceSuspectDesync,
    Error {
        context: String,
    },
    /// QR login: a code is ready to scan.
    QrReady {
        url: String,
    },
    /// QR login: scanned, waiting for approval on the phone. The user info
    /// comes from `pending_ticket` (id:discriminator:avatar:username).
    QrScanned {
        username: String,
        user_id: String,
        avatar: Option<String>,
    },
    QrLogin {
        token: String,
    },
    /// QR login: the code expired; the UI regenerates it silently.
    QrExpired,
    QrFailed {
        reason: String,
    },
    Profile(ProfileEvent),
    /// The guild's active threads, from READY, GUILD_CREATE or
    /// THREAD_LIST_SYNC: they replace the active threads of `parent_ids`
    /// (the whole guild when `None`).
    ThreadsSynced {
        guild_id: String,
        parent_ids: Option<Vec<String>>,
        threads: Vec<Channel>,
    },
    /// THREAD_CREATE/THREAD_UPDATE, or a fetched archive page.
    ThreadsUpserted {
        threads: Vec<Channel>,
    },
    /// A page of forum posts (`SearchForumPosts`) for `sort_by`; empty and
    /// final when the fetch failed.
    ForumPosts {
        forum_id: String,
        sort_by: &'static str,
        threads: Vec<Channel>,
        first_messages: Vec<Message>,
        has_more: bool,
    },
    /// First messages of forum posts (`LoadForumPostData`).
    ForumFirstMessages {
        messages: Vec<Message>,
    },
    /// A thread we created: registered and opened.
    ThreadCreated {
        thread: Channel,
    },
    ThreadDeleted {
        thread_id: String,
    },
    /// Users joined or left a thread (THREAD_MEMBER(S)_UPDATE).
    ThreadMembers {
        thread_id: String,
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// A guild's roles, from READY or GUILD_CREATE (member list groups).
    GuildRoles {
        guild_id: String,
        roles: Vec<crate::member_list::Role>,
    },
    /// Our own member in a guild (`LoadOwnMember`): just the role ids.
    OwnMemberLoaded {
        guild_id: String,
        roles: Vec<String>,
    },
    /// When we joined a guild (READY, GUILD_CREATE), in ms: channels and
    /// threads created after it wear the sidebar's "NOVO" tag.
    GuildJoinedAt {
        guild_id: String,
        joined_at_ms: i64,
    },
    /// A DM partner's profile (`LoadDmProfile`); `None` when the fetch
    /// failed, so the card shows its placeholder instead of retry-looping.
    DmProfileLoaded {
        user_id: String,
        profile: Option<crate::model::UserProfile>,
    },
    /// MESSAGE_ACK from the gateway: another device read the channel up to
    /// `message_id` (issue #62).
    ReadAcked {
        channel_id: String,
        message_id: String,
    },
    MemberListUpdate {
        update: crate::member_list::MemberListUpdate,
    },
    /// The page around `message_id`; empty when the fetch failed.
    MessagesAround {
        channel_id: String,
        message_id: String,
        messages: Vec<Message>,
    },
}

/// Settings → Perfil (ui/profile_settings.rs), wrapped in [`UiEvent::Profile`].
#[derive(Debug)]
pub enum ProfileEvent {
    Loaded(Result<crate::model::Profile, String>),
    /// The avatar file, cropped: its data URI and preview; `Ok(None)` = cancelled.
    AvatarPicked(Result<Option<(String, egui::ColorImage)>, String>),
    /// `user` is set once PATCH /users/@me went through, even if a later
    /// request failed.
    Saved {
        user: Option<User>,
        error: Option<String>,
    },
}
