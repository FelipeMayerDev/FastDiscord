//! Central chat view: channel header, Discord-style timeline (date
//! separators, grouped messages, replies, new-messages divider) and the
//! multiline compose box.

use std::collections::HashMap;

use chrono::DateTime;
use egui::text::LayoutJob;
use egui::{
    Align, Align2, Color32, FontId, Image, Label, RichText, ScrollArea, Sense, Shape, TextEdit,
    TextFormat, Vec2, pos2,
};

use crate::app::{ChannelRef, VesktopApp};
use crate::markup::{self, Style};
use crate::model::{GuildEmoji, Message, ReactionEmoji, User};
use crate::theme;
use crate::ui::channel_sidebar::draw_search_icon;
use crate::ui::image_viewer::ImageViewer;
use crate::ui::{fade, round_avatar};
use crate::util;

/// Left margin, avatar width and the content indent they add up to.
const AVATAR: f32 = 40.0;
const LEFT: f32 = 16.0;
const GAP: f32 = 12.0;
const CONTENT: f32 = LEFT + AVATAR + GAP;
/// Column where a reply's mini avatar hangs.
const REPLY_AVATAR_X: f32 = LEFT + AVATAR - 8.0;
/// Inline images never grow beyond this.
const INLINE_MAX: f32 = 420.0;
const GROUPING_MINUTES: i64 = 7;
const JUMP_HIGHLIGHT_SECS: f32 = 2.0;

#[derive(Clone, Default)]
struct ChatTools {
    search_open: bool,
    search: String,
    emoji_query: String,
    /// Emoji picker tab: a Unicode group, `None` for the servers' emojis.
    emoji_tab: Option<emojis::Group>,
    gif_query: String,
    /// The query last sent to the GIF endpoints, and when the field last
    /// changed (typing is debounced).
    gif_requested: Option<String>,
    gif_edited: f64,
    attachment_open: bool,
    attachment_path: String,
}

fn active_search(tools: &ChatTools) -> String {
    if tools.search_open {
        tools.search.trim().to_lowercase()
    } else {
        String::new()
    }
}

pub fn attachment_selected(ctx: &egui::Context, channel_id: &str, path: &std::path::Path) {
    let id = egui::Id::new(("chat_tools", channel_id));
    ctx.data_mut(|data| {
        let tools = data.get_temp_mut_or_default::<ChatTools>(id);
        tools.attachment_path = path.to_string_lossy().into_owned();
        tools.attachment_open = true;
    });
    ctx.request_repaint();
}

pub fn paint(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let Some(channel_id) = app.selected_channel.clone() else {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Nenhuma conversa selecionada")
                        .size(20.0)
                        .family(theme::bold())
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("Escolha um servidor ou uma conversa à esquerda.")
                        .color(theme::MUTED),
                );
            });
        });
        return;
    };
    crate::ui::threads::draft_window(app, ui.ctx());
    crate::ui::threads::message_action_window(app, ui.ctx());
    if app
        .find_channel(&channel_id)
        .is_some_and(crate::model::Channel::is_forum)
    {
        crate::ui::threads::paint_forum(app, ui, &channel_id);
        return;
    }

    let tools_id = egui::Id::new(("chat_tools", &channel_id));
    let mut tools = ui
        .ctx()
        .data_mut(|data| data.get_temp::<ChatTools>(tools_id).unwrap_or_default());
    let name = app.channel_name(&channel_id);
    let topic = app.channel_topic(&channel_id);
    let is_dm = matches!(app.channel_index.get(&channel_id), Some(ChannelRef::Dm));
    let prefix = if is_dm { "@" } else { "#" };

    // Header, Discord-style: avatar plus name on a DM (with presence), the
    // channel name elsewhere, and the search icon on the right edge.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.add_space(14.0);
        if is_dm {
            let dm = app
                .dm_channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .cloned();
            if let Some(dm) = dm {
                let texture = util::dm_icon_url(&dm)
                    .and_then(|url| app.images.get(ui.ctx(), &app.handle, &url));
                let (avatar_rect, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                match texture {
                    Some(texture) => {
                        egui::Image::new((texture.id(), avatar_rect.size()))
                            .corner_radius(egui::CornerRadius::same(12))
                            .paint_at(ui, avatar_rect);
                    }
                    None if dm.dm_partner().is_none() => {
                        crate::ui::paint_group_avatar(ui.painter(), avatar_rect)
                    }
                    None => {
                        ui.painter().circle_filled(
                            avatar_rect.center(),
                            12.0,
                            util::name_color(&name),
                        );
                    }
                }
                if let Some(partner) = dm.dm_partner() {
                    crate::ui::presence_dot(
                        ui,
                        avatar_rect,
                        app.presence.get(&partner.id).map(String::as_str),
                        app.presence_device.get(&partner.id).map(String::as_str),
                        theme::CHAT,
                    );
                }
                ui.add_space(8.0);
            }
            crate::ui::emoji::label(
                app,
                ui,
                RichText::new(&name)
                    .family(theme::bold())
                    .size(16.0)
                    .color(theme::TEXT),
                false,
            );
            if let Some(partner_status) = app
                .dm_channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .and_then(|channel| channel.dm_partner())
                .and_then(|partner| app.presence.get(&partner.id))
            {
                ui.add_space(8.0);
                let (dot, label) = match partner_status.as_str() {
                    "online" => (theme::GREEN, "Online"),
                    "idle" => (theme::YELLOW, "Ausente"),
                    "dnd" => (theme::RED, "Não perturbe"),
                    _ => (theme::MUTED, "Offline"),
                };
                ui.label(RichText::new("●").size(10.0).color(dot));
                ui.add_space(3.0);
                ui.label(RichText::new(label).small().color(theme::MUTED));
            }
        } else {
            ui.add_space(2.0);
            crate::ui::emoji::label(
                app,
                ui,
                RichText::new(format!("{prefix} {name}"))
                    .family(theme::bold())
                    .size(16.0)
                    .color(theme::TEXT),
                false,
            );
            if let Some(topic) = &topic {
                ui.add_space(10.0);
                let channel_names = app.channel_names();
                let runs = content_runs_sized(topic, &app.user_cache, &channel_names, 12.5);
                paint_runs_with(app, ui, runs, false);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(14.0);
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
            ui.painter()
                .rect_filled(rect, 4.0, crate::ui::control_bg(ui, &response, false));
            draw_search_icon(ui.painter(), rect.center(), theme::TEXT);
            if response.on_hover_text("Buscar nesta conversa").clicked() {
                tools.search_open = !tools.search_open;
            }
            ui.add_space(8.0);
            crate::ui::members::toggle(ui);
            if !is_dm {
                ui.add_space(8.0);
                crate::ui::threads::header_button(app, ui, &channel_id);
            }
        });
    });
    ui.separator();
    crate::ui::members::panel(app, ui, &channel_id);

    if tools.search_open {
        ui.horizontal(|ui| {
            ui.add(
                TextEdit::singleline(&mut tools.search)
                    .hint_text("Buscar nas mensagens carregadas"),
            );
            if ui.button("Limpar").clicked() {
                tools.search.clear();
            }
        });
    }
    // The compose box goes in first as a bottom panel: the message list's
    // ScrollArea takes all remaining height, so anything after it is clipped.
    let typers = app.typers(&channel_id);
    egui::Panel::bottom("compose")
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            ui.add_space(6.0);
            if !typers.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.label(
                        RichText::new(typing_label(&typers))
                            .small()
                            .color(theme::MUTED),
                    );
                });
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs(1));
            }
            if let Some(error) = &app.compose_error {
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.label(RichText::new(error).small().color(theme::RED));
                });
                ui.add_space(2.0);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                ui.add_space(14.0);
                let width = ui.available_width();
                let hint = if name.is_empty() {
                    "Enviar uma mensagem".to_string()
                } else {
                    format!("Conversar em {prefix}{name}")
                };
                let mut send = false;
                egui::Frame::default()
                    .fill(theme::INPUT)
                    .corner_radius(theme::RADIUS_MD)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(width - 28.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::ZERO;
                            // Attachment plus, inside the box like Discord's.
                            let (plus_rect, plus) =
                                ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                            ui.painter().rect_filled(
                                plus_rect,
                                4.0,
                                crate::ui::control_bg(ui, &plus, false),
                            );
                            let pc = plus_rect.center();
                            let stroke = egui::Stroke::new(1.8, theme::TEXT);
                            ui.painter().line_segment(
                                [pos2(pc.x - 6.0, pc.y), pos2(pc.x + 6.0, pc.y)],
                                stroke,
                            );
                            ui.painter().line_segment(
                                [pos2(pc.x, pc.y - 6.0), pos2(pc.x, pc.y + 6.0)],
                                stroke,
                            );
                            if plus.on_hover_text("Enviar arquivo").clicked() {
                                tools.attachment_open = true;
                            }
                            ui.add_space(6.0);

                            let edit = TextEdit::multiline(&mut app.compose)
                                .hint_text(hint)
                                .desired_rows(1)
                                .desired_width(ui.available_width() - 76.0)
                                .frame(egui::Frame::NONE)
                                .show(ui);
                            // Enter sends, Shift+Enter breaks the line.
                            let enter = ui.ctx().input(|input| {
                                input.key_pressed(egui::Key::Enter) && !input.modifiers.shift
                            });
                            if edit.response.has_focus() && enter {
                                send = true;
                            }

                            // Right cluster: GIF and emoji, Discord-style.
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                                    let (smile_rect, smile) =
                                        ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                                    ui.painter().rect_filled(
                                        smile_rect,
                                        4.0,
                                        crate::ui::control_bg(ui, &smile, false),
                                    );
                                    let motion = crate::ui::animate_bool(
                                        ui.ctx(),
                                        smile.id.with("smile_motion"),
                                        smile.hovered(),
                                        0.12,
                                    );
                                    draw_smiley(
                                        ui.painter(),
                                        smile_rect.center() - Vec2::new(0.0, motion),
                                        theme::TEXT,
                                    );
                                    let smile = smile.on_hover_text("Inserir emoji");
                                    picker_popup(&smile, "emoji_picker").show(|ui| {
                                        if let Some(emoji) = emoji_picker(
                                            app,
                                            ui,
                                            &mut tools.emoji_query,
                                            &mut tools.emoji_tab,
                                            smile.clicked(),
                                        ) {
                                            app.compose.push_str(&emoji_markup(&emoji));
                                        }
                                    });
                                    let (gif_rect, gif) = ui
                                        .allocate_exact_size(Vec2::new(30.0, 24.0), Sense::click());
                                    ui.painter().rect_filled(
                                        gif_rect,
                                        4.0,
                                        crate::ui::control_bg(ui, &gif, false),
                                    );
                                    ui.painter().text(
                                        gif_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "GIF",
                                        egui::FontId::proportional(12.0),
                                        theme::TEXT,
                                    );
                                    let gif = gif.on_hover_text("Enviar GIF");
                                    picker_popup(&gif, "gif_picker").show(|ui| {
                                        gif_picker(app, ui, &mut tools, &channel_id, gif.clicked());
                                    });
                                },
                            );
                        });
                    });
                // Typing signal, throttled inside note_typing.
                if app.compose != app.last_compose {
                    app.last_compose = app.compose.clone();
                    app.note_typing();
                }
                if send {
                    app.send_current_message();
                }
                ui.add_space(14.0);
            });
            ui.add_space(10.0);
        });

    let ctx = ui.ctx().clone();
    let mut upload = false;
    egui::Window::new("Enviar arquivo")
        .collapsible(false)
        .open(&mut tools.attachment_open)
        .show(&ctx, |ui| {
            if ui
                .add_enabled(
                    !app.file_picker_open && !app.attachment_sending,
                    egui::Button::new("Escolher arquivo…"),
                )
                .clicked()
            {
                app.choose_attachment();
            }
            ui.label("Arquivo selecionado (até 10 MB)");
            ui.text_edit_singleline(&mut tools.attachment_path);
            if ui
                .add_enabled(
                    !app.attachment_sending && !tools.attachment_path.trim().is_empty(),
                    egui::Button::new("Enviar"),
                )
                .clicked()
            {
                upload = true;
            }
        });
    if upload {
        app.send_attachment(std::path::PathBuf::from(tools.attachment_path.trim()));
        tools.attachment_open = false;
    }
    ui.ctx()
        .data_mut(|data| data.insert_temp(tools_id, tools.clone()));

    if app.attachment_sending {
        ui.label("Enviando arquivo…");
    }
    let channel_names = app.channel_names();
    let mut load_older = false;
    let first_unread = app.first_unread.get(&channel_id).cloned();
    // The frame the view lands on the divider isn't "read" yet, even though
    // it still scrolls from the bottom; Discord keeps the divider until you
    // actually reach the present.
    let mut landed_on_unread = false;

    let scroll = ScrollArea::vertical()
        .auto_shrink(false)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.add_space(8.0);

            let has_more = app.has_more.get(&channel_id).copied().unwrap_or(false);
            if has_more {
                ui.vertical_centered(|ui| {
                    if app.loading_channels.contains(&channel_id) {
                        ui.label(RichText::new("Carregando…").small().color(theme::MUTED));
                    } else if ui.button("Carregar mensagens anteriores").clicked() {
                        load_older = true;
                    }
                });
                ui.add_space(8.0);
            } else {
                conversation_start(app, ui, &channel_id);
            }

            // Cloned so message painting can take `&mut app` (image cache).
            let needle = active_search(&tools);
            let list: Vec<_> = app
                .messages
                .get(&channel_id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(crate::ui::threads::starter_as_original)
                .filter(|message| {
                    needle.is_empty()
                        || message.content.to_lowercase().contains(&needle)
                        || message
                            .author
                            .display_name()
                            .to_lowercase()
                            .contains(&needle)
                })
                .collect();
            if !needle.is_empty() && list.is_empty() {
                ui.label("Nenhuma mensagem carregada corresponde à busca.");
            }
            for (index, message) in list.iter().enumerate() {
                let previous = if index > 0 {
                    Some(&list[index - 1])
                } else {
                    None
                };
                let day_change = previous
                    .map(|prev| {
                        util::local_day(&prev.timestamp) != util::local_day(&message.timestamp)
                    })
                    .unwrap_or(true);
                let grouped = needle.is_empty()
                    && !day_change
                    && previous.is_some_and(|prev| can_group(prev, message));

                if day_change && !message.timestamp.is_empty() {
                    date_separator(ui, &util::day_label_long(&message.timestamp));
                }
                if first_unread.as_deref() == Some(message.id.as_str()) {
                    let divider = new_messages_divider(ui);
                    if app.unread_jump.remove(channel_id.as_str()) {
                        // Land with the divider at the top, first unread
                        // message right below it, like Discord.
                        ui.scroll_to_rect(divider, Some(Align::Min));
                        landed_on_unread = true;
                    }
                }
                // Discord breathes between message groups: a new author or a
                // reply opens a taller gap than a grouped continuation.
                if index > 0 && !grouped {
                    ui.add_space(12.0);
                } else if index > 0 {
                    ui.add_space(4.0);
                }
                paint_message(app, ui, message, grouped, &channel_names);
            }

            if app.jump_to_present {
                app.jump_to_present = false;
                ui.scroll_to_cursor(Some(Align::BOTTOM));
            }
        });

    let at_bottom =
        (scroll.state.offset.y + scroll.inner_rect.height()) >= scroll.content_size.y - 1.0;
    let detached = app.detached.contains(&channel_id);
    if at_bottom && !detached && !landed_on_unread && active_search(&tools).is_empty() {
        if let Some(last) = app
            .messages
            .get(&channel_id)
            .and_then(|list| list.last())
            .map(|message| message.id.clone())
        {
            app.mark_channel_read(&channel_id, last);
        }
    } else if active_search(&tools).is_empty() {
        // "Ir para o presente", pinned above the compose box.
        let members = if crate::ui::members::shown(ui.ctx()) {
            crate::ui::members::WIDTH
        } else {
            0.0
        };
        egui::Area::new(egui::Id::new("jump_to_present"))
            .anchor(Align2::RIGHT_BOTTOM, [-24.0 - members, -96.0])
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                if ui
                    .button(RichText::new("Ir para o presente").color(theme::TEXT))
                    .on_hover_text("Pular para as mensagens mais recentes")
                    .clicked()
                {
                    app.return_to_present(&channel_id);
                }
            });
    }

    if load_older {
        app.load_older_messages();
    }
}

