//! Threads (channel types 10/11/12) and forum posts: the state the gateway
//! keeps for them, their rows under the parent channel, the "Tópicos"
//! browser, forum channels and the "Criar tópico" dialog.
//!
//! Threads live in `guild_channels` next to their parents, so the chat,
//! mentions and unread routing treat them as ordinary channels. Active
//! ones come from the gateway; archived ones only from the browser's REST
//! fetch or, in forums, from `threads/search` pages, which also bring the
//! posts' first messages (`post-data` fills in the active posts').

use std::collections::{HashMap, HashSet};

use egui::{Color32, FontId, RichText, Sense, Vec2, pos2};

use crate::app::{ChannelRef, VesktopApp};
use crate::backend::{Command, UiEvent};
use crate::model::{
    CHANNEL_KIND_ANNOUNCEMENT, CHANNEL_KIND_ANNOUNCEMENT_THREAD, CHANNEL_KIND_GUILD_TEXT,
    CHANNEL_KIND_PUBLIC_THREAD, Channel, ForumTag, MESSAGE_KIND_DEFAULT,
    MESSAGE_KIND_THREAD_CREATED, MESSAGE_KIND_THREAD_STARTER, Message, ThreadMember,
};
use crate::theme;
use crate::ui::fade;

/// Discord caps thread names at 100 characters.
const NAME_MAX: usize = 100;
/// Reply type: a reply can start a thread like a plain message.
const MESSAGE_KIND_REPLY: i64 = 19;

impl VesktopApp {
    /// A known channel by id, threads included.
    pub(crate) fn find_channel(&self, id: &str) -> Option<&Channel> {
        match self.channel_index.get(id)? {
            ChannelRef::Guild(guild_id) => self
                .guild_channels
                .get(guild_id)?
                .iter()
                .find(|channel| channel.id == id),
            ChannelRef::Dm => self.dm_channels.iter().find(|channel| channel.id == id),
        }
    }

    fn find_thread_mut(&mut self, id: &str) -> Option<&mut Channel> {
        let Some(ChannelRef::Guild(guild_id)) = self.channel_index.get(id) else {
            return None;
        };
        self.guild_channels
            .get_mut(guild_id)?
            .iter_mut()
            .find(|channel| channel.id == id && channel.is_thread())
    }

    /// Where a guild's threads go: its channel list, or a holding list
    /// until the channels load (an empty `guild_channels` entry would pass
    /// for a loaded guild).
    fn thread_list(&mut self, guild_id: &str) -> &mut Vec<Channel> {
        if self.guild_channels.contains_key(guild_id) {
            self.guild_channels
                .get_mut(guild_id)
                .expect("checked above")
        } else {
            self.pending_threads
                .entry(guild_id.to_string())
                .or_default()
        }
    }

    /// Indexes the guild's threads once its channels are loaded.
    fn index_threads(&mut self, guild_id: &str) {
        for channel in self.guild_channels.get(guild_id).into_iter().flatten() {
            if channel.is_thread() {
                self.channel_index
                    .insert(channel.id.clone(), ChannelRef::Guild(guild_id.to_string()));
            }
        }
    }

    fn upsert_threads(&mut self, threads: Vec<Channel>) {
        for thread in threads {
            let guild_id = thread.guild_id.clone().or_else(|| {
                match self.channel_index.get(thread.parent_id.as_deref()?)? {
                    ChannelRef::Guild(id) => Some(id.clone()),
                    ChannelRef::Dm => None,
                }
            });
            let Some(guild_id) = guild_id.filter(|_| thread.is_thread()) else {
                continue;
            };
            upsert_thread(self.thread_list(&guild_id), thread);
            self.index_threads(&guild_id);
        }
    }

