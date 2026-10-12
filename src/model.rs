//! Discord API data models. Only the fields the native client actually uses
//! are modeled; everything else arrives with `#[serde(default)]`.

use serde::{Deserialize, Serialize};

pub const CHANNEL_KIND_GUILD_TEXT: i64 = 0;
pub const CHANNEL_KIND_DM: i64 = 1;
pub const CHANNEL_KIND_VOICE: i64 = 2;
pub const CHANNEL_KIND_GROUP_DM: i64 = 3;
pub const CHANNEL_KIND_CATEGORY: i64 = 4;
pub const CHANNEL_KIND_ANNOUNCEMENT: i64 = 5;
pub const CHANNEL_KIND_ANNOUNCEMENT_THREAD: i64 = 10;
pub const CHANNEL_KIND_PUBLIC_THREAD: i64 = 11;
pub const CHANNEL_KIND_PRIVATE_THREAD: i64 = 12;
pub const CHANNEL_KIND_FORUM: i64 = 15;
pub const CHANNEL_KIND_MEDIA: i64 = 16;

/// Message types rendered as regular chat messages (0 = default, plus the
/// common "system" ones the client just shows as plain text for now).
pub const MESSAGE_KIND_DEFAULT: i64 = 0;
/// "X iniciou um tópico", posted in the parent channel; the reference
/// points at the thread.
pub const MESSAGE_KIND_THREAD_CREATED: i64 = 18;
/// A thread's first message, quoting the parent message it started from.
pub const MESSAGE_KIND_THREAD_STARTER: i64 = 21;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub discriminator: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
}

impl User {
    pub fn display_name(&self) -> &str {
        self.global_name.as_deref().unwrap_or(&self.username)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Relationship {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: u8,
    pub user: User,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Guild {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
}

/// A guild's custom emoji (READY, GUILD_CREATE, GUILD_EMOJIS_UPDATE).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GuildEmoji {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub animated: bool,
    /// `false` once the guild lost the boost level that unlocked it.
    #[serde(default)]
    pub available: Option<bool>,
}

/// One result of Discord's GIF picker endpoints (`/gifs/search`,
/// `/gifs/trending-gifs`), Tenor behind them.
#[derive(Clone, Debug, Deserialize)]
pub struct Gif {
    /// Provider page (tenor.com/view/…): what the official client sends,
    /// which Discord embeds as a gifv.
    pub url: String,
    /// The media in the requested `media_format`.
    pub src: String,
    #[serde(default)]
    pub width: f32,
    #[serde(default)]
    pub height: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub recipients: Vec<User>,
    /// Group DM icon hash (`channel-icons/{id}/{icon}`).
    #[serde(default)]
    pub icon: Option<String>,
    /// Snowflake of the last message, for ordering DMs by recency.
    #[serde(default)]
    pub last_message_id: Option<String>,
    /// Threads only: their guild (gateway payloads, REST thread lists).
    #[serde(default)]
    pub guild_id: Option<String>,
    /// Threads only: messages sent, not counting the starter message.
    #[serde(default)]
    pub message_count: Option<u64>,
    #[serde(default)]
    pub thread_metadata: Option<ThreadMetadata>,
    /// Threads only: set when the current user joined the thread.
    #[serde(default)]
    pub member: Option<ThreadMember>,
    /// Forums only: the tags posts can carry.
    #[serde(default)]
    pub available_tags: Vec<ForumTag>,
    /// Forum posts only: ids of their tags, from the parent's `available_tags`.
    #[serde(default)]
    pub applied_tags: Vec<String>,
}

/// A forum tag; at most one of the emoji fields is set.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForumTag {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub emoji_id: Option<String>,
    #[serde(default)]
    pub emoji_name: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ThreadMetadata {
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub locked: bool,
}

/// The current user's membership in a thread. `id` (the thread) only
/// comes in lists, where members ride beside the threads.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ThreadMember {
    #[serde(default)]
    pub id: Option<String>,
}

/// `{ threads, members }`: THREAD_LIST_SYNC and the REST thread lists.
#[derive(Debug, Default, Deserialize)]
pub struct ThreadList {
    #[serde(default)]
    pub threads: Vec<Channel>,
    #[serde(default)]
    pub members: Vec<ThreadMember>,
}

impl ThreadList {
    /// The threads, with the user's memberships attached.
    pub fn into_threads(self) -> Vec<Channel> {
        let mut threads = self.threads;
        for thread in &mut threads {
            if let Some(member) = self
                .members
                .iter()
                .find(|member| member.id.as_deref() == Some(thread.id.as_str()))
            {
                thread.member = Some(member.clone());
            }
        }
        threads
    }
}

/// `GET /channels/{forum}/threads/search`: a page of posts with their first
/// messages (whose ids are the posts' ids).
#[derive(Debug, Default, Deserialize)]
pub struct ThreadSearch {
    #[serde(flatten)]
    pub list: ThreadList,
    #[serde(default)]
    pub first_messages: Vec<Message>,
    #[serde(default)]
    pub has_more: bool,
}

impl Channel {
    pub fn is_thread(&self) -> bool {
        matches!(
            self.kind,
            CHANNEL_KIND_ANNOUNCEMENT_THREAD
                | CHANNEL_KIND_PUBLIC_THREAD
                | CHANNEL_KIND_PRIVATE_THREAD
        )
    }