/// Discord groups consecutive messages from the same author within 7
/// minutes, as long as the later one isn't a reply or a system message.
fn can_group(previous: &Message, message: &Message) -> bool {
    if message.message_reference.is_some() || message.is_system() {
        return false;
    }
    if previous.author.id != message.author.id {
        return false;
    }
    let gap = (|| {
        let start = DateTime::parse_from_rfc3339(&previous.timestamp).ok()?;
        let end = DateTime::parse_from_rfc3339(&message.timestamp).ok()?;
        Some(end.signed_duration_since(start))
    })();
    matches!(gap, Some(span) if span.num_minutes() < GROUPING_MINUTES)
}

fn paint_message(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    message: &Message,
    grouped: bool,
    channel_names: &HashMap<String, String>,
) {
    if crate::ui::threads::paint_thread_created(app, ui, message) {
        return;
    }
    let row_top = ui.cursor().top();
    let row_left = ui.cursor().left();
    let author_name = message.author.display_name();
    // Row background (hover fade, mention highlight) goes under the content:
    // reserve its slot now, fill it once the row's height is known.
    let background = ui.painter().add(Shape::Noop);
    let mentions_me = message.mention_everyone
        || app
            .me
            .as_ref()
            .is_some_and(|me| message.mentions.iter().any(|user| user.id == me.id));

    let mut reply_band: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        if message.message_reference.is_some() {
            let top = ui.cursor().top();
            reply_excerpt(app, ui, message);
            let bottom = ui.cursor().top();
            reply_band = Some(egui::Rect::from_min_max(
                pos2(row_left + LEFT, top),
                pos2(ui.max_rect().right(), bottom),
            ));
        }

        if grouped {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(CONTENT);
                ui.vertical(|ui| {
                    paint_content(app, ui, message, channel_names);
                });
            });
        } else {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(LEFT);
                // Animated avatars play on hover, like Discord; the still
                // one stays up while the GIF loads.
                let hovered = ui.rect_contains_pointer(egui::Rect::from_min_size(
                    ui.cursor().min,
                    Vec2::splat(AVATAR),
                ));
                let animated = util::animated_avatar_url(&message.author)
                    .filter(|_| hovered)
                    .and_then(|url| app.images.get(ui.ctx(), &app.handle, &url));
                let texture = animated.or_else(|| {
                    let avatar_url = util::user_avatar_url(&message.author);
                    app.images.get(ui.ctx(), &app.handle, &avatar_url)
                });
                let avatar = round_avatar(
                    ui,
                    texture.as_ref(),
                    AVATAR,
                    author_name,
                    util::name_color(author_name),
                );
                crate::ui::presence_dot(
                    ui,
                    avatar,
                    app.presence.get(&message.author.id).map(String::as_str),
                    app.presence_device
                        .get(&message.author.id)
                        .map(String::as_str),
                    theme::CHAT,
                );
                ui.add_space(GAP);
                ui.vertical(|ui| {
                    // Author line: name, then the clan tag chip and the
                    // time right after it, small muted text, Discord-style.
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        ui.add(
                            egui::Label::new(
                                RichText::new(author_name)
                                    .family(theme::bold())
                                    .size(15.0)
                                    .color(util::name_color(author_name)),
                            )
                            .truncate(),
                        );
                        if let Some(clan) = message.author.clan_tag() {
                            ui.add_space(6.0);
                            crate::ui::clan_tag(app, ui, clan);
                        }
                        ui.add_space(8.0);
                        let (label, exact) = util::header_time(&message.timestamp);
                        if !label.is_empty() {
                            ui.label(RichText::new(label).size(11.5).color(theme::MUTED))
                                .on_hover_text(exact);
                        }
                        if message.edited_timestamp.is_some() {
                            ui.add_space(6.0);
                            ui.label(RichText::new("(editada)").size(11.0).color(theme::MUTED));
                        }
                    });
                    paint_content(app, ui, message, channel_names);
                });
            });
        }
    });

    // Discord highlights the whole row with a subtle overlay.
    let row = egui::Rect::from_min_max(
        pos2(row_left, row_top),
        pos2(ui.max_rect().right(), ui.cursor().top()),
    );
    let response = ui.interact(
        row,
        egui::Id::new(("msg_row", message.id.as_str())),
        Sense::hover(),
    );
    crate::ui::threads::message_menu(app, ui, &response, message);
    let hover = fade(
        ui,
        response.id,
        response.hovered(),
        Color32::TRANSPARENT,
        theme::ROW_HOVER,
    );
    let mut base = if mentions_me {
        theme::MENTIONED_BG
    } else {
        Color32::TRANSPARENT
    };
    if let Some((_, jumped)) = app.reply_jump.as_mut().filter(|(id, _)| *id == message.id) {
        let at = *jumped.get_or_insert_with(|| {
            ui.scroll_to_rect(row, Some(Align::Center));
            std::time::Instant::now()
        });
        // The jumped-to message flashes, then fades back over 2 s.
        let left = 1.0 - at.elapsed().as_secs_f32() / JUMP_HIGHLIGHT_SECS;
        if left > 0.0 {
            base = theme::BLURPLE.gamma_multiply(0.25 * left);
            ui.ctx().request_repaint();
        } else {
            app.reply_jump = None;
        }
    }
    ui.painter()
        .set(background, Shape::rect_filled(row, 0.0, base.blend(hover)));
    if mentions_me {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(row.min, Vec2::new(2.0, row.height())),
            0.0,
            theme::MENTIONED_BAR,
        );
    }
    if response.hovered() {
        // Grouped messages reveal their time in the left gutter.
        if grouped {
            ui.painter().text(
                pos2(row_left + LEFT + AVATAR, row_top + 3.0),
                Align2::RIGHT_TOP,
                util::message_time(&message.timestamp),
                FontId::proportional(11.0),
                theme::MUTED,
            );
        }
    }

    // Curved connector from the author's avatar up to the reply excerpt.
    if let Some(band) = reply_band {
        let start = pos2(row_left + REPLY_AVATAR_X + 8.0, band.bottom() + 4.0);
        let end = pos2(row_left + REPLY_AVATAR_X, band.center().y);
        ui.painter().add(Shape::CubicBezier(
            egui::epaint::CubicBezierShape::from_points_stroke(
                [
                    start,
                    pos2(start.x, band.center().y + 2.0),
                    pos2(end.x + 8.0, band.center().y),
                    end,
                ],
                false,
                Color32::TRANSPARENT,
                egui::Stroke::new(1.5, theme::MUTED),
            ),
        ));
    }
}