    pub(crate) fn handle_thread_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::ThreadsSynced {
                guild_id,
                parent_ids,
                threads,
            } => {
                sync_threads(self.thread_list(&guild_id), parent_ids.as_deref(), threads);
                self.index_threads(&guild_id);
            }
            UiEvent::ThreadsUpserted { threads } => self.upsert_threads(threads),
            UiEvent::ThreadCreated { mut thread } => {
                // Creating a thread joins it.
                thread.member.get_or_insert_with(ThreadMember::default);
                let id = thread.id.clone();
                self.upsert_threads(vec![thread]);
                if self.channel_index.contains_key(&id) {
                    self.select_channel(id);
                }
            }
            UiEvent::ThreadDeleted { thread_id } => {
                let parent = self
                    .find_thread_mut(&thread_id)
                    .and_then(|thread| thread.parent_id.clone());
                if let Some(ChannelRef::Guild(guild_id)) = self.channel_index.remove(&thread_id)
                    && let Some(list) = self.guild_channels.get_mut(&guild_id)
                {
                    list.retain(|channel| channel.id != thread_id);
                }
                if self.selected_channel.as_deref() == Some(thread_id.as_str())
                    && let Some(parent) = parent
                {
                    self.select_channel(parent);
                }
            }
            UiEvent::ForumPosts {
                forum_id,
                sort_by,
                threads,
                first_messages,
                has_more,
            } => {
                // A page for a sort since abandoned only adds posts.
                if let Some(paging) = self
                    .forums
                    .pages
                    .get_mut(&forum_id)
                    .filter(|paging| paging.sort.param() == sort_by)
                {
                    paging.offset += threads.len();
                    paging.has_more = has_more;
                    paging.loading = false;
                }
                self.forums.store(first_messages);
                self.upsert_threads(threads);
            }
            UiEvent::ForumFirstMessages { messages } => self.forums.store(messages),
            UiEvent::ThreadMembers {
                thread_id,
                added,
                removed,
            } => {
                let Some(me) = self.me.as_ref().map(|me| me.id.clone()) else {
                    return;
                };
                if let Some(thread) = self.find_thread_mut(&thread_id) {
                    if added.contains(&me) {
                        thread.member.get_or_insert_with(ThreadMember::default);
                    } else if removed.contains(&me) {
                        thread.member = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// Keeps a thread's activity and count current as messages arrive
    /// (Discord sends no THREAD_UPDATE for those).
    pub(crate) fn note_thread_message(&mut self, message: &Message) {
        let Some(thread) = self.find_thread_mut(&message.channel_id) else {
            return;
        };
        let newer = thread
            .last_message_id
            .as_deref()
            .is_none_or(|last| (message.id.len(), message.id.as_str()) > (last.len(), last));
        if newer {
            thread.last_message_id = Some(message.id.clone());
            thread.message_count = Some(thread.message_count.unwrap_or(0) + 1);
        }
    }

    /// Opens a thread that may not be registered yet (archived ones known
    /// only from a message).
    fn open_thread(&mut self, thread: Channel) {
        let id = thread.id.clone();
        if !self.channel_index.contains_key(&id) {
            self.upsert_threads(vec![thread]);
        }
        if self.channel_index.contains_key(&id) {
            self.select_channel(id);
        }
    }

    /// The thread a message started: the live copy, or the message's own.
    fn started_thread(&self, message: &Message) -> Option<Channel> {
        self.find_channel(&message.id)
            .filter(|channel| channel.is_thread())
            .cloned()
            .or_else(|| message.thread.as_deref().cloned())
    }
}

/// Inserts or refreshes a thread. Updates don't carry `member`, so a known
/// membership survives them.
fn upsert_thread(list: &mut Vec<Channel>, thread: Channel) {
    match list.iter_mut().find(|channel| channel.id == thread.id) {
        Some(old) => {
            let member = old.member.take();
            *old = thread;
            if old.member.is_none() {
                old.member = member;
            }
        }
        None => list.push(thread),
    }
}

/// Applies a full list of active threads for `parent_ids` (the whole guild
/// when `None`). Active threads missing from it were archived or became
/// unreachable: they are flagged archived, which hides them from the
/// sidebar but keeps an open one working.
fn sync_threads(list: &mut Vec<Channel>, parent_ids: Option<&[String]>, threads: Vec<Channel>) {
    for channel in list.iter_mut() {
        let in_scope = parent_ids.is_none_or(|ids| {
            channel
                .parent_id
                .as_ref()
                .is_some_and(|parent| ids.contains(parent))
        });
        if channel.is_thread() && in_scope && !threads.iter().any(|t| t.id == channel.id) {
            channel.thread_metadata.get_or_insert_default().archived = true;
        }
    }
    for thread in threads {
        upsert_thread(list, thread);
    }
}

/// Most recent activity first, by snowflake (numeric order).
fn by_activity(a: &&Channel, b: &&Channel) -> std::cmp::Ordering {
    let key = |channel: &Channel| {
        let id = channel
            .last_message_id
            .clone()
            .unwrap_or(channel.id.clone());
        (id.len(), id)
    };
    key(b).cmp(&key(a))
}

/// The threads Discord nests under a channel: active ones the user joined,
/// plus the open one, most recently active first.
pub(crate) fn sidebar_threads<'a>(
    channels: &'a [Channel],
    parent_id: &str,
    selected: Option<&str>,
) -> Vec<&'a Channel> {
    let mut threads: Vec<&Channel> = channels
        .iter()
        .filter(|channel| {
            channel.is_thread()
                && channel.parent_id.as_deref() == Some(parent_id)
                && ((channel.member.is_some() && !channel.is_archived())
                    || selected == Some(channel.id.as_str()))
        })
        .collect();
    threads.sort_by(by_activity);
    threads
}

/// Every known thread of a channel, split into (active, archived).
fn channel_threads(channels: &[Channel], parent_id: &str) -> (Vec<Channel>, Vec<Channel>) {
    let mut threads: Vec<&Channel> = channels
        .iter()
        .filter(|channel| channel.is_thread() && channel.parent_id.as_deref() == Some(parent_id))
        .collect();
    threads.sort_by(by_activity);
    let (archived, active) = threads.into_iter().cloned().partition(Channel::is_archived);
    (active, archived)
}

fn message_count_label(thread: &Channel) -> String {
    match thread.message_count.unwrap_or(0) {
        1 => "1 mensagem".to_string(),
        count => format!("{count} mensagens"),
    }
}

/// The joined threads under a sidebar channel row, with Discord's spine.
pub(crate) fn paint_sidebar_threads(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    guild_id: &str,
    parent_id: &str,
    selected: Option<&str>,
    clicked: &mut Option<String>,
) {
    let Some(channels) = app.guild_channels.get(guild_id) else {
        return;
    };
    let threads: Vec<Channel> = sidebar_threads(channels, parent_id, selected)
        .into_iter()
        .cloned()
        .collect();
    let spine_x = ui.cursor().left() + 20.0;
    let spine_top = ui.cursor().top();
    for thread in threads {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
        let is_selected = selected == Some(thread.id.as_str());
        let unread = app.unread.get(&thread.id).copied().unwrap_or(0) > 0;
        let row = egui::Rect::from_min_max(pos2(rect.left() + 32.0, rect.top()), rect.max);
        ui.painter().rect_filled(
            row,
            theme::RADIUS_SM,
            crate::ui::control_bg(ui, &response, is_selected),
        );
        // The spine runs down from the parent and curls into each row.
        let stroke = egui::Stroke::new(1.5, theme::DIVIDER);
        let y = rect.center().y;
        ui.painter()
            .line_segment([pos2(spine_x, spine_top), pos2(spine_x, y - 6.0)], stroke);
        ui.painter()
            .add(egui::epaint::QuadraticBezierShape::from_points_stroke(
                [
                    pos2(spine_x, y - 6.0),
                    pos2(spine_x, y),
                    pos2(spine_x + 8.0, y),
                ],
                false,
                egui::Color32::TRANSPARENT,
                stroke,
            ));
        let color = if is_selected || unread {
            theme::WHITE
        } else {
            fade(
                ui,
                response.id.with("text"),
                response.hovered(),
                theme::MUTED,
                theme::TEXT,
            )
        };
        let mut text = RichText::new(thread.display_name()).size(14.0).color(color);
        if unread {
            text = text.family(theme::bold());
        }
        let mut content = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row.shrink2(Vec2::new(8.0, 2.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        // Private threads we haven't joined wear Discord's padlock.
        if app.channel_hidden(&thread, guild_id) {
            let (icon_rect, _) = content.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            crate::ui::icons::draw(
                content.painter(),
                crate::ui::icons::Icon::Lock,
                icon_rect.center(),
                theme::MUTED,
                0.0,
            );
            content.add_space(4.0);
        }
        crate::ui::emoji::label(app, &mut content, text, true);
        if response.clicked() && !app.channel_hidden(&thread, guild_id) {
            *clicked = Some(thread.id.clone());
        }
    }
}

/// A speech bubble with two lines: Discord's threads glyph.
fn draw_threads_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.6, color);
    let bubble = egui::Rect::from_center_size(center - Vec2::new(0.0, 1.0), Vec2::new(16.0, 12.0));
    painter.rect_stroke(bubble, 3.0, stroke, egui::StrokeKind::Middle);
    painter.line_segment(
        [
            pos2(bubble.left() + 3.0, bubble.bottom()),
            pos2(bubble.left() + 2.0, bubble.bottom() + 4.0),
        ],
        stroke,
    );
    for dy in [-2.0, 2.0] {
        painter.line_segment(
            [
                pos2(bubble.left() + 4.0, bubble.center().y + dy),
                pos2(bubble.right() - 4.0, bubble.center().y + dy),
            ],
            stroke,
        );
    }
}

/// The header's "Tópicos" button and its popover: active and archived
/// threads of a text or announcement channel, plus "Criar tópico".
pub(crate) fn header_button(app: &mut VesktopApp, ui: &mut egui::Ui, channel_id: &str) {
    let Some(parent) = app.find_channel(channel_id).cloned() else {
        return;
    };
    if !matches!(
        parent.kind,
        CHANNEL_KIND_GUILD_TEXT | CHANNEL_KIND_ANNOUNCEMENT
    ) {
        return;
    }
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    ui.painter()
        .rect_filled(rect, 4.0, crate::ui::control_bg(ui, &response, false));
    draw_threads_icon(ui.painter(), rect.center(), theme::TEXT);
    let response = response.on_hover_text("Tópicos");
    if response.clicked() {
        app.send(Command::LoadThreads {
            channel_id: channel_id.to_string(),
        });
    }
    egui::Popup::menu(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::BOTTOM_END)
        .width(320.0)
        .show(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Tópicos")
                        .family(theme::bold())
                        .size(15.0)
                        .color(theme::TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Criar").clicked() {
                        start_draft(ui.ctx(), ThreadDraft::standalone(&parent));
                        ui.close();
                    }
                });
            });
            ui.separator();
            if let Some(id) = thread_list(app, ui, channel_id, 360.0) {
                ui.close();
                app.select_channel(id);
            }
        });
}

