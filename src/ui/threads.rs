//! Threads (channel types 10/11/12) and forum posts: the state the gateway
//! keeps for them, their rows under the parent channel, the "Tópicos"
//! browser, forum channels and the "Criar tópico" dialog.
//!
//! Threads live in `guild_channels` next to their parents, so the chat,
//! mentions and unread routing treat them as ordinary channels. Active
//! ones come from the gateway; archived ones only from the browser's REST
//! fetch.

use egui::{RichText, Sense, Vec2, pos2};

use crate::app::{ChannelRef, VesktopApp};
use crate::backend::{Command, UiEvent};
use crate::model::{
    CHANNEL_KIND_ANNOUNCEMENT, CHANNEL_KIND_ANNOUNCEMENT_THREAD, CHANNEL_KIND_GUILD_TEXT,
    CHANNEL_KIND_PUBLIC_THREAD, Channel, MESSAGE_KIND_DEFAULT, MESSAGE_KIND_THREAD_CREATED,
    MESSAGE_KIND_THREAD_STARTER, Message, ThreadMember,
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
        crate::ui::emoji::label(app, &mut content, text, true);
        if response.clicked() {
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

/// A forum channel: its posts instead of a chat, plus "Nova postagem".
pub(crate) fn paint_forum(app: &mut VesktopApp, ui: &mut egui::Ui, forum_id: &str) {
    // Archived posts come over REST, once per forum and session.
    let loaded_id = egui::Id::new("forum_threads_loaded");
    let first_visit = ui.ctx().data_mut(|data| {
        data.get_temp_mut_or_default::<std::collections::HashSet<String>>(loaded_id)
            .insert(forum_id.to_string())
    });
    if first_visit {
        app.send(Command::LoadThreads {
            channel_id: forum_id.to_string(),
        });
    }
    let Some(forum) = app.find_channel(forum_id).cloned() else {
        return;
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.add_space(16.0);
        ui.label(
            RichText::new(format!("# {}", forum.display_name()))
                .family(theme::bold())
                .size(16.0)
                .color(theme::TEXT),
        );
        if let Some(topic) = forum.topic.as_deref().filter(|topic| !topic.is_empty()) {
            ui.add_space(10.0);
            ui.label(RichText::new(topic).small().color(theme::MUTED));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(14.0);
            if ui.button("Nova postagem").clicked() {
                start_draft(ui.ctx(), ThreadDraft::standalone(&forum));
            }
        });
    });
    ui.separator();
    if let Some(error) = &app.compose_error {
        ui.label(RichText::new(error).small().color(theme::RED));
    }
    let mut clicked = None;
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(16, 8))
        .show(ui, |ui| {
            clicked = thread_list(app, ui, forum_id, f32::INFINITY);
        });
    if let Some(id) = clicked {
        app.select_channel(id);
    }
}

/// Right-click on a message: open the thread it started, or start one.
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
    let can_start =
        parent.is_some() && matches!(message.kind, MESSAGE_KIND_DEFAULT | MESSAGE_KIND_REPLY);
    if thread.is_none() && !can_start {
        return;
    }
    egui::Popup::menu(row)
        .open_memory(opened.then_some(egui::SetOpenCommand::Bool(true)))
        .at_pointer_fixed()
        .show(|ui| match (thread, parent) {
            (Some(thread), _) => {
                if ui.button("Abrir tópico").clicked() {
                    app.open_thread(thread);
                }
            }
            (None, Some(parent)) => {
                if ui.button("Criar tópico").clicked() {
                    start_draft(ui.ctx(), ThreadDraft::from_message(&parent, message));
                }
            }
            (None, None) => {}
        });
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
}