fn reply_excerpt(app: &mut VesktopApp, ui: &mut egui::Ui, message: &Message) {
    let original = message.referenced_message.as_deref();
    let excerpt = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(REPLY_AVATAR_X);
        let (name, texture, color) = match original {
            Some(original) => {
                let name = original.author.display_name();
                let url = util::user_avatar_url(&original.author);
                (
                    name.to_string(),
                    app.images.get(ui.ctx(), &app.handle, &url),
                    util::name_color(name),
                )
            }
            None => ("Mensagem original".to_string(), None, theme::MUTED),
        };
        round_avatar(ui, texture.as_ref(), 16.0, &name, color);
        ui.add_space(6.0);
        ui.label(
            RichText::new(&name)
                .small()
                .family(theme::bold())
                .color(util::name_color(&name)),
        );
        let excerpt = original.map(excerpt_text).unwrap_or_default();
        if !excerpt.is_empty() {
            ui.label(RichText::new(excerpt).small().color(theme::MUTED));
        }
    });
    // Like Discord, a click jumps to the original (same channel only).
    let reference = message.message_reference.as_ref();
    let target = reference
        .filter(|r| {
            r.channel_id
                .as_ref()
                .is_none_or(|id| *id == message.channel_id)
        })
        .and_then(|r| r.message_id.clone());
    if let Some(target) = target {
        let response = ui
            .interact(
                excerpt.response.rect,
                egui::Id::new(("reply_jump", message.id.as_str())),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            app.jump_to_message(&message.channel_id, target);
        }
    }
}

/// One-line, single-line excerpt of the replied-to message.
fn excerpt_text(message: &Message) -> String {
    let text = if message.content.is_empty() {
        if !message.attachments.is_empty() {
            format!("📎 {}", message.attachments[0].filename)
        } else if message.embeds.iter().any(|e| e.title.is_some()) {
            "Clique para ver o anexo".to_string()
        } else {
            String::new()
        }
    } else {
        message.content.replace('\n', " ")
    };
    let mut text = text;
    if text.chars().count() > 120 {
        text = text.chars().take(120).collect::<String>() + "…";
    }
    text
}

fn paint_content(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    message: &Message,
    channel_names: &HashMap<String, String>,
) {
    // A bare Tenor/Giphy link shows only its GIF, like Discord.
    if !message.content.is_empty() && !bare_media_link(message) {
        let runs = content_runs(&message.content, &app.user_cache, channel_names);
        paint_runs(app, ui, runs);
    }
    let invites = discord_invites(&message.content);
    for code in &invites {
        let url = format!("https://discord.gg/{code}");
        let details = invite_details(app, ui.ctx(), code);
        egui::Frame::new()
            .fill(theme::SIDEBAR)
            .corner_radius(4.0)
            .inner_margin(12)
            .show(ui, |ui| {
                ui.set_max_width(INLINE_MAX);
                ui.label(
                    RichText::new("CONVITE PARA UM SERVIDOR")
                        .small()
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(match &details {
                        Some(Ok(details)) => details.name.as_str(),
                        Some(Err(_)) => "Convite indisponível",
                        None => "Carregando convite…",
                    })
                    .family(theme::bold())
                    .color(theme::TEXT),
                );
                if let Some(Ok(details)) = &details {
                    if let Some(count) = details.members {
                        ui.label(
                            RichText::new(format!("{count} membros"))
                                .small()
                                .color(theme::MUTED),
                        );
                    }
                }
                if ui
                    .button("Abrir convite")
                    .on_hover_text("Entrar pelo Discord no navegador")
                    .clicked()
                {
                    open_url(ui, Some(&url));
                }
            });
    }
    for attachment in &message.attachments {
        if inline_image(app, ui, attachment) {
            continue;
        }
        let label = RichText::new(format!(
            "📎 {} ({})",
            attachment.filename,
            util::human_size(attachment.size)
        ))
        .small()
        .color(theme::BLURPLE)
        .underline();
        if ui
            .add(egui::Button::new(label).frame(false))
            .on_hover_text("Abrir no navegador")
            .clicked()
        {
            open_url(ui, attachment.url.as_deref());
        }
    }
    for embed in &message.embeds {
        if embed.url.as_deref().is_some_and(|url| {
            discord_invites(url)
                .iter()
                .any(|code| invites.contains(code))
        }) {
            continue;
        }
        if let Some(url) = util::embed_picture_url(embed) {
            inline_picture(app, ui, &url);
        }
        // GIF/image embeds are only their picture in Discord: Giphy's
        // carry a title that would otherwise show as a link below the GIF.
        if embed.title.is_none() && embed.description.is_none()
            || matches!(embed.kind.as_deref(), Some("gifv" | "image"))
        {
            continue;
        }
        // Discord's embed: a darker 4px-rounded card with its color bar on
        // the left edge.
        let bar_color = embed.color.map_or(theme::RAIL, |rgb| {
            Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
        });
        let card = egui::Frame::new()
            .fill(theme::SIDEBAR)
            .corner_radius(4.0)
            .inner_margin(egui::Margin {
                left: 16,
                right: 16,
                top: 8,
                bottom: 12,
            })
            .show(ui, |ui| {
                ui.set_max_width(INLINE_MAX);
                ui.spacing_mut().item_spacing.y = 6.0;
                if let Some(title) = &embed.title {
                    let title = RichText::new(title).family(theme::bold()).size(15.0);
                    match &embed.url {
                        Some(url) => {
                            if ui.link(title.color(theme::BLURPLE)).clicked() {
                                open_url(ui, Some(url));
                            }
                        }
                        None => {
                            ui.label(title.color(theme::TEXT));
                        }
                    }
                }
                if let Some(description) = &embed.description {
                    ui.label(RichText::new(description).size(13.5).color(theme::TEXT));
                }
            })
            .response
            .rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(card.min, Vec2::new(4.0, card.height())),
            egui::CornerRadius {
                nw: 4,
                sw: 4,
                ne: 0,
                se: 0,
            },
            bar_color,
        );
    }
    reaction_chips(app, ui, message);
    crate::ui::threads::message_thread_summary(app, ui, message);
}