/// Threads of `parent_id` as clickable rows, active then archived;
/// returns the clicked one.
fn thread_list(
    app: &VesktopApp,
    ui: &mut egui::Ui,
    parent_id: &str,
    max_height: f32,
) -> Option<String> {
    let Some(ChannelRef::Guild(guild_id)) = app.channel_index.get(parent_id) else {
        return None;
    };
    let (active, archived) = app
        .guild_channels
        .get(guild_id)
        .map(|channels| channel_threads(channels, parent_id))
        .unwrap_or_default();
    if active.is_empty() && archived.is_empty() {
        ui.label(RichText::new("Nenhum tópico ainda.").color(theme::MUTED));
        return None;
    }
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .min_scrolled_height(max_height)
        .max_height(max_height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (title, threads) in [("ATIVOS", &active), ("ARQUIVADOS", &archived)] {
                if threads.is_empty() {
                    continue;
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(title)
                        .size(12.0)
                        .family(theme::bold())
                        .color(theme::MUTED),
                );
                for thread in threads {
                    if thread_card(ui, thread) {
                        clicked = Some(thread.id.clone());
                    }
                }
            }
        });
    clicked
}

/// One thread or forum post: name over its count and last activity.
fn thread_card(ui: &mut egui::Ui, thread: &Channel) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 46.0), Sense::click());
    ui.painter().rect_filled(
        rect,
        theme::RADIUS_SM,
        crate::ui::control_bg(ui, &response, false),
    );
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(10.0, 5.0)))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    content.spacing_mut().item_spacing.y = 2.0;
    content.add(
        egui::Label::new(
            RichText::new(thread.display_name())
                .family(theme::bold())
                .color(theme::TEXT),
        )
        .truncate()
        .selectable(false),
    );
    let mut detail = message_count_label(thread);
    let last = thread.last_message_id.as_deref().unwrap_or(&thread.id);
    if let Some(when) =
        crate::util::snowflake_ms(last).and_then(chrono::DateTime::from_timestamp_millis)
    {
        detail.push_str(" · ");
        detail.push_str(&crate::util::header_time(&when.to_rfc3339()).0);
    }
    content.add(
        egui::Label::new(RichText::new(detail).small().color(theme::MUTED))
            .truncate()
            .selectable(false),
    );
    response.clicked()
}

/// What the forum view keeps beside the threads (`app.forums`): posts'
/// first messages, which the gateway doesn't carry, and archive paging.
#[derive(Default)]
pub(crate) struct Forums {
    /// By post id: a post's first message has the post's snowflake.
    first_messages: HashMap<String, Message>,
    /// Posts whose first message was already asked for.
    requested: HashSet<String>,
    pages: HashMap<String, ForumPaging>,
}

impl Forums {
    fn store(&mut self, messages: Vec<Message>) {
        for message in messages {
            self.first_messages.insert(message.id.clone(), message);
        }
    }
}

/// Archived posts come from `threads/search`, 25 at a time.
struct ForumPaging {
    sort: ForumSort,
    offset: usize,
    has_more: bool,
    loading: bool,
}