    /// Forum and media channels hold posts (threads), not messages.
    pub fn is_forum(&self) -> bool {
        matches!(self.kind, CHANNEL_KIND_FORUM | CHANNEL_KIND_MEDIA)
    }

    pub fn is_archived(&self) -> bool {
        self.thread_metadata
            .as_ref()
            .is_some_and(|meta| meta.archived)
    }

    pub fn is_voice(&self) -> bool {
        self.kind == CHANNEL_KIND_VOICE
    }

    pub fn is_selectable(&self) -> bool {
        matches!(
            self.kind,
            CHANNEL_KIND_GUILD_TEXT
                | CHANNEL_KIND_ANNOUNCEMENT
                | CHANNEL_KIND_DM
                | CHANNEL_KIND_GROUP_DM
        )
    }

    /// The other user of a 1:1 DM; `None` for group DMs and guild channels.
    pub fn dm_partner(&self) -> Option<&User> {
        (self.kind == CHANNEL_KIND_DM)
            .then(|| self.recipients.first())
            .flatten()
    }

    pub fn display_name(&self) -> String {
        match self.kind {
            CHANNEL_KIND_DM => self
                .recipients
                .first()
                .map(|user| user.display_name().to_string())
                .unwrap_or_else(|| "Mensagem direta".to_string()),
            CHANNEL_KIND_GROUP_DM => self.name.clone().unwrap_or_else(|| "Grupo".to_string()),
            _ => self.name.clone().unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    pub author: User,
    #[serde(rename = "type", default)]
    pub kind: i64,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub embeds: Vec<Embed>,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub mentions: Vec<User>,
    #[serde(default)]
    pub message_reference: Option<MessageReference>,
    /// The original message, when Discord embeds it alongside the reply.
    #[serde(default)]
    pub referenced_message: Option<Box<Message>>,
    /// The thread this message started, when it started one.
    #[serde(default)]
    pub thread: Option<Box<Channel>>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reaction {
    #[serde(default)]
    pub count: u64,
    pub emoji: ReactionEmoji,
}

/// A reaction's emoji: custom ones have an `id`, Unicode ones only a `name`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReactionEmoji {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// `message_reference`: which message a reply points at.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MessageReference {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
}

impl Message {
    pub fn is_system(&self) -> bool {
        self.kind != MESSAGE_KIND_DEFAULT
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Embed {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// `rich`, `image`, `gifv` (Tenor/Giphy), `video`, `link`…
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub image: Option<EmbedMedia>,
    #[serde(default)]
    pub thumbnail: Option<EmbedMedia>,
    #[serde(default)]
    pub video: Option<EmbedMedia>,
    /// The left bar's color, `0xRRGGBB`.
    #[serde(default)]
    pub color: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbedMedia {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
}

/// Who is in which voice channel, from VOICE_STATE_UPDATE and the
/// voice_states seeded in READY. `guild_id` is always filled by the
/// dispatch layer, even though READY entries omit it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VoiceState {
    #[serde(default)]
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub user_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub self_mute: bool,
    #[serde(default)]
    pub self_deaf: bool,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub deaf: bool,
    #[serde(default)]
    pub suppress: bool,
    /// Go Live: set by the server while the user streams.
    #[serde(default)]
    pub self_stream: bool,
}

/// A guild member, fetched to turn voice-state user ids into names.
#[derive(Clone, Debug, Deserialize)]
pub struct Member {
    pub user: User,
    #[serde(default)]
    pub nick: Option<String>,
}

/// The editable bits of our own profile (settings → Perfil). Empty strings
/// mean unset; `nick` is for the guild selected when it was loaded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub global_name: String,
    pub pronouns: String,
    pub bio: String,
    pub nick: String,
}