/// Discord's reaction chips under a message: emoji and count, outlined in
/// blurple when we reacted (a click adds or removes ours), who reacted on
/// hover, and a "+" chip to add another while the message is hovered.
fn reaction_chips(app: &mut VesktopApp, ui: &mut egui::Ui, message: &Message) {
    if message.reactions.is_empty() {
        return;
    }
    const HEIGHT: f32 = 24.0;
    const EMOJI: f32 = 16.0;
    let row_hovered = ui
        .ctx()
        .read_response(egui::Id::new(("msg_row", message.id.as_str())))
        .is_some_and(|row| row.contains_pointer());
    let mut toggled = None;
    let mut picked = None;
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(4.0);
        for reaction in &message.reactions {
            let image = crate::ui::threads::emoji_image(
                reaction.emoji.id.as_deref(),
                reaction.emoji.name.as_deref(),
            );
            let text_color = if reaction.me {
                theme::MENTION_TEXT
            } else {
                theme::MUTED
            };
            let count = ui.painter().layout_no_wrap(
                reaction.count.to_string(),
                FontId::new(14.0, theme::bold()),
                text_color,
            );
            let width = 8.0 + EMOJI + 6.0 + count.size().x + 8.0;
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::click());
            let (fill, border) = if reaction.me {
                (theme::BLURPLE.gamma_multiply(0.15), theme::BLURPLE)
            } else if response.hovered() {
                (theme::SIDEBAR, theme::SELECTED)
            } else {
                (theme::SIDEBAR, theme::SIDEBAR)
            };
            ui.painter().rect(
                rect,
                8.0,
                fill,
                egui::Stroke::new(1.0, border),
                egui::StrokeKind::Inside,
            );
            let icon = egui::Rect::from_min_size(
                pos2(rect.left() + 8.0, rect.center().y - EMOJI / 2.0),
                Vec2::splat(EMOJI),
            );
            match image.and_then(|url| app.images.get(ui.ctx(), &app.handle, &url)) {
                Some(texture) => Image::from_texture(&texture).paint_at(ui, icon),
                None => {
                    ui.painter().text(
                        icon.center(),
                        Align2::CENTER_CENTER,
                        reaction.emoji.name.as_deref().unwrap_or("?"),
                        FontId::proportional(EMOJI - 2.0),
                        theme::TEXT,
                    );
                }
            }
            ui.painter().galley(
                pos2(icon.right() + 6.0, rect.center().y - count.size().y / 2.0),
                count,
                text_color,
            );
            let response = response.on_hover_ui(|ui| {
                ui.label(reactors_text(app, message, reaction));
            });
            if response.clicked() {
                toggled = Some((reaction.emoji.clone(), !reaction.me));
            }
        }
        let popup_id = egui::Id::new(("reaction_add", message.id.as_str()));
        if row_hovered || egui::Popup::is_id_open(ui.ctx(), popup_id) {
            let (rect, add) = ui.allocate_exact_size(Vec2::new(36.0, HEIGHT), Sense::click());
            let color = if add.hovered() {
                theme::TEXT
            } else {
                theme::MUTED
            };
            ui.painter().rect_filled(rect, 8.0, theme::SIDEBAR);
            draw_smiley(ui.painter(), rect.center() - Vec2::new(3.0, 0.0), color);
            ui.painter().text(
                pos2(rect.right() - 7.0, rect.top() + 7.0),
                Align2::CENTER_CENTER,
                "+",
                FontId::new(11.0, theme::bold()),
                color,
            );
            let add = add.on_hover_text("Adicionar reação");
            egui::Popup::from_toggle_button_response(&add)
                .id(popup_id)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .align(egui::RectAlign::TOP_START)
                .gap(8.0)
                .show(|ui| {
                    let id = egui::Id::new("reaction_picker");
                    let (mut query, mut tab) = ui.ctx().data_mut(|data| {
                        data.get_temp::<(String, Option<emojis::Group>)>(id)
                            .unwrap_or_default()
                    });
                    picked = emoji_picker(app, ui, &mut query, &mut tab, add.clicked());
                    ui.ctx().data_mut(|data| data.insert_temp(id, (query, tab)));
                });
        }
    });
    if let Some((emoji, add)) = toggled {
        app.toggle_reaction(&message.channel_id, &message.id, emoji, add);
    }
    // Re-adding one of ours is a no-op on both ends.
    if let Some(emoji) = picked {
        app.toggle_reaction(&message.channel_id, &message.id, emoji, true);
    }
}

/// The chip's tooltip, Discord-style: "Ana, Bia e mais 3 reagiram com
/// :fire:". The names load on first hover.
fn reactors_text(
    app: &mut VesktopApp,
    message: &Message,
    reaction: &crate::model::Reaction,
) -> String {
    let emoji = match (&reaction.emoji.id, reaction.emoji.name.as_deref()) {
        (Some(_), Some(name)) => format!(":{name}:"),
        (None, Some(name)) => emojis::get(name)
            .and_then(|emoji| emoji.shortcode())
            .map_or_else(|| name.to_string(), |code| format!(":{code}:")),
        _ => String::new(),
    };
    let Some(key) = reaction.emoji.api_key() else {
        return emoji;
    };
    let slot = (message.id.clone(), key.clone());
    let Some(users) = app.reactors.get(&slot) else {
        app.reactors.insert(slot, None);
        app.send(crate::backend::events::Command::FetchReactors {
            channel_id: message.channel_id.clone(),
            message_id: message.id.clone(),
            emoji: key,
        });
        return format!("Carregando… {emoji}");
    };
    let Some(users) = users else {
        return format!("Carregando… {emoji}");
    };
    reactors_sentence(
        users.iter().map(|user| user.display_name()).collect(),
        reaction.count,
        &emoji,
    )
}

fn reactors_sentence(names: Vec<&str>, count: u64, emoji: &str) -> String {
    let others = count.saturating_sub(names.len() as u64);
    let who = match (names.as_slice(), others) {
        ([], _) => return format!("{count} reagiram com {emoji}"),
        ([one], 0) => return format!("{one} reagiu com {emoji}"),
        (names, 0) => {
            let (last, rest) = names.split_last().unwrap_or((&"", &[]));
            format!("{} e {last}", rest.join(", "))
        }
        (names, others) => format!("{} e mais {others}", names.join(", ")),
    };
    format!("{who} reagiram com {emoji}")
}

/// Discord hides the link text when the message is nothing but a GIF/image
/// URL that produced a gifv/image embed — the picture (or player) replaces
/// it entirely.
fn bare_media_link(message: &Message) -> bool {
    let content = message.content.trim();
    if content.is_empty() || content.chars().any(char::is_whitespace) {
        return false;
    }
    message.embeds.iter().any(|embed| {
        matches!(embed.kind.as_deref(), Some("gifv" | "image"))
            && embed
                .url
                .as_deref()
                .is_some_and(|url| bare_url(content) == bare_url(url))
    })
}

/// Normalize a URL for the bare-link comparison: drop any autolink angle
/// brackets, the query string, the fragment and trailing slashes, so a
/// pasted `…view/cat?si=1` matches the embed's canonical URL.
fn bare_url(url: &str) -> &str {
    let url = url.trim().trim_start_matches('<').trim_end_matches('>');
    url.split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
}

#[cfg(test)]
mod reactors_tests {
    use super::reactors_sentence;

    #[test]
    fn names_who_reacted_like_discord() {
        assert_eq!(
            reactors_sentence(vec!["Ana"], 1, ":fire:"),
            "Ana reagiu com :fire:"
        );
        assert_eq!(
            reactors_sentence(vec!["Ana", "Bia", "Caio"], 3, ":fire:"),
            "Ana, Bia e Caio reagiram com :fire:"
        );
        assert_eq!(
            reactors_sentence(vec!["Ana", "Bia", "Caio"], 7, ":fire:"),
            "Ana, Bia, Caio e mais 4 reagiram com :fire:"
        );
    }
}

#[cfg(test)]
mod bare_link_tests {
    use super::bare_media_link;

    fn message(content: &str, embeds: serde_json::Value) -> crate::model::Message {
        serde_json::from_value(serde_json::json!({
            "id": "1",
            "author": { "id": "2", "username": "ana" },
            "content": content,
            "embeds": embeds,
        }))
        .unwrap()
    }

    #[test]
    fn hides_the_link_for_gifv_and_image_embeds() {
        let gifv = message(
            "https://tenor.com/view/cat-gif-123?si=1",
            serde_json::json!([{ "type": "gifv", "url": "https://tenor.com/view/cat-gif-123" }]),
        );
        assert!(bare_media_link(&gifv));

        let image = message(
            "https://cdn.discordapp.com/attachments/1/2/meme.gif",
            serde_json::json!([{
                "type": "image",
                "url": "https://cdn.discordapp.com/attachments/1/2/meme.gif",
            }]),
        );
        assert!(bare_media_link(&image));

        let autolink = message(
            "<https://tenor.com/view/cat-gif-123>",
            serde_json::json!([{ "type": "gifv", "url": "https://tenor.com/view/cat-gif-123" }]),
        );
        assert!(bare_media_link(&autolink));
    }

    #[test]
    fn keeps_text_around_links_and_non_media_embeds() {
        let with_text = message(
            "olha isso https://tenor.com/view/cat-gif-123",
            serde_json::json!([{ "type": "gifv", "url": "https://tenor.com/view/cat-gif-123" }]),
        );
        assert!(!bare_media_link(&with_text));

        let rich = message(
            "https://example.com",
            serde_json::json!([{ "type": "rich", "url": "https://example.com" }]),
        );
        assert!(!bare_media_link(&rich));

        let different_url = message(
            "https://tenor.com/view/cat-gif-123",
            serde_json::json!([{ "type": "gifv", "url": "https://tenor.com/view/dog-gif-456" }]),
        );
        assert!(!bare_media_link(&different_url));
    }
}

#[derive(Clone)]
struct InviteDetails {
    name: String,
    members: Option<u64>,
}

type InviteCache =
    std::sync::Arc<std::sync::Mutex<HashMap<String, Option<Result<InviteDetails, ()>>>>>;