impl ForumPaging {
    fn new(sort: ForumSort) -> Self {
        Self {
            sort,
            offset: 0,
            has_more: true,
            loading: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ForumSort {
    Activity,
    Creation,
}

impl ForumSort {
    fn param(self) -> &'static str {
        match self {
            Self::Activity => "last_message_time",
            Self::Creation => "creation_time",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Activity => "Atividade recente",
            Self::Creation => "Data de criação",
        }
    }
}

/// The forum's search text and tag filter, per forum (egui memory).
#[derive(Clone, Default)]
struct ForumView {
    query: String,
    tags: Vec<String>,
}

const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 24 * 60 * MINUTE_MS;

/// Discord's compact age: "Há 5 min", "Há 3h", "Há 1d", "Há 2 meses".
fn ago(then_ms: i64, now_ms: i64) -> String {
    let minutes = (now_ms - then_ms).max(0) / MINUTE_MS;
    match minutes {
        0 => "Agora".to_string(),
        1..60 => format!("Há {minutes} min"),
        60..1440 => format!("Há {}h", minutes / 60),
        1440..43_200 => format!("Há {}d", minutes / 1440),
        43_200..525_600 => match minutes / 43_200 {
            1 => "Há 1 mês".to_string(),
            months => format!("Há {months} meses"),
        },
        _ => match minutes / 525_600 {
            1 => "Há 1 ano".to_string(),
            years => format!("Há {years} anos"),
        },
    }
}

/// The "NOVO" badge. Discord shows it for posts unread and created after
/// the user's last look at the forum, from read states this client doesn't
/// keep. Approximated: never opened nor joined, and created in the last
/// three days.
fn is_new(post: &Channel, opened: bool, now_ms: i64) -> bool {
    !opened
        && post.member.is_none()
        && crate::util::snowflake_ms(&post.id).is_some_and(|ms| now_ms - ms < 3 * DAY_MS)
}

/// Title contains the search text (any case) and, with tags picked, the
/// post carries one of them (Discord's "match some").
fn post_matches(post: &Channel, query: &str, tags: &[String]) -> bool {
    post.display_name()
        .to_lowercase()
        .contains(&query.trim().to_lowercase())
        && (tags.is_empty() || post.applied_tags.iter().any(|tag| tags.contains(tag)))
}

/// The image of a tag or reaction emoji: custom from Discord's CDN,
/// Unicode as Twemoji.
pub(crate) fn emoji_image(id: Option<&str>, name: Option<&str>) -> Option<String> {
    match id {
        Some(id) => Some(crate::ui::emoji::custom_url(id, false)),
        None => emojis::get(name?).map(crate::ui::emoji::twemoji_url),
    }
}

fn tag_emoji(tag: &ForumTag) -> Option<String> {
    emoji_image(tag.emoji_id.as_deref(), tag.emoji_name.as_deref())
}

/// A tag's text; its emoji is drawn as an image unless no image is known.
fn tag_text(tag: &ForumTag) -> String {
    match &tag.emoji_name {
        Some(emoji) if tag_emoji(tag).is_none() => format!("{emoji} {}", tag.name),
        _ => tag.name.clone(),
    }
}

/// One line of a post's first message, mentions shown as names.
fn preview_text(app: &VesktopApp, message: &Message) -> String {
    let text: String = crate::markup::tokenize(&message.content)
        .into_iter()
        .map(|segment| match segment.style {
            _ if segment.marks.spoiler => "▒".repeat(segment.text.chars().count()),
            crate::markup::Style::Mention => message
                .mentions
                .iter()
                .find(|user| user.id == segment.text.trim_start_matches('@'))
                .map(|user| format!("@{}", user.display_name()))
                .unwrap_or(segment.text),
            crate::markup::Style::ChannelName => app
                .find_channel(segment.text.trim_start_matches('#'))
                .map(|channel| format!("#{}", channel.display_name()))
                .unwrap_or(segment.text),
            _ => segment.text,
        })
        .collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first image attached to a message, for the post's thumbnail.
fn first_image(message: &Message) -> Option<String> {
    message
        .attachments
        .iter()
        .find(|attachment| {
            attachment
                .content_type
                .as_deref()
                .is_some_and(|kind| kind.starts_with("image/"))
        })
        .and_then(|attachment| attachment.url.clone())
}

impl VesktopApp {
    /// Asks for the forum's next page of archived posts, unless one is on
    /// its way or the archive is exhausted.
    fn load_forum_page(&mut self, forum_id: &str) {
        let paging = self
            .forums
            .pages
            .entry(forum_id.to_string())
            .or_insert_with(|| ForumPaging::new(ForumSort::Activity));
        if paging.loading || !paging.has_more {
            return;
        }
        paging.loading = true;
        let (sort_by, offset) = (paging.sort.param(), paging.offset);
        self.send(Command::SearchForumPosts {
            forum_id: forum_id.to_string(),
            sort_by,
            offset,
        });
    }
}

/// A rounded label: tags, badges, chips. `emoji` is a custom emoji painted
/// before the text; `fill` sees the response, for hover.
#[allow(clippy::too_many_arguments)]
fn pill(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    text: &str,
    font: FontId,
    color: Color32,
    // Image URL of the emoji drawn before the text.
    emoji: Option<&str>,
    height: f32,
    fill: impl FnOnce(&egui::Ui, &egui::Response) -> Color32,
) -> egui::Response {
    let (galley, size) = pill_size(ui, text, font, color, emoji.is_some(), height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    ui.painter()
        .rect_filled(rect, height / 2.0, fill(ui, &response));
    let mut x = rect.left() + height / 2.5;
    if let Some(url) = emoji {
        let icon = egui::Rect::from_min_size(
            pos2(x, rect.center().y - height * 0.3),
            Vec2::splat(height * 0.6),
        );
        if let Some(texture) = app.images.get(ui.ctx(), &app.handle, url) {
            egui::Image::from_texture(&texture).paint_at(ui, icon);
        }
        x += icon.width() + 4.0;
    }
    let y = rect.center().y - galley.size().y / 2.0;
    ui.painter().galley(pos2(x, y), galley, color);
    response
}

fn pill_size(
    ui: &egui::Ui,
    text: &str,
    font: FontId,
    color: Color32,
    emoji: bool,
    height: f32,
) -> (std::sync::Arc<egui::Galley>, Vec2) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let emoji_width = if emoji { height * 0.6 + 4.0 } else { 0.0 };
    let width = galley.size().x + emoji_width + 2.0 * height / 2.5;
    (galley, Vec2::new(width, height))
}

/// A filled speech bubble: the posts' message count and "Nova postagem".
fn draw_bubble(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: Color32) {
    painter.circle_filled(center, radius, color);
    painter.add(egui::Shape::convex_polygon(
        vec![
            center + Vec2::new(-radius, 0.0),
            center + Vec2::new(0.0, radius),
            center + Vec2::new(-radius, radius),
        ],
        color,
        egui::Stroke::NONE,
    ));
}

/// A clipboard with a check: the post guidelines button.
fn draw_guidelines_icon(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    let board = egui::Rect::from_center_size(center, Vec2::new(13.0, 15.0));
    painter.rect_stroke(board, 2.0, stroke, egui::StrokeKind::Middle);
    painter.add(egui::Shape::line(
        vec![
            center + Vec2::new(-3.5, 0.0),
            center + Vec2::new(-1.0, 2.5),
            center + Vec2::new(3.5, -2.5),
        ],
        stroke,
    ));
}

fn chip_fill(ui: &egui::Ui, response: &egui::Response, selected: bool) -> Color32 {
    if selected {
        theme::BLURPLE.gamma_multiply(0.6)
    } else {
        fade(
            ui,
            response.id,
            response.hovered(),
            theme::HOVER,
            theme::SELECTED,
        )
    }
}

/// A forum channel, as Discord lays it out: search-or-create bar, post
/// guidelines, sort and tag chips, then the posts as cards.
pub(crate) fn paint_forum(app: &mut VesktopApp, ui: &mut egui::Ui, forum_id: &str) {
    let Some(forum) = app.find_channel(forum_id).cloned() else {
        return;
    };
    let view_id = egui::Id::new(("forum_view", forum_id));
    let mut view: ForumView = ui
        .ctx()
        .data_mut(|data| data.get_temp(view_id).unwrap_or_default());
    let guidelines_id = egui::Id::new(("forum_guidelines", forum_id));
    let mut guidelines_open = ui
        .ctx()
        .data_mut(|data| *data.get_persisted_mut_or(guidelines_id, true));
    let topic = forum.topic.clone().filter(|topic| !topic.trim().is_empty());

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.add_space(16.0);
        let (icon, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
        draw_threads_icon(ui.painter(), icon.center(), theme::MUTED);
        ui.add_space(8.0);
        ui.label(
            RichText::new(forum.display_name())
                .family(theme::bold())
                .size(16.0)
                .color(theme::TEXT),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(14.0);
            crate::ui::members::toggle(ui);
        });
    });
    ui.separator();
    crate::ui::members::panel(app, ui, forum_id);

    let sort = app
        .forums
        .pages
        .get(forum_id)
        .map_or(ForumSort::Activity, |paging| paging.sort);
    let mut new_sort = None;
    let mut clicked = None;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let output = egui::ScrollArea::vertical()
        .id_salt(("forum", forum_id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(20, 12))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    search_bar(ui, &forum, &mut view, topic.is_some(), &mut guidelines_open);
                    if let Some(error) = &app.compose_error {
                        ui.label(RichText::new(error).small().color(theme::RED));
                    }
                    if let Some(topic) = &topic {
                        guidelines(app, ui, topic, &mut guidelines_open);
                    }
                    new_sort = filter_chips(app, ui, &forum, sort, &mut view);
                    clicked = post_cards(app, ui, &forum, sort, &view, now_ms);
                });
        });
    let near_bottom =
        output.state.offset.y + output.inner_rect.height() > output.content_size.y - 400.0;

    if let Some(sort) = new_sort {
        app.forums
            .pages
            .insert(forum_id.to_string(), ForumPaging::new(sort));
    }
    // The first page, then one more whenever the list nears its end.
    let first_page = app
        .forums
        .pages
        .get(forum_id)
        .is_none_or(|paging| paging.offset == 0);
    if first_page || near_bottom {
        app.load_forum_page(forum_id);
    }
    if let Some(id) = clicked {
        app.select_channel(id);
    }
    ui.ctx().data_mut(|data| {
        data.insert_temp(view_id, view);
        data.insert_persisted(guidelines_id, guidelines_open);
    });
}

/// "Buscar ou criar postagem...", the guidelines toggle and "Nova postagem"
/// (titled with what was typed).
fn search_bar(
    ui: &mut egui::Ui,
    forum: &Channel,
    view: &mut ForumView,
    has_guidelines: bool,
    guidelines_open: &mut bool,
) {
    egui::Frame::new()
        .fill(theme::SIDEBAR)
        .stroke(egui::Stroke::new(1.0, theme::DIVIDER))
        .corner_radius(theme::RADIUS_SM)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (icon, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                crate::ui::channel_sidebar::draw_search_icon(
                    ui.painter(),
                    icon.center(),
                    theme::MUTED,
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let text = ui.painter().layout_no_wrap(
                        "Nova postagem".to_string(),
                        FontId::new(14.0, theme::bold()),
                        theme::WHITE,
                    );
                    let size = Vec2::new(text.size().x + 44.0, 34.0);
                    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                    let fill = fade(
                        ui,
                        response.id,
                        response.hovered(),
                        theme::BLURPLE,
                        Color32::from_rgb(0x47, 0x52, 0xc4),
                    );
                    ui.painter().rect_filled(rect, theme::RADIUS_SM, fill);
                    draw_bubble(
                        ui.painter(),
                        pos2(rect.left() + 20.0, rect.center().y),
                        6.5,
                        theme::WHITE,
                    );
                    let y = rect.center().y - text.size().y / 2.0;
                    ui.painter()
                        .galley(pos2(rect.left() + 32.0, y), text, theme::WHITE);
                    if response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        let name = view.query.trim().chars().take(NAME_MAX).collect();
                        start_draft(
                            ui.ctx(),
                            ThreadDraft {
                                name,
                                ..ThreadDraft::standalone(forum)
                            },
                        );
                    }
                    if has_guidelines {
                        ui.add_space(8.0);
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                        ui.painter().rect_filled(
                            rect,
                            theme::RADIUS_SM,
                            crate::ui::control_bg(ui, &response, *guidelines_open),
                        );
                        draw_guidelines_icon(ui.painter(), rect.center(), theme::TEXT);
                        if response.on_hover_text("Diretrizes da postagem").clicked() {
                            *guidelines_open = !*guidelines_open;
                        }
                    }
                    ui.add_space(8.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut view.query)
                            .hint_text("Buscar ou criar postagem...")
                            .font(FontId::proportional(16.0))
                            .frame(egui::Frame::NONE)
                            .desired_width(ui.available_width()),
                    );
                });
            });
        });
}

/// "Diretrizes da postagem": the forum's topic, collapsed or expanded by
/// its header (or the search bar's button).
fn guidelines(app: &mut VesktopApp, ui: &mut egui::Ui, topic: &str, open: &mut bool) {
    let frame = egui::Frame::new()
        .fill(theme::SIDEBAR)
        .corner_radius(theme::RADIUS_SM)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                draw_guidelines_icon(
                    ui.painter(),
                    ui.cursor().min + Vec2::new(8.0, 9.0),
                    theme::MUTED,
                );
                ui.add_space(22.0);
                ui.label(
                    RichText::new("Diretrizes da postagem")
                        .family(theme::bold())
                        .color(theme::TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(if *open { "▾" } else { "▸" }).color(theme::MUTED));
                });
            });
            if *open {
                ui.add_space(4.0);
                let runs = crate::ui::chat::content_runs_sized(
                    topic,
                    &app.user_cache,
                    &HashMap::new(),
                    13.5,
                );
                crate::ui::chat::paint_runs_with(app, ui, runs, false);
            }
        });
    // The header row toggles; the text below stays selectable.
    let header = egui::Rect::from_min_max(
        frame.response.rect.min,
        pos2(
            frame.response.rect.right(),
            frame.response.rect.top() + 38.0,
        ),
    );
    let response = ui
        .interact(header, ui.id().with("forum_guidelines"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        *open = !*open;
    }
}

/// "Ordenar e ver", a divider, then the tags that fit as filter chips and
/// "Todos" with every tag. Returns a newly picked sort.
fn filter_chips(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    forum: &Channel,
    sort: ForumSort,
    view: &mut ForumView,
) -> Option<ForumSort> {
    const CHIP: f32 = 32.0;
    let font = FontId::new(14.0, theme::bold());
    let mut new_sort = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let response = pill(
            app,
            ui,
            "Ordenar e ver ▾",
            font.clone(),
            theme::MUTED,
            None,
            CHIP,
            |ui, response| chip_fill(ui, response, false),
        );
        egui::Popup::menu(&response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .width(220.0)
            .show(|ui| {
                ui.label(RichText::new("Ordenar por").small().color(theme::MUTED));
                for option in [ForumSort::Activity, ForumSort::Creation] {
                    if ui
                        .selectable_label(sort == option, option.label())
                        .clicked()
                    {
                        if sort != option {
                            new_sort = Some(option);
                        }
                        ui.close();
                    }
                }
                ui.separator();
                ui.label(RichText::new("Ver como").small().color(theme::MUTED));
                let _ = ui.selectable_label(true, "Lista");
            });
        if forum.available_tags.is_empty() {
            return;
        }
        let (divider, _) = ui.allocate_exact_size(Vec2::new(1.0, CHIP - 8.0), Sense::hover());
        ui.painter().rect_filled(divider, 0.0, theme::DIVIDER);

        // Tags in order while they fit, leaving room for "Todos".
        let (_, todos) = pill_size(ui, "Todos ▾", font.clone(), theme::MUTED, false, CHIP);
        let mut room = ui.available_width() - todos.x - 8.0;
        for tag in &forum.available_tags {
            let text = tag_text(tag);
            let emoji = tag_emoji(tag);
            let (_, size) = pill_size(ui, &text, font.clone(), theme::TEXT, emoji.is_some(), CHIP);
            room -= size.x + 8.0;
            if room < 0.0 {
                break;
            }
            let on = view.tags.contains(&tag.id);
            let response = pill(
                app,
                ui,
                &text,
                font.clone(),
                theme::TEXT,
                emoji.as_deref(),
                CHIP,
                |ui, response| chip_fill(ui, response, on),
            );
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                toggle_tag(&mut view.tags, &tag.id);
            }
        }
        let response = pill(
            app,
            ui,
            "Todos ▾",
            font.clone(),
            theme::MUTED,
            None,
            CHIP,
            |ui, response| chip_fill(ui, response, false),
        );
        egui::Popup::menu(&response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .width(220.0)
            .show(|ui| {
                for tag in &forum.available_tags {
                    let mut on = view.tags.contains(&tag.id);
                    if ui.checkbox(&mut on, tag_text(tag)).changed() {
                        toggle_tag(&mut view.tags, &tag.id);
                    }
                }
                if !view.tags.is_empty() {
                    ui.separator();
                    if ui.button("Limpar filtros").clicked() {
                        view.tags.clear();
                    }
                }
            });
    });
    new_sort
}

fn toggle_tag(tags: &mut Vec<String>, id: &str) {
    match tags.iter().position(|tag| tag == id) {
        Some(index) => {
            tags.remove(index);
        }
        None => tags.push(id.to_string()),
    }
}

/// The forum's posts that pass the filters, as cards; returns the clicked
/// one. Also asks for the first messages active posts lack.
fn post_cards(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    forum: &Channel,
    sort: ForumSort,
    view: &ForumView,
    now_ms: i64,
) -> Option<String> {
    let Some(ChannelRef::Guild(guild_id)) = app.channel_index.get(&forum.id) else {
        return None;
    };
    let mut posts: Vec<Channel> = app
        .guild_channels
        .get(guild_id)
        .into_iter()
        .flatten()
        .filter(|channel| {
            channel.is_thread() && channel.parent_id.as_deref() == Some(forum.id.as_str())
        })
        .cloned()
        .collect();
    match sort {
        ForumSort::Activity => posts.sort_by(|a, b| by_activity(&a, &b)),
        ForumSort::Creation => {
            posts.sort_by_key(|post| std::cmp::Reverse((post.id.len(), post.id.clone())))
        }
    }

    let missing: Vec<String> = posts
        .iter()
        .filter(|post| {
            !app.forums.first_messages.contains_key(&post.id)
                && !app.forums.requested.contains(&post.id)
        })
        .map(|post| post.id.clone())
        .collect();
    // ponytail: 25 per request, Discord's search page size; the endpoint's
    // own cap is undocumented.
    for chunk in missing.chunks(25) {
        app.forums.requested.extend(chunk.iter().cloned());
        app.send(Command::LoadForumPostData {
            forum_id: forum.id.clone(),
            thread_ids: chunk.to_vec(),
        });
    }

    let shown: Vec<&Channel> = posts
        .iter()
        .filter(|post| post_matches(post, &view.query, &view.tags))
        .collect();
    let loading = app
        .forums
        .pages
        .get(&forum.id)
        .is_none_or(|paging| paging.loading);
    if shown.is_empty() && !loading {
        let empty = if posts.is_empty() {
            "Nenhuma postagem ainda."
        } else {
            "Nenhuma postagem encontrada."
        };
        ui.label(RichText::new(empty).color(theme::MUTED));
    }
    let mut clicked = None;
    for post in shown {
        let opened = app.messages.contains_key(&post.id) || app.last_seen.contains_key(&post.id);
        let new = is_new(post, opened, now_ms);
        if post_card(app, ui, post, &forum.available_tags, new, now_ms) {
            clicked = Some(post.id.clone());
        }
    }
    if loading {
        ui.vertical_centered(|ui| ui.spinner());
    }
    clicked
}