fn invite_details(
    app: &VesktopApp,
    ctx: &egui::Context,
    code: &str,
) -> Option<Result<InviteDetails, ()>> {
    let cache: InviteCache = ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<InviteCache>(egui::Id::new("discord_invite_details"))
            .clone()
    });
    let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(details) = entries.get(code) {
        return details.clone();
    }
    entries.insert(code.to_string(), None);
    drop(entries);
    let code = code.to_string();
    let ctx = ctx.clone();
    app.handle.spawn(async move {
        let result = async {
            let response = reqwest::Client::new()
                .get(format!(
                    "https://discord.com/api/v10/invites/{code}?with_counts=true"
                ))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|_| ())?
                .error_for_status()
                .map_err(|_| ())?
                .json::<serde_json::Value>()
                .await
                .map_err(|_| ())?;
            let name = response["guild"]["name"]
                .as_str()
                .or_else(|| response["channel"]["name"].as_str())
                .ok_or(())?
                .to_string();
            Ok(InviteDetails {
                name,
                members: response["approximate_member_count"].as_u64(),
            })
        }
        .await;
        cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(code, Some(result));
        ctx.request_repaint();
    });
    None
}

/// Recognize only Discord invite hosts and canonicalize their safe invite codes.
fn discord_invites(content: &str) -> Vec<&str> {
    let mut codes = Vec::new();
    for word in content.split(|c: char| {
        c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')' | '[' | ']' | '"' | '\'')
    }) {
        let word = word
            .strip_prefix("https://")
            .or_else(|| word.strip_prefix("http://"))
            .unwrap_or(word);
        let word = word.strip_prefix("www.").unwrap_or(word);
        let code = [
            "discord.gg/",
            "discord.com/invite/",
            "discordapp.com/invite/",
        ]
        .iter()
        .find_map(|prefix| word.strip_prefix(prefix));
        let Some(code) = code else { continue };
        let code = code
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_end_matches(['.', ',', '!', ';', ':']);
        if !code.is_empty()
            && code
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            && !codes.contains(&code)
        {
            codes.push(code);
        }
    }
    codes
}

#[cfg(test)]
mod topic_runs_tests {
    /// Channel topics and forum guidelines render through the message
    /// markdown pipeline at a smaller base size.
    #[test]
    fn sized_runs_keep_their_base_size() {
        let users = std::collections::HashMap::new();
        let channels = std::collections::HashMap::new();
        let runs = super::content_runs_sized("**regras** do servidor", &users, &channels, 12.5);
        let sizes: Vec<f32> = runs
            .iter()
            .flat_map(|run| match run {
                super::Inline::Text(job, _) => job
                    .sections
                    .iter()
                    .map(|section| section.format.font_id.size)
                    .collect::<Vec<_>>(),
                super::Inline::Emoji { format, .. } => vec![format.font_id.size],
                _ => vec![],
            })
            .collect();
        assert_eq!(sizes, vec![12.5, 12.5]);
    }
}

#[cfg(test)]
mod invite_tests {
    use super::discord_invites;

    #[test]
    fn links_resolve_unicode_positions_and_allow_only_web_urls() {
        let users = std::collections::HashMap::new();
        let channels = std::collections::HashMap::new();
        let text = |text: &str| match super::content_runs(text, &users, &channels).pop() {
            Some(super::Inline::Text(job, extras)) => (job, extras),
            _ => panic!("expected text"),
        };
        let link_at = |job: &egui::text::LayoutJob, extras: &super::Extras, char_index| {
            let start = super::section_at(job, char_index)?.byte_range.start.0;
            super::link_target(extras, start).map(str::to_string)
        };
        let (job, extras) = text("Olá 😀 https://example.com/ação e http://example.org");
        assert!(job.text.starts_with(" https://"));
        assert_eq!(
            link_at(&job, &extras, 1).as_deref(),
            Some("https://example.com/ação")
        );
        assert_eq!(link_at(&job, &extras, 0), None);
        let index = job.text.find("http://").unwrap();
        assert_eq!(
            link_at(&job, &extras, job.text[..index].chars().count()).as_deref(),
            Some("http://example.org")
        );
        // Masked links point at their target; code is never a link.
        let (job, extras) = text("[site](https://example.com) `https://example.com`");
        assert_eq!(
            link_at(&job, &extras, 0).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(link_at(&job, &extras, job.text.chars().count() - 1), None);
        let unsafe_extras = super::Extras {
            links: vec![(0, "javascript:alert(1)".into())],
            spoilers: vec![],
        };
        assert_eq!(super::link_target(&unsafe_extras, 0), None);
    }

    #[test]
    fn quotes_and_code_blocks_become_blocks_and_spoilers_hide() {
        let users = std::collections::HashMap::new();
        let channels = std::collections::HashMap::new();
        let runs = super::content_runs("oi\n> citado\n```\nx\n```\n||segredo||", &users, &channels);
        let kinds: Vec<&str> = runs
            .iter()
            .map(|run| match run {
                super::Inline::Text(..) => "text",
                super::Inline::Emoji { .. } => "emoji",
                super::Inline::Quote(_) => "quote",
                super::Inline::CodeBlock(_) => "code",
            })
            .collect();
        assert_eq!(kinds, vec!["text", "quote", "code", "text"]);
        let Some(super::Inline::Text(job, extras)) = runs.last() else {
            panic!("expected the spoiler text");
        };
        assert_eq!(job.sections[0].format.color, crate::theme::SPOILER);
        assert_eq!(extras.spoilers, vec![(0, crate::theme::TEXT)]);
    }

    #[test]
    fn closed_search_restores_full_history() {
        let mut tools = super::ChatTools {
            search: "  ALICE  ".into(),
            search_open: true,
            ..Default::default()
        };
        assert_eq!(super::active_search(&tools), "alice");
        tools.search_open = false;
        assert!(super::active_search(&tools).is_empty());
    }

    #[test]
    fn recognizes_invites_and_ignores_lookalike_hosts() {
        assert_eq!(
            discord_invites(
                "Venha <https://discord.gg/abc-123> ou (https://www.discord.com/invite/other?utm=x). discord.gg/abc-123"
            ),
            vec!["abc-123", "other"]
        );
        assert_eq!(
            discord_invites("https://discordapp.com/invite/old_code!"),
            vec!["old_code"]
        );
        assert!(discord_invites("https://evil.test/discord.gg/code https://discord.gg.evil/code discord.gg/ discord.gg/code/extra").is_empty());
    }
}

/// Inline image attachment when it is an image with a URL; false otherwise.
fn inline_image(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    attachment: &crate::model::Attachment,
) -> bool {
    let Some(url) = &attachment.url else {
        return false;
    };
    let is_image = attachment
        .content_type
        .as_deref()
        .is_some_and(|kind| kind.starts_with("image/"))
        || matches!(
            attachment.filename.rsplit('.').next(),
            Some("png" | "jpg" | "jpeg" | "gif" | "webp")
        );
    if !is_image {
        return false;
    }
    inline_picture(app, ui, url);
    true
}

/// An inline image (GIFs animate); a click opens it in the viewer.
fn inline_picture(app: &mut VesktopApp, ui: &mut egui::Ui, url: &str) {
    let Some(texture) = app.images_large.get(ui.ctx(), &app.handle, url) else {
        return; // fetched: keep the line hidden while it loads
    };
    let size = texture.size_vec2();
    let scale = (INLINE_MAX / size.x).min(300.0 / size.y).min(1.0);
    let response = ui
        .add(
            Image::new((texture.id(), size * scale))
                .corner_radius(egui::CornerRadius::same(theme::RADIUS_SM as u8))
                .sense(Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        app.viewer = Some(ImageViewer::new(url.to_string()));
    }
}

fn open_url(ui: &egui::Ui, url: Option<&str>) {
    if let Some(url) = url.filter(|url| !url.is_empty()) {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
}

fn conversation_start(app: &mut VesktopApp, ui: &mut egui::Ui, channel_id: &str) {
    let is_dm = matches!(app.channel_index.get(channel_id), Some(ChannelRef::Dm));
    let name = app.channel_name(channel_id);
    let avatar_url = if is_dm {
        app.dm_channels
            .iter()
            .find(|channel| channel.id == channel_id)
            .and_then(util::dm_icon_url)
    } else {
        None
    };
    let avatar_color = if is_dm {
        util::name_color(&name)
    } else {
        theme::BLURPLE
    };
    let loaded = avatar_url
        .as_deref()
        .and_then(|url| app.images.get(ui.ctx(), &app.handle, url));

    ui.vertical_centered(|ui| {
        ui.add_space(32.0);
        let group = app
            .dm_channels
            .iter()
            .find(|channel| channel.id == channel_id)
            .is_some_and(|channel| channel.dm_partner().is_none());
        if group && loaded.is_none() {
            crate::ui::group_avatar(ui, 80.0);
        } else {
            round_avatar(
                ui,
                loaded.as_ref(),
                80.0,
                if is_dm { &name } else { "#" },
                avatar_color,
            );
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(if is_dm {
                name.clone()
            } else {
                format!("# {name}")
            })
            .size(26.0)
            .family(theme::bold())
            .color(theme::TEXT),
        );
        ui.add_space(2.0);
        ui.label(
            RichText::new(if is_dm {
                format!("Este é o começo do seu histórico de mensagens diretas com @{name}.")
            } else {
                format!("Este é o começo do canal #{name}.")
            })
            .small()
            .color(theme::MUTED),
        );
        ui.add_space(8.0);
    });
}

fn date_separator(ui: &mut egui::Ui, label: &str) {
    ui.add_space(8.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::hover());
    let painter = ui.painter();
    painter.line_segment(
        [
            pos2(rect.left(), rect.center().y),
            pos2(rect.right(), rect.center().y),
        ],
        egui::Stroke::new(1.0, theme::DIVIDER),
    );
    let galley =
        painter.layout_no_wrap(label.to_string(), FontId::proportional(12.0), theme::MUTED);
    let pad = 8.0;
    let label_rect =
        egui::Rect::from_center_size(rect.center(), galley.size() + Vec2::new(pad * 2.0, 0.0));
    painter.rect_filled(label_rect, 0.0, theme::CHAT);
    painter.galley(
        label_rect.min + Vec2::new(pad, (label_rect.height() - galley.size().y) / 2.0),
        galley,
        theme::MUTED,
    );
    ui.add_space(8.0);
}

/// Red "NOVAS" divider, Discord-style: line across, label at the left.
fn new_messages_divider(ui: &mut egui::Ui) -> egui::Rect {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
    let painter = ui.painter();
    painter.line_segment(
        [
            pos2(rect.left(), rect.center().y),
            pos2(rect.right(), rect.center().y),
        ],
        egui::Stroke::new(1.5, theme::RED),
    );
    let galley =
        painter.layout_no_wrap("NOVAS".to_string(), FontId::proportional(11.0), theme::RED);
    let pad = 6.0;
    let label_rect = egui::Rect::from_min_size(
        pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0),
        galley.size() + Vec2::new(pad * 2.0, 0.0),
    );
    painter.rect_filled(label_rect, 0.0, theme::CHAT);
    painter.galley(
        label_rect.min + Vec2::new(pad, (label_rect.height() - galley.size().y) / 2.0),
        galley,
        theme::RED,
    );
    ui.add_space(4.0);
    rect
}

/// "Ir para o presente", pinned above the compose box while scrolled up.
fn typing_label(names: &[String]) -> String {
    match names.len() {
        1 => format!("{} está digitando…", names[0]),
        2 => format!("{} e {} estão digitando…", names[0], names[1]),
        _ => format!("{}, {} e {} estão digitando…", names[0], names[1], names[2]),
    }
}

/// Per-section extras a `LayoutJob` can't carry, by section start byte:
/// link targets (bare or masked) and hidden spoilers' real colors.
#[derive(Default)]
pub(crate) struct Extras {
    links: Vec<(usize, String)>,
    spoilers: Vec<(usize, Color32)>,
}

fn section_at(job: &LayoutJob, char_index: usize) -> Option<&egui::text::LayoutSection> {
    let byte_index = job.text.char_indices().nth(char_index)?.0;
    job.sections.iter().find(|section| {
        section
            .byte_range
            .contains(&egui::text::ByteIndex(byte_index))
    })
}

/// Which revealed-spoiler key a section has: its text and position, so a
/// spoiler stays open across frames.
fn spoiler_key(job: &LayoutJob, start: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (&job.text, start).hash(&mut hasher);
    hasher.finish()
}

const REVEALED_SPOILERS: &str = "revealed_spoilers";

/// Keep egui's wrapping and text selection, with links and spoilers
/// hit-tested per glyph: a link opens on click, a spoiler reveals.
fn linked_content(ui: &mut egui::Ui, mut job: LayoutJob, extras: Extras) {
    let revealed_id = egui::Id::new(REVEALED_SPOILERS);
    let mut hidden = Vec::new();
    if !extras.spoilers.is_empty() {
        let revealed: std::collections::HashSet<u64> = ui
            .ctx()
            .data(|data| data.get_temp(revealed_id))
            .unwrap_or_default();
        for (start, color) in &extras.spoilers {
            let key = spoiler_key(&job, *start);
            let Some(section) = job
                .sections
                .iter_mut()
                .find(|section| section.byte_range.start.0 == *start)
            else {
                continue;
            };
            if revealed.contains(&key) {
                section.format.color = *color;
                section.format.background = theme::SPOILER_SHOWN;
            } else {
                hidden.push((*start, key));
            }
        }
    }
    let (pos, galley, response) = Label::new(job).sense(Sense::click()).layout_in_ui(ui);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), galley.text())
    });
    let hovered_section = if response.hovered() {
        ui.input(|input| input.pointer.hover_pos())
            .and_then(|pointer| {
                let mut char_index = 0;
                for row in &galley.rows {
                    for (index, glyph) in row.glyphs.iter().enumerate() {
                        if glyph
                            .logical_rect()
                            .translate(pos.to_vec2() + row.pos.to_vec2())
                            .contains(pointer)
                        {
                            return section_at(&galley.job, char_index + index)
                                .map(|section| section.byte_range.start.0);
                        }
                    }
                    char_index += row.char_count_including_newline().0;
                }
                None
            })
    } else {
        None
    };
    let spoiler = hovered_section
        .and_then(|start| hidden.iter().find(|(hidden, _)| *hidden == start))
        .map(|(_, key)| *key);
    let link = hovered_section
        .filter(|_| spoiler.is_none())
        .and_then(|start| link_target(&extras, start))
        .map(str::to_string);
    egui::text_selection::LabelSelectionState::label_text_selection(
        ui,
        &response,
        pos,
        galley,
        theme::TEXT,
        egui::Stroke::NONE,
    );
    if let Some(key) = spoiler {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if response.clicked() {
            ui.ctx().data_mut(|data| {
                data.get_temp_mut_or_default::<std::collections::HashSet<u64>>(revealed_id)
                    .insert(key);
            });
        }
    } else if let Some(url) = link {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if response.clicked() {
            open_url(ui, Some(&url));
        }
        response.on_hover_text(url);
    } else if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
}