/// One post: tags, title (+ "NOVO"), "Autor: prévia", reactions, message
/// count and age, with the first image as a thumbnail on the right.
fn post_card(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    post: &Channel,
    tags: &[ForumTag],
    new: bool,
    now_ms: i64,
) -> bool {
    const PAD: f32 = 16.0;
    const THUMB: f32 = 94.0;
    // The first message arrives with the post's page, or is the open
    // thread's oldest cached message.
    let first = app
        .forums
        .first_messages
        .get(&post.id)
        .cloned()
        .or_else(|| {
            app.messages
                .get(&post.id)
                .and_then(|messages| messages.first())
                .filter(|message| message.id == post.id)
                .cloned()
        });
    let thumbnail = first.as_ref().and_then(first_image);
    let width = ui.available_width();
    let top = ui.cursor().min;
    let background = ui.painter().add(egui::Shape::Noop);
    let text_width = width
        - 2.0 * PAD
        - if thumbnail.is_some() {
            THUMB + PAD
        } else {
            0.0
        };
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                top + Vec2::new(PAD, 12.0),
                Vec2::new(text_width, f32::INFINITY),
            ))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    content.spacing_mut().item_spacing.y = 6.0;

    let applied: Vec<&ForumTag> = tags
        .iter()
        .filter(|tag| post.applied_tags.contains(&tag.id))
        .collect();
    if !applied.is_empty() {
        content.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for tag in applied {
                pill(
                    app,
                    ui,
                    &tag_text(tag),
                    FontId::new(12.5, theme::bold()),
                    theme::TEXT,
                    tag_emoji(tag).as_deref(),
                    24.0,
                    |_, _| theme::SIDEBAR,
                );
            }
        });
    }
    content.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.scope(|ui| {
            ui.set_max_width(text_width - if new { 60.0 } else { 0.0 });
            crate::ui::emoji::label(
                app,
                ui,
                RichText::new(post.display_name())
                    .family(theme::bold())
                    .size(19.0)
                    .color(theme::TEXT),
                true,
            );
        });
        if new {
            pill(
                app,
                ui,
                "NOVO",
                FontId::new(11.0, theme::bold()),
                theme::WHITE,
                None,
                18.0,
                |_, _| theme::BLURPLE,
            );
        }
    });
    if let Some(first) = &first {
        let author = first.author.display_name();
        let mut job = egui::text::LayoutJob::default();
        job.append(
            author,
            0.0,
            egui::TextFormat::simple(
                FontId::new(14.0, theme::bold()),
                crate::util::name_color(author),
            ),
        );
        job.append(
            &format!(": {}", preview_text(app, first)),
            0.0,
            egui::TextFormat::simple(FontId::proportional(14.0), theme::MUTED),
        );
        content.add(egui::Label::new(job).truncate().selectable(false));
    }
    content.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if let Some(reaction) = first.as_ref().and_then(|first| first.reactions.first()) {
            let emoji = emoji_image(reaction.emoji.id.as_deref(), reaction.emoji.name.as_deref());
            let name = if emoji.is_some() {
                String::new()
            } else {
                reaction.emoji.name.clone().unwrap_or_default()
            };
            pill(
                app,
                ui,
                format!("{name} {}", reaction.count).trim(),
                FontId::proportional(14.0),
                theme::TEXT,
                emoji.as_deref(),
                26.0,
                |_, _| theme::SIDEBAR,
            );
            ui.add_space(4.0);
        }
        let (icon, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
        draw_bubble(ui.painter(), icon.center(), 6.0, theme::TEXT);
        ui.label(
            RichText::new(post.message_count.unwrap_or(0).to_string())
                .size(14.0)
                .color(theme::TEXT),
        );
        let last = post.last_message_id.as_deref().unwrap_or(&post.id);
        if let Some(ms) = crate::util::snowflake_ms(last) {
            ui.label(
                RichText::new(format!("·  {}", ago(ms, now_ms)))
                    .size(14.0)
                    .color(theme::MUTED),
            );
        }
    });

    let content_height = content.min_rect().bottom() + 12.0 - top.y;
    let thumb_height = if thumbnail.is_some() {
        THUMB + 2.0 * PAD
    } else {
        0.0
    };
    let rect = egui::Rect::from_min_size(top, Vec2::new(width, content_height.max(thumb_height)));
    let response = ui
        .allocate_rect(rect, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = fade(
        ui,
        response.id,
        response.hovered(),
        theme::CHAT.lerp_to_gamma(theme::HOVER, 0.5),
        theme::HOVER,
    );
    ui.painter().set(
        background,
        egui::epaint::RectShape::new(
            rect,
            theme::RADIUS_SM,
            fill,
            egui::Stroke::new(1.0, theme::DIVIDER),
            egui::StrokeKind::Inside,
        ),
    );
    if let Some(url) = thumbnail
        && let Some(texture) = app.images.get(ui.ctx(), &app.handle, &url)
    {
        // Center-cropped to a square, like Discord's.
        let size = texture.size_vec2();
        let side = size.x.min(size.y);
        let crop = Vec2::new(side / size.x, side / size.y);
        let uv = egui::Rect::from_center_size(pos2(0.5, 0.5), crop);
        let thumb = egui::Rect::from_center_size(
            pos2(rect.right() - PAD - THUMB / 2.0, rect.center().y),
            Vec2::splat(THUMB),
        );
        egui::Image::from_texture(&texture)
            .uv(uv)
            .corner_radius(theme::RADIUS_SM)
            .paint_at(ui, thumb);
    }
    response.clicked()
}

/// Right-click on a message: open the thread it started, or start one;
/// on our own messages, edit or delete them too.
pub(crate) fn message_menu(
    app: &mut VesktopApp,
    ui: &egui::Ui,
    row: &egui::Response,
    message: &Message,
) {
    // The row only senses hover (a click sense would swallow the links
    // inside it), so the right click is read from the input.
    let opened = row.hovered() && ui.input(|input| input.pointer.secondary_clicked());
    if !opened && !egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(row)) {
        return;
    }
    let thread = app.started_thread(message);
    let parent = app
        .find_channel(&message.channel_id)
        .filter(|channel| {
            matches!(
                channel.kind,
                CHANNEL_KIND_GUILD_TEXT | CHANNEL_KIND_ANNOUNCEMENT
            )
        })
        .cloned();
    let regular = matches!(message.kind, MESSAGE_KIND_DEFAULT | MESSAGE_KIND_REPLY);
    let can_start = parent.is_some() && regular;
    let own = regular && app.me.as_ref().is_some_and(|me| me.id == message.author.id);
    if thread.is_none() && !can_start && !own && !regular {
        return;
    }
    egui::Popup::menu(row)
        .open_memory(opened.then_some(egui::SetOpenCommand::Bool(true)))
        .at_pointer_fixed()
        .show(|ui| {
            if let Some(thread) = thread {
                if ui.button("Abrir tópico").clicked() {
                    app.open_thread(thread);
                }
            } else if can_start
                && let Some(parent) = parent
                && ui.button("Criar tópico").clicked()
            {
                start_draft(ui.ctx(), ThreadDraft::from_message(&parent, message));
            }
            if regular {
                ui.menu_button("Adicionar reação", |ui| {
                    let id = egui::Id::new("reaction_picker");
                    let (mut query, mut tab) = ui.ctx().data_mut(|data| {
                        data.get_temp::<(String, Option<emojis::Group>)>(id)
                            .unwrap_or_default()
                    });
                    let picked =
                        crate::ui::chat::emoji_picker(app, ui, &mut query, &mut tab, false);
                    ui.ctx().data_mut(|data| data.insert_temp(id, (query, tab)));
                    // Re-adding one of ours is a no-op on both ends.
                    if let Some(emoji) = picked {
                        app.toggle_reaction(&message.channel_id, &message.id, emoji, true);
                    }
                });
            }
            if own {
                let action = |delete| MessageAction {
                    channel_id: message.channel_id.clone(),
                    message_id: message.id.clone(),
                    content: message.content.clone(),
                    delete,
                };
                if ui.button("Editar mensagem").clicked() {
                    start_message_action(ui.ctx(), action(false));
                }
                if ui
                    .button(RichText::new("Apagar mensagem").color(theme::RED))
                    .clicked()
                {
                    start_message_action(ui.ctx(), action(true));
                }
            }
        });
}