/// A section's link target, only for web URLs with a host.
fn link_target(extras: &Extras, start: usize) -> Option<&str> {
    let (_, url) = extras.links.iter().find(|(at, _)| *at == start)?;
    let parsed = reqwest::Url::parse(url).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some())
        .then_some(url.as_str())
}

/// Message text between its emoji: styled text, or an emoji image with the
/// text it falls back to; quotes and code blocks are blocks of their own.
pub(crate) enum Inline {
    Text(LayoutJob, Extras),
    Emoji {
        url: String,
        alt: String,
        format: TextFormat,
    },
    Quote(Vec<Inline>),
    CodeBlock(LayoutJob),
}

/// Message text: one selectable label when it has no emoji; otherwise its
/// runs wrap between emoji images, selectable run by run. A message of only
/// emoji (up to 27) shows them jumbo, like Discord.
pub(crate) fn paint_runs(app: &mut VesktopApp, ui: &mut egui::Ui, runs: Vec<Inline>) {
    paint_runs_with(app, ui, runs, true);
}

/// `allow_jumbo: false` keeps emojis at text size — channel topics and
/// the forum guidelines never blow up a lone emoji.
pub(crate) fn paint_runs_with(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    runs: Vec<Inline>,
    allow_jumbo: bool,
) {
    let emojis = runs
        .iter()
        .filter(|run| matches!(run, Inline::Emoji { .. }))
        .count();
    let jumbo = allow_jumbo
        && (1..=27).contains(&emojis)
        && runs.iter().all(|run| match run {
            Inline::Text(job, _) => job.text.trim().is_empty(),
            Inline::Emoji { .. } => true,
            Inline::Quote(_) | Inline::CodeBlock(_) => false,
        });
    let mut paragraph = Vec::new();
    for run in runs {
        match run {
            Inline::Quote(inner) => {
                paint_paragraph(app, ui, std::mem::take(&mut paragraph), jumbo);
                paint_quote(app, ui, inner);
            }
            Inline::CodeBlock(job) => {
                paint_paragraph(app, ui, std::mem::take(&mut paragraph), jumbo);
                egui::Frame::new()
                    .fill(theme::RAIL)
                    .stroke(egui::Stroke::new(1.0, theme::SPOILER))
                    .corner_radius(4.0)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width());
                        ui.add(Label::new(job).wrap());
                    });
            }
            run => paragraph.push(run),
        }
    }
    paint_paragraph(app, ui, paragraph, jumbo);
}

/// Discord's block quote: a rounded 4px bar beside the indented text.
fn paint_quote(app: &mut VesktopApp, ui: &mut egui::Ui, runs: Vec<Inline>) {
    let rect = ui
        .horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(16.0);
            ui.vertical(|ui| paint_runs_with(app, ui, runs, false));
        })
        .response
        .rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height())),
        2.0,
        theme::QUOTE_BAR,
    );
}

fn paint_paragraph(app: &mut VesktopApp, ui: &mut egui::Ui, runs: Vec<Inline>, jumbo: bool) {
    if runs.is_empty() {
        return;
    }
    if !runs.iter().any(|run| matches!(run, Inline::Emoji { .. })) {
        for run in runs {
            if let Inline::Text(job, extras) = run {
                linked_content(ui, job, extras);
            }
        }
        return;
    }
    let size = if jumbo {
        48.0
    } else {
        // Discord's inline emojis run ~1.5× the text size.
        runs.iter()
            .map(|run| match run {
                Inline::Emoji { format, .. } => format.font_id.size * 1.5,
                _ => 0.0,
            })
            .fold(0.0_f32, f32::max)
            .round()
            .max(16.0)
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for run in runs {
            match run {
                Inline::Text(job, extras) => linked_content(ui, job, extras),
                Inline::Emoji { url, alt, format } => {
                    let name = emojis::get(&alt)
                        .and_then(emojis::Emoji::shortcode)
                        .map_or_else(|| alt.clone(), |code| format!(":{code}:"));
                    crate::ui::emoji::image(app, ui, &url, &alt, &format, size).on_hover_text(name);
                }
                Inline::Quote(_) | Inline::CodeBlock(_) => {}
            }
        }
    });
}

/// Maps markup segments to styled runs and emoji, resolving mention and
/// channel ids against the state the caller already gathered.
pub(crate) fn content_runs(
    text: &str,
    users: &HashMap<String, User>,
    channels: &HashMap<String, String>,
) -> Vec<Inline> {
    content_runs_sized(text, users, channels, 14.5)
}

/// Runs being built: the finished ones plus the text job still growing.
#[derive(Default)]
struct RunBuilder {
    runs: Vec<Inline>,
    job: LayoutJob,
    extras: Extras,
}

impl RunBuilder {
    fn text(
        &mut self,
        text: &str,
        format: TextFormat,
        link: Option<&str>,
        spoiler: Option<Color32>,
    ) {
        let start = self.job.text.len();
        if let Some(url) = link {
            self.extras.links.push((start, url.to_string()));
        }
        if let Some(color) = spoiler {
            self.extras.spoilers.push((start, color));
        }
        self.job.append(text, 0.0, format);
    }

    fn push(&mut self, run: Inline) {
        self.flush();
        self.runs.push(run);
    }

    fn flush(&mut self) {
        let job = std::mem::take(&mut self.job);
        let extras = std::mem::take(&mut self.extras);
        if !job.text.is_empty() {
            self.runs.push(Inline::Text(job, extras));
        }
    }

    fn finish(mut self) -> Vec<Inline> {
        self.flush();
        self.runs
    }
}

/// The same pipeline at a different base size — channel topics and the
/// forum guidelines use it at reading sizes smaller than a message.
pub(crate) fn content_runs_sized(
    text: &str,
    users: &HashMap<String, User>,
    channels: &HashMap<String, String>,
    size: f32,
) -> Vec<Inline> {
    let mut outer = RunBuilder::default();
    let mut quote = RunBuilder::default();
    for segment in markup::tokenize(text) {
        if !segment.quote && !(quote.runs.is_empty() && quote.job.text.is_empty()) {
            outer.push(Inline::Quote(std::mem::take(&mut quote).finish()));
        }
        let target = if segment.quote {
            &mut quote
        } else {
            &mut outer
        };
        if segment.style == Style::CodeBlock {
            let mut job = LayoutJob::default();
            job.append(
                &segment.text,
                0.0,
                TextFormat::simple(FontId::monospace(size - 1.5), theme::TEXT),
            );
            target.push(Inline::CodeBlock(job));
            continue;
        }
        let base = match segment.size {
            markup::Size::Normal => size,
            markup::Size::H1 => size * 1.65,
            markup::Size::H2 => size * 1.38,
            markup::Size::H3 => size * 1.1,
            markup::Size::Small => size * 0.83,
        };
        let marks = segment.marks;
        let bold =
            marks.bold || !matches!(segment.size, markup::Size::Normal | markup::Size::Small);
        let mut format = TextFormat {
            font_id: if bold {
                FontId::new(base, theme::bold())
            } else {
                FontId::proportional(base)
            },
            color: match segment.size {
                markup::Size::Small => theme::MUTED,
                _ if bold => theme::WHITE,
                _ => theme::TEXT,
            },
            italics: marks.italic,
            ..Default::default()
        };
        let resolved = match segment.style {
            Style::Normal | Style::CodeBlock => None,
            Style::Code => {
                format.font_id = FontId::monospace(base - 1.5);
                format.background = theme::INPUT;
                None
            }
            Style::Mention => {
                format.color = theme::MENTION_TEXT;
                format.background = theme::MENTION_BG;
                users
                    .get(segment.text.trim_start_matches('@'))
                    .map(|user| format!("@{}", user.display_name()))
            }
            Style::ChannelName => {
                format.color = theme::MENTION_TEXT;
                format.background = theme::MENTION_BG;
                channels
                    .get(segment.text.trim_start_matches('#'))
                    .map(|name| format!("#{name}"))
            }
            Style::Emoji => {
                format.color = theme::MUTED;
                None
            }
            Style::Link => {
                format.color = theme::BLURPLE;
                None
            }
        };
        if marks.underline || segment.style == Style::Link {
            format.underline = egui::Stroke::new(1.0, format.color);
        }
        if marks.strike {
            format.strikethrough = egui::Stroke::new(1.0, format.color);
        }
        let link = (segment.style == Style::Link)
            .then(|| segment.url.clone().unwrap_or_else(|| segment.text.clone()));
        let text = resolved.unwrap_or(segment.text);
        if marks.spoiler {
            // Hidden: the text in its own background until clicked. Emoji
            // stay text so no image gives the spoiler away.
            let real = format.color;
            format.color = theme::SPOILER;
            format.background = theme::SPOILER;
            format.underline.color = theme::SPOILER;
            format.strikethrough.color = theme::SPOILER;
            let text = match crate::ui::emoji::custom(&text) {
                Some((alt, _)) if segment.style == Style::Emoji => alt,
                _ => text,
            };
            target.text(&text, format, link.as_deref(), Some(real));
            continue;
        }
        let custom = (segment.style == Style::Emoji)
            .then(|| crate::ui::emoji::custom(&text))
            .flatten();
        if let Some((alt, url)) = custom {
            target.push(Inline::Emoji { url, alt, format });
            continue;
        }
        if matches!(segment.style, Style::Code | Style::Link) {
            target.text(&text, format, link.as_deref(), None);
            continue;
        }
        for piece in crate::ui::emoji::split(&text) {
            match piece {
                crate::ui::emoji::Piece::Text(text) => {
                    target.text(text, format.clone(), None, None)
                }
                crate::ui::emoji::Piece::Emoji(emoji) => target.push(Inline::Emoji {
                    url: crate::ui::emoji::twemoji_url(emoji),
                    alt: emoji.as_str().to_string(),
                    format: format.clone(),
                }),
            }
        }
    }
    if !(quote.runs.is_empty() && quote.job.text.is_empty()) {
        outer.push(Inline::Quote(quote.finish()));
    }
    outer.finish()
}

/// The compose box pickers' popover: above its button, toggled by it,
/// closed by a click outside or Escape. Fixed ids, since the compose
/// box's auto ids shift when the typing line shows up.
fn picker_popup<'a>(button: &egui::Response, id: &str) -> egui::Popup<'a> {
    egui::Popup::from_toggle_button_response(button)
        .id(egui::Id::new(id))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::TOP_END)
        .gap(8.0)
}

/// GIF picker: Discord's GIF endpoints (trending while the field is empty)
/// in a masonry grid; a click sends the GIF's link, as the official
/// client does.
fn gif_picker(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    tools: &mut ChatTools,
    channel_id: &str,
    just_opened: bool,
) {
    ui.set_width(400.0);
    let search = ui.add(
        TextEdit::singleline(&mut tools.gif_query)
            .hint_text("Pesquisar no Tenor")
            .desired_width(f32::INFINITY),
    );
    if just_opened {
        search.request_focus();
    }
    let now = ui.input(|input| input.time);
    if search.changed() {
        tools.gif_edited = now;
    }
    // Search once typing pauses.
    let query = tools.gif_query.trim().to_string();
    if tools.gif_requested.as_ref() != Some(&query) {
        let wait = 0.35 - (now - tools.gif_edited);
        if wait <= 0.0 {
            app.send(crate::backend::events::Command::SearchGifs {
                query: query.clone(),
            });
            tools.gif_requested = Some(query);
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
    }
    ui.add_space(8.0);
    // The latest results stay up while a newer search is in flight.
    let gifs = match &app.gifs {
        None => {
            ui.label(RichText::new("Carregando GIFs…").color(theme::MUTED));
            return;
        }
        Some((_, Err(error))) => {
            ui.label(
                RichText::new(format!("Não consegui carregar os GIFs: {error}"))
                    .small()
                    .color(theme::RED),
            );
            if ui.button("Tentar de novo").clicked() {
                app.gifs = None;
                tools.gif_requested = None;
            }
            return;
        }
        Some((_, Ok(gifs))) if gifs.is_empty() => {
            ui.label(RichText::new("Nenhum GIF encontrado.").color(theme::MUTED));
            return;
        }
        Some((_, Ok(gifs))) => gifs,
    };
    let mut send = None;
    ScrollArea::vertical()
        .min_scrolled_height(360.0)
        .max_height(360.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let width = ui.available_width();
            let (rects, height) = masonry(gifs.iter().map(|gif| (gif.width, gif.height)), width);
            let (area, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
            for (index, (gif, rect)) in gifs.iter().zip(rects).enumerate() {
                let rect = rect.translate(area.min.to_vec2());
                // Only what's on screen gets fetched.
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let response = ui.interact(rect, ui.id().with(("gif", index)), Sense::click());
                // ponytail: the image cache never evicts, so every search
                // keeps its thumbnails; add eviction if memory shows it.
                match app.images.get(ui.ctx(), &app.handle, &gif.src) {
                    Some(texture) => Image::from_texture(&texture)
                        .corner_radius(4)
                        .paint_at(ui, rect),
                    None => {
                        ui.painter().rect_filled(rect, 4.0, theme::INPUT);
                    }
                }
                if response.hovered() {
                    ui.painter().rect_stroke(
                        rect,
                        4.0,
                        egui::Stroke::new(2.0, theme::BLURPLE),
                        egui::StrokeKind::Inside,
                    );
                }
                if response.clicked() {
                    send = Some(gif.url.clone());
                }
            }
        });
    if let Some(url) = send {
        app.send(crate::backend::events::Command::SendMessage {
            channel_id: channel_id.to_string(),
            content: url,
        });
        ui.close();
    }
}

/// Three fixed-width columns, each GIF under the shortest one, like the
/// official picker. Returns the rects (from the grid's top-left) and the
/// grid height.
fn masonry(sizes: impl Iterator<Item = (f32, f32)>, width: f32) -> (Vec<egui::Rect>, f32) {
    const COLUMNS: usize = 3;
    const GAP: f32 = 6.0;
    let column = (width - GAP * (COLUMNS - 1) as f32) / COLUMNS as f32;
    let mut heights = [0.0f32; COLUMNS];
    let rects = sizes
        .map(|(w, h)| {
            let height = if w > 0.0 && h > 0.0 {
                (column * h / w).clamp(column / 2.0, column * 2.0)
            } else {
                column
            };
            let (index, top) = heights
                .iter()
                .copied()
                .enumerate()
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or_default();
            heights[index] = top + height + GAP;
            egui::Rect::from_min_size(
                pos2(index as f32 * (column + GAP), top),
                Vec2::new(column, height),
            )
        })
        .collect();
    let tallest = heights.into_iter().fold(0.0, f32::max);
    (rects, (tallest - GAP).max(0.0))
}

/// Emoji picker tabs: the servers' custom emojis, then the Unicode groups.
const EMOJI_TABS: [(Option<emojis::Group>, &str, &str); 10] = [
    (None, "⭐", "Emojis dos servidores"),
    (
        Some(emojis::Group::SmileysAndEmotion),
        "😀",
        "Carinhas e emoções",
    ),
    (Some(emojis::Group::PeopleAndBody), "👋", "Pessoas e corpo"),
    (
        Some(emojis::Group::AnimalsAndNature),
        "🐶",
        "Animais e natureza",
    ),
    (Some(emojis::Group::FoodAndDrink), "🍔", "Comidas e bebidas"),
    (
        Some(emojis::Group::TravelAndPlaces),
        "🚗",
        "Viagens e lugares",
    ),
    (Some(emojis::Group::Activities), "⚽", "Atividades"),
    (Some(emojis::Group::Objects), "💡", "Objetos"),
    (Some(emojis::Group::Symbols), "❤", "Símbolos"),
    (Some(emojis::Group::Flags), "🏁", "Bandeiras"),
];

#[derive(Clone, Copy)]
enum PickerEmoji<'a> {
    Unicode(&'static emojis::Emoji),
    Custom(&'a GuildEmoji),
}

/// What goes into the compose box for an emoji.
fn emoji_markup(emoji: &ReactionEmoji) -> String {
    let name = emoji.name.clone().unwrap_or_default();
    match &emoji.id {
        None => name,
        Some(id) => format!("<{}:{name}:{id}>", if emoji.animated { "a" } else { "" }),
    }
}

impl PickerEmoji<'_> {
    fn emoji(self) -> ReactionEmoji {
        match self {
            Self::Unicode(emoji) => ReactionEmoji {
                id: None,
                name: Some(emoji.as_str().to_string()),
                animated: false,
            },
            Self::Custom(emoji) => ReactionEmoji {
                id: Some(emoji.id.clone()),
                name: Some(emoji.name.clone()),
                animated: emoji.animated,
            },
        }
    }

    fn markup(self) -> String {
        emoji_markup(&self.emoji())
    }

    fn name(self) -> String {
        match self {
            Self::Unicode(emoji) => format!(":{}:", emoji.shortcode().unwrap_or(emoji.name())),
            Self::Custom(emoji) => format!(":{}:", emoji.name),
        }
    }
}

/// The emojis the picker lists: for a query, matches across everything
/// (custom names, Unicode names and shortcodes), custom first; otherwise
/// the tab's.
fn picker_emojis<'a>(
    query: &str,
    tab: Option<emojis::Group>,
    custom: &[&'a GuildEmoji],
) -> Vec<PickerEmoji<'a>> {
    let query = query.trim().trim_matches(':').to_lowercase();
    if query.is_empty() {
        return match tab {
            None => custom
                .iter()
                .map(|emoji| PickerEmoji::Custom(emoji))
                .collect(),
            Some(group) => group.emojis().map(PickerEmoji::Unicode).collect(),
        };
    }
    let custom = custom
        .iter()
        .filter(|emoji| emoji.name.to_lowercase().contains(&query))
        .map(|emoji| PickerEmoji::Custom(emoji));
    let unicode = emojis::iter()
        .filter(|emoji| {
            emoji.name().contains(&query) || emoji.shortcodes().any(|code| code.contains(&query))
        })
        .map(PickerEmoji::Unicode);
    custom.chain(unicode).collect()
}