/// "Editar mensagem" / "Apagar mensagem" on one of our messages, open in
/// a modal until saved, confirmed or cancelled.
#[derive(Clone)]
struct MessageAction {
    channel_id: String,
    message_id: String,
    content: String,
    delete: bool,
}

fn message_action_id() -> egui::Id {
    egui::Id::new("message_action")
}

fn start_message_action(ctx: &egui::Context, action: MessageAction) {
    ctx.data_mut(|data| data.insert_temp(message_action_id(), Some(action)));
}

/// The edit box or the delete confirmation while one is open.
pub(crate) fn message_action_window(app: &mut VesktopApp, ctx: &egui::Context) {
    let Some(mut action) = ctx.data(|data| {
        data.get_temp::<Option<MessageAction>>(message_action_id())
            .flatten()
    }) else {
        return;
    };
    let mut done = false;
    let modal = egui::Modal::new(message_action_id()).show(ctx, |ui| {
        ui.set_width(420.0);
        let title = if action.delete {
            "Apagar mensagem"
        } else {
            "Editar mensagem"
        };
        ui.label(
            RichText::new(title)
                .family(theme::bold())
                .size(18.0)
                .color(theme::TEXT),
        );
        ui.add_space(6.0);
        if action.delete {
            ui.label("Tem certeza que deseja apagar esta mensagem?");
            egui::Frame::new()
                .fill(theme::SIDEBAR)
                .corner_radius(theme::RADIUS_SM)
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(RichText::new(&action.content).color(theme::MUTED)).wrap(),
                    );
                });
        } else {
            // Enter saves, Shift+Enter breaks the line, as in the compose box.
            if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                done = true;
            }
            let response = ui.add(
                egui::TextEdit::multiline(&mut action.content)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            if ui.memory(|memory| memory.focused().is_none()) {
                response.request_focus();
            }
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let confirm = if action.delete {
                egui::Button::new(RichText::new("Apagar").color(theme::WHITE)).fill(theme::RED)
            } else {
                egui::Button::new(RichText::new("Salvar").color(theme::WHITE)).fill(theme::BLURPLE)
            };
            if ui.add(confirm).clicked() {
                done = true;
            }
            if ui.button("Cancelar").clicked() {
                ui.close();
            }
        });
    });
    // Emptying a message asks to delete it, as Discord does.
    let content = action.content.trim().to_string();
    if done && !action.delete && content.is_empty() {
        action.delete = true;
        done = false;
    } else if done {
        app.compose_error = None;
        app.send(if action.delete {
            Command::DeleteMessage {
                channel_id: action.channel_id.clone(),
                message_id: action.message_id.clone(),
            }
        } else {
            Command::EditMessage {
                channel_id: action.channel_id.clone(),
                message_id: action.message_id.clone(),
                content,
            }
        });
    }
    let open = !done && !modal.should_close();
    ctx.data_mut(|data| data.insert_temp(message_action_id(), open.then_some(action)));
}

/// Under a message that started a thread: its name and "N mensagens ›".
pub(crate) fn message_thread_summary(app: &mut VesktopApp, ui: &mut egui::Ui, message: &Message) {
    let Some(thread) = app.started_thread(message) else {
        return;
    };
    let frame = egui::Frame::new()
        .fill(theme::SIDEBAR)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(thread.display_name())
                        .family(theme::bold())
                        .color(theme::TEXT),
                );
                ui.label(
                    RichText::new(format!("{} ›", message_count_label(&thread)))
                        .color(theme::BLURPLE),
                );
            });
        });
    let response = ui
        .interact(
            frame.response.rect,
            egui::Id::new(("thread_summary", message.id.as_str())),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        app.open_thread(thread);
    }
}

/// "X iniciou um tópico: nome" (type 18) as a compact system line; true
/// when it painted the message.
pub(crate) fn paint_thread_created(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    message: &Message,
) -> bool {
    if message.kind != MESSAGE_KIND_THREAD_CREATED {
        return false;
    }
    let thread_id = message
        .message_reference
        .as_ref()
        .and_then(|reference| reference.channel_id.clone());
    let mut open = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(16.0 + 40.0 - 22.0);
        let (icon, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
        draw_threads_icon(ui.painter(), icon.center(), theme::MUTED);
        ui.add_space(12.0);
        ui.label(
            RichText::new(message.author.display_name())
                .family(theme::bold())
                .color(theme::TEXT),
        );
        ui.label(RichText::new(" iniciou um tópico: ").color(theme::MUTED));
        open = ui
            .link(
                RichText::new(&message.content)
                    .family(theme::bold())
                    .color(theme::TEXT),
            )
            .clicked();
        ui.add_space(8.0);
        ui.label(
            RichText::new(crate::util::header_time(&message.timestamp).0)
                .size(11.5)
                .color(theme::MUTED),
        );
    });
    if let (true, Some(thread_id)) = (open, thread_id) {
        let thread = app.find_channel(&thread_id).cloned().or_else(|| {
            // Archived and never fetched: enough to open its chat.
            serde_json::from_value(serde_json::json!({
                "id": thread_id,
                "type": CHANNEL_KIND_PUBLIC_THREAD,
                "name": message.content,
                "parent_id": message.channel_id,
                "thread_metadata": { "archived": true },
            }))
            .ok()
        });
        if let Some(thread) = thread {
            app.open_thread(thread);
        }
    }
    true
}

/// A thread's first message (type 21) only points at the parent message;
/// show that message in its place, as Discord does.
pub(crate) fn starter_as_original(message: Message) -> Message {
    if message.kind != MESSAGE_KIND_THREAD_STARTER {
        return message;
    }
    match message.referenced_message {
        Some(original) => Message {
            id: message.id,
            channel_id: message.channel_id,
            kind: MESSAGE_KIND_DEFAULT,
            message_reference: None,
            referenced_message: None,
            thread: None,
            ..*original
        },
        None => Message {
            referenced_message: None,
            ..message
        },
    }
}

#[derive(Clone, Default)]
struct ThreadDraft {
    channel_id: String,
    message_id: Option<String>,
    name: String,
    content: String,
    kind: i64,
    forum: bool,
}