/// Emoji picker: search, category tabs, Unicode emojis and the servers'
/// custom ones (the current server first). A click returns the pick;
/// Shift+click keeps the picker open, as in Discord.
pub(crate) fn emoji_picker(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    query: &mut String,
    tab_state: &mut Option<emojis::Group>,
    just_opened: bool,
) -> Option<ReactionEmoji> {
    const CELL: f32 = 36.0;
    const COLUMNS: usize = 9;
    ui.set_width(CELL * COLUMNS as f32);
    let search = ui.add(
        TextEdit::singleline(query)
            .hint_text("Encontre o emoji perfeito")
            .desired_width(f32::INFINITY),
    );
    if just_opened {
        search.request_focus();
    }
    let mut guild_ids: Vec<&str> = app.guilds.iter().map(|guild| guild.id.as_str()).collect();
    guild_ids.sort_by_key(|id| Some(*id) != app.selected_guild.as_deref());
    let custom: Vec<&GuildEmoji> = guild_ids
        .iter()
        .filter_map(|id| app.guild_emojis.get(*id))
        .flatten()
        .filter(|emoji| emoji.available != Some(false))
        .collect();
    let tab = match *tab_state {
        None if custom.is_empty() => Some(emojis::Group::SmileysAndEmotion),
        tab => tab,
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (group, icon, label) in EMOJI_TABS {
            if group.is_none() && custom.is_empty() {
                continue;
            }
            let selected = query.trim().is_empty() && group == tab;
            if ui
                .selectable_label(selected, RichText::new(icon).size(18.0))
                .on_hover_text(label)
                .clicked()
            {
                *tab_state = group;
                query.clear();
            }
        }
    });
    ui.add_space(4.0);
    let list = picker_emojis(query, tab, &custom);
    if list.is_empty() {
        ui.label(RichText::new("Nenhum emoji encontrado.").color(theme::MUTED));
        return None;
    }
    let mut picked = None;
    ScrollArea::vertical()
        .min_scrolled_height(300.0)
        .max_height(300.0)
        .auto_shrink([false, true])
        .show_rows(ui, CELL, list.len().div_ceil(COLUMNS), |ui, rows| {
            for row in rows {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    for emoji in list.iter().skip(row * COLUMNS).take(COLUMNS) {
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::splat(CELL), Sense::click());
                        if response.hovered() {
                            ui.painter().rect_filled(rect, 4.0, theme::HOVER);
                        }
                        let url = match emoji {
                            PickerEmoji::Unicode(emoji) => crate::ui::emoji::twemoji_url(emoji),
                            PickerEmoji::Custom(custom) => {
                                crate::ui::emoji::custom_url(&custom.id, custom.animated)
                            }
                        };
                        match app.images.get(ui.ctx(), &app.handle, &url) {
                            Some(texture) => {
                                Image::from_texture(&texture).paint_at(ui, rect.shrink(6.0));
                            }
                            None if matches!(emoji, PickerEmoji::Unicode(_)) => {
                                ui.painter().text(
                                    rect.center(),
                                    Align2::CENTER_CENTER,
                                    emoji.markup(),
                                    FontId::proportional(24.0),
                                    theme::TEXT,
                                );
                            }
                            None => {}
                        }
                        if response.on_hover_text(emoji.name()).clicked() {
                            picked = Some(*emoji);
                        }
                    }
                });
            }
        });
    let picked = picked.map(PickerEmoji::emoji);
    if picked.is_some() && !ui.input(|input| input.modifiers.shift) {
        ui.close();
    }
    picked
}

/// Smiley face for the compose box emoji button.
fn draw_smiley(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    painter.circle_stroke(center, 7.0, stroke);
    painter.circle_filled(pos2(center.x - 2.5, center.y - 2.0), 1.1, color);
    painter.circle_filled(pos2(center.x + 2.5, center.y - 2.0), 1.1, color);
    let mouth: Vec<egui::Pos2> = (0..=6)
        .map(|i| {
            let t = i as f32 / 6.0;
            let angle = std::f32::consts::PI * (0.15 + 0.7 * t);
            pos2(
                center.x + 4.0 * angle.cos(),
                center.y + 4.0 * angle.sin() - 0.5,
            )
        })
        .collect();
    painter.add(egui::Shape::line(mouth, stroke));
}

#[cfg(test)]
mod picker_tests {
    use super::*;

    #[test]
    fn emoji_search_covers_custom_and_unicode() {
        let party = GuildEmoji {
            id: "1".into(),
            name: "PartyBlob".into(),
            animated: true,
            available: None,
        };
        let custom = [&party];
        let found = picker_emojis(":party", None, &custom);
        let markup: Vec<String> = found.iter().map(|emoji| emoji.markup()).collect();
        assert_eq!(markup[0], "<a:PartyBlob:1>");
        assert!(markup.contains(&"🎉".to_string()));
        // Twemoji draws every sequence, flags included.
        assert!(
            picker_emojis("", Some(emojis::Group::Flags), &custom)
                .iter()
                .any(|emoji| emoji.markup() == "🇧🇷")
        );
        // No query: the tab's list, custom ones only on their own tab.
        assert_eq!(picker_emojis("  ", None, &custom).len(), 1);
        assert_eq!(
            picker_emojis("", Some(emojis::Group::SmileysAndEmotion), &custom)[0].markup(),
            "😀"
        );
    }

    #[test]
    fn gif_results_parse_and_lay_out_in_columns() {
        let gifs: Vec<crate::model::Gif> = serde_json::from_str(
            r#"[{"id":"1","title":"","url":"https://tenor.com/view/a-1","src":"https://media.tenor.com/xAAAAM/a.gif","gif_src":"https://media.tenor.com/xAAAAC/a.gif","width":200,"height":100,"preview":"p"},
                {"id":"2","url":"https://tenor.com/view/b-2","src":"https://media.tenor.com/yAAAAM/b.gif"}]"#,
        )
        .unwrap();
        assert_eq!(gifs[0].url, "https://tenor.com/view/a-1");
        assert_eq!(gifs[1].width, 0.0);
        let sizes = [
            (200.0, 100.0),
            (100.0, 100.0),
            (100.0, 100.0),
            (100.0, 100.0),
        ];
        let (rects, height) = masonry(sizes.into_iter(), 312.0);
        // 100 px columns; the fourth goes under the shortest (the first).
        assert_eq!(rects[0].height(), 50.0);
        assert_eq!(rects[3].min, pos2(0.0, 56.0));
        assert_eq!(height, 156.0);
        assert_eq!(masonry(std::iter::empty(), 312.0).1, 0.0);
    }
}