impl ThreadDraft {
    /// From the browser or a forum: name plus a first message.
    fn standalone(parent: &Channel) -> Self {
        Self {
            channel_id: parent.id.clone(),
            kind: if parent.kind == CHANNEL_KIND_ANNOUNCEMENT {
                CHANNEL_KIND_ANNOUNCEMENT_THREAD
            } else {
                CHANNEL_KIND_PUBLIC_THREAD
            },
            forum: parent.is_forum(),
            ..Default::default()
        }
    }

    /// From a message: named after its text, like Discord's default.
    fn from_message(parent: &Channel, message: &Message) -> Self {
        let text = message.content.replace('\n', " ");
        Self {
            message_id: Some(message.id.clone()),
            name: text.trim().chars().take(40).collect(),
            ..Self::standalone(parent)
        }
    }
}

fn draft_id() -> egui::Id {
    egui::Id::new("thread_draft")
}

fn start_draft(ctx: &egui::Context, draft: ThreadDraft) {
    ctx.data_mut(|data| data.insert_temp(draft_id(), Some(draft)));
}

/// The "Criar tópico" / "Nova postagem" window while a draft is open.
pub(crate) fn draft_window(app: &mut VesktopApp, ctx: &egui::Context) {
    let Some(mut draft) =
        ctx.data(|data| data.get_temp::<Option<ThreadDraft>>(draft_id()).flatten())
    else {
        return;
    };
    let mut open = true;
    let mut create = false;
    let title = if draft.forum {
        "Nova postagem"
    } else {
        "Criar tópico"
    };
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label("Nome do tópico");
            ui.add(egui::TextEdit::singleline(&mut draft.name).char_limit(NAME_MAX));
            if draft.message_id.is_none() {
                ui.label(if draft.forum {
                    "Mensagem"
                } else {
                    "Primeira mensagem (opcional)"
                });
                ui.add(egui::TextEdit::multiline(&mut draft.content).desired_rows(3));
            }
            let ready =
                !draft.name.trim().is_empty() && (!draft.forum || !draft.content.trim().is_empty());
            if ui.add_enabled(ready, egui::Button::new("Criar")).clicked() {
                create = true;
            }
        });
    if create {
        app.compose_error = None;
        app.send(Command::CreateThread {
            channel_id: draft.channel_id.clone(),
            message_id: draft.message_id.clone(),
            name: draft.name.trim().to_string(),
            content: draft.content.trim().to_string(),
            kind: draft.kind,
            forum: draft.forum,
        });
    }
    let next = (open && !create).then_some(draft);
    ctx.data_mut(|data| data.insert_temp(draft_id(), next));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str, parent: &str, last: Option<&str>, joined: bool, archived: bool) -> Channel {
        let mut thread: Channel = serde_json::from_value(serde_json::json!({
            "id": id,
            "type": CHANNEL_KIND_PUBLIC_THREAD,
            "parent_id": parent,
            "last_message_id": last,
            "thread_metadata": { "archived": archived },
        }))
        .unwrap();
        thread.member = joined.then(ThreadMember::default);
        thread
    }

    #[test]
    fn sidebar_nests_joined_active_threads_by_recent_activity() {
        let channels = vec![
            thread("10", "1", Some("50"), true, false),
            thread("11", "1", Some("900"), true, false),
            thread("12", "1", None, false, false),
            thread("13", "1", None, true, true),
            thread("14", "2", None, true, false),
        ];
        let ids = |list: Vec<&Channel>| list.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(sidebar_threads(&channels, "1", None)), ["11", "10"]);
        // The open thread shows even when unjoined or archived.
        assert_eq!(
            ids(sidebar_threads(&channels, "1", Some("13"))),
            ["11", "10", "13"]
        );
        let (active, archived) = channel_threads(&channels, "1");
        assert_eq!(active.len(), 3);
        assert_eq!(archived[0].id, "13");
    }

    #[test]
    fn list_sync_replaces_active_threads_in_scope_and_keeps_membership() {
        let mut list = vec![
            thread("10", "1", None, true, false),
            thread("11", "1", None, true, false),
            thread("12", "2", None, true, false),
        ];
        // Channel 1 now has only thread 11, updated without `member`.
        sync_threads(
            &mut list,
            Some(&["1".to_string()]),
            vec![thread("11", "1", Some("99"), false, false)],
        );
        let get = |id: &str| list.iter().find(|c| c.id == id).unwrap();
        assert!(get("10").is_archived());
        assert!(!get("11").is_archived() && get("11").member.is_some());
        assert_eq!(get("11").last_message_id.as_deref(), Some("99"));
        assert!(!get("12").is_archived());
        sync_threads(&mut list, None, Vec::new());
        assert!(list.iter().all(Channel::is_archived));
    }

    #[test]
    fn thread_list_attaches_joined_members() {
        let list: crate::model::ThreadList = serde_json::from_value(serde_json::json!({
            "threads": [
                { "id": "10", "type": 11, "parent_id": "1" },
                { "id": "11", "type": 11, "parent_id": "1" }
            ],
            "members": [{ "id": "11", "user_id": "9", "flags": 1 }],
            "has_more": false
        }))
        .unwrap();
        let threads = list.into_threads();
        assert!(threads[0].member.is_none());
        assert!(threads[1].member.is_some());
    }

    #[test]
    fn forum_search_page_parses_and_filters_by_title_and_tags() {
        let page: crate::model::ThreadSearch = serde_json::from_value(serde_json::json!({
            "threads": [
                { "id": "10", "type": 11, "parent_id": "1", "name": "Erro TikTok",
                  "applied_tags": ["7"] },
                { "id": "11", "type": 11, "parent_id": "1", "name": "Frete Amazon" }
            ],
            "members": [{ "id": "11", "user_id": "9", "flags": 1 }],
            "first_messages": [{
                "id": "10", "channel_id": "10", "content": "oi",
                "author": { "id": "9", "username": "gael" },
                "reactions": [{ "count": 2, "emoji": { "id": null, "name": "👍" } }]
            }],
            "has_more": true,
            "total_results": 40
        }))
        .unwrap();
        assert!(page.has_more);
        assert_eq!(page.first_messages[0].reactions[0].count, 2);
        let posts = page.list.into_threads();
        assert!(posts[1].member.is_some());
        assert!(post_matches(&posts[0], " tiktok", &[]));
        assert!(!post_matches(&posts[1], "tiktok", &[]));
        let tag = ["7".to_string()];
        assert!(post_matches(&posts[0], "", &tag));
        assert!(!post_matches(&posts[1], "", &tag));
    }

    #[test]
    fn age_labels_and_new_badge() {
        let now = 1_800_000_000_000;
        assert_eq!(ago(now - 30_000, now), "Agora");
        assert_eq!(ago(now - 5 * MINUTE_MS, now), "Há 5 min");
        assert_eq!(ago(now - 3 * 60 * MINUTE_MS, now), "Há 3h");
        assert_eq!(ago(now - DAY_MS, now), "Há 1d");
        assert_eq!(ago(now - 65 * DAY_MS, now), "Há 2 meses");
        // A snowflake made a day before `now`.
        let id = (((now - DAY_MS - 1_420_070_400_000) as u64) << 22).to_string();
        let post = thread(&id, "1", None, false, false);
        assert!(is_new(&post, false, now));
        assert!(!is_new(&post, true, now));
        assert!(!is_new(&post, false, now + 3 * DAY_MS));
        assert!(!is_new(&thread(&id, "1", None, true, false), false, now));
    }
}
