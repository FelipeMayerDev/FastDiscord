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
use crate::model::{Message, User};
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

#[derive(Clone, Default)]
struct ChatTools {
    search_open: bool,
    search: String,
    emoji_open: bool,
    gif_open: bool,
    gif_url: String,
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
                        ui.painter().image(
                            texture.id(),
                            avatar_rect,
                            egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
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
                        theme::CHAT,
                    );
                }
                ui.add_space(8.0);
            }
            ui.label(
                RichText::new(&name)
                    .family(theme::bold())
                    .size(16.0)
                    .color(theme::TEXT),
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
            ui.label(
                RichText::new(format!("{prefix} {name}"))
                    .family(theme::bold())
                    .size(16.0)
                    .color(theme::TEXT),
            );
            if let Some(topic) = &topic {
                ui.add_space(10.0);
                ui.label(RichText::new(topic).small().color(theme::MUTED));
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
        });
    });
    ui.separator();

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
                                    draw_smiley(ui.painter(), smile_rect.center(), theme::TEXT);
                                    if smile.on_hover_text("Inserir emoji").clicked() {
                                        tools.emoji_open = true;
                                    }
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
                                    if gif.on_hover_text("Enviar GIF por link").clicked() {
                                        tools.gif_open = true;
                                    }
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
    egui::Window::new("Emoji")
        .collapsible(false)
        .resizable(false)
        .open(&mut tools.emoji_open)
        .show(&ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for emoji in [
                    "😀", "😂", "❤️", "👍", "🎉", "🔥", "😢", "👀", "✅", "🙏", "🚀", "💀",
                ] {
                    if ui.button(emoji).clicked() {
                        app.compose.push_str(emoji);
                    }
                }
            });
        });
    let mut send_gif = false;
    egui::Window::new("Enviar GIF")
        .collapsible(false)
        .open(&mut tools.gif_open)
        .show(&ctx, |ui| {
            ui.label("Cole um link de GIF, Tenor ou Giphy.");
            ui.text_edit_singleline(&mut tools.gif_url);
            if ui
                .add_enabled(
                    tools.gif_url.trim().starts_with("https://"),
                    egui::Button::new("Enviar"),
                )
                .clicked()
            {
                send_gif = true;
            }
        });
    if send_gif {
        app.send(crate::backend::events::Command::SendMessage {
            channel_id: channel_id.clone(),
            content: tools.gif_url.trim().to_string(),
        });
        tools.gif_url.clear();
        tools.gif_open = false;
    }
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
                    new_messages_divider(ui);
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
    if at_bottom && active_search(&tools).is_empty() {
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
        egui::Area::new(egui::Id::new("jump_to_present"))
            .anchor(Align2::RIGHT_BOTTOM, [-24.0, -96.0])
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                if ui
                    .button(RichText::new("Ir para o presente").color(theme::TEXT))
                    .on_hover_text("Pular para as mensagens mais recentes")
                    .clicked()
                {
                    app.jump_to_present = true;
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
                    theme::CHAT,
                );
                ui.add_space(GAP);
                ui.vertical(|ui| {
                    // Author line: name, then the time right after it in
                    // small muted text, Discord-style.
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
    let hover = fade(
        ui,
        response.id,
        response.hovered(),
        Color32::TRANSPARENT,
        theme::ROW_HOVER,
    );
    let base = if mentions_me {
        theme::MENTIONED_BG
    } else {
        Color32::TRANSPARENT
    };
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
    ui.horizontal(|ui| {
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
    let bare_gif = message.embeds.iter().any(|embed| {
        embed.kind.as_deref() == Some("gifv")
            && embed.url.as_deref() == Some(message.content.trim())
    });
    if !message.content.is_empty() && !bare_gif {
        let job = content_job(&message.content, &app.user_cache, channel_names);
        linked_content(ui, job);
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
        if embed.title.is_none() && embed.description.is_none() {
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
mod invite_tests {
    use super::discord_invites;

    #[test]
    fn links_resolve_unicode_positions_and_allow_only_web_urls() {
        let users = std::collections::HashMap::new();
        let channels = std::collections::HashMap::new();
        let job = super::content_job(
            "Olá 😀 https://example.com/ação e http://example.org",
            &users,
            &channels,
        );
        assert_eq!(
            super::link_at_char(&job, 6),
            Some("https://example.com/ação")
        );
        assert_eq!(super::link_at_char(&job, 5), None);
        let index = job.text.find("http://").unwrap();
        assert_eq!(
            super::link_at_char(&job, job.text[..index].chars().count()),
            Some("http://example.org")
        );
        let code = super::content_job(
            "`https://example.com` javascript:alert(1) file:///tmp/test",
            &users,
            &channels,
        );
        assert_eq!(super::link_at_char(&code, 0), None);
        let mut unsafe_job = job.clone();
        unsafe_job.text = "javascript:alert(1)".into();
        unsafe_job.sections = vec![
            job.sections
                .iter()
                .find(|s| s.format.underline.width > 0.0)
                .unwrap()
                .clone(),
        ];
        unsafe_job.sections[0].byte_range =
            egui::text::ByteIndex(0)..egui::text::ByteIndex(unsafe_job.text.len());
        assert_eq!(super::link_at_char(&unsafe_job, 0), None);
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
fn new_messages_divider(ui: &mut egui::Ui) {
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
}

/// "Ir para o presente", pinned above the compose box while scrolled up.
fn typing_label(names: &[String]) -> String {
    match names.len() {
        1 => format!("{} está digitando…", names[0]),
        2 => format!("{} e {} estão digitando…", names[0], names[1]),
        _ => format!("{}, {} e {} estão digitando…", names[0], names[1], names[2]),
    }
}

/// Keep egui's wrapping and text selection, with links hit-tested per glyph.
fn linked_content(ui: &mut egui::Ui, job: LayoutJob) {
    let (pos, galley, response) = Label::new(job).sense(Sense::click()).layout_in_ui(ui);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), galley.text())
    });
    let hovered_link = if response.hovered() {
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
                            return link_at_char(&galley.job, char_index + index)
                                .map(str::to_string);
                        }
                    }
                    char_index += row.char_count_including_newline().0;
                }
                None
            })
    } else {
        None
    };
    egui::text_selection::LabelSelectionState::label_text_selection(
        ui,
        &response,
        pos,
        galley,
        theme::TEXT,
        egui::Stroke::NONE,
    );
    if let Some(url) = hovered_link {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if response.clicked() {
            open_url(ui, Some(&url));
        }
        response.on_hover_text(url);
    } else if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
}

fn link_at_char(job: &LayoutJob, char_index: usize) -> Option<&str> {
    let byte_index = job.text.char_indices().nth(char_index)?.0;
    let section = job.sections.iter().find(|section| {
        section
            .byte_range
            .contains(&egui::text::ByteIndex(byte_index))
    })?;
    if section.format.color != theme::BLURPLE || section.format.underline.width == 0.0 {
        return None;
    }
    let text = &job.text[section.byte_range.start.0..section.byte_range.end.0];
    let url = reqwest::Url::parse(text).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then_some(text)
}

/// Maps markup segments to a styled `LayoutJob`, resolving mention and
/// channel ids against the state the caller already gathered.
fn content_job(
    text: &str,
    users: &HashMap<String, User>,
    channels: &HashMap<String, String>,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    for segment in markup::tokenize(text) {
        let mut format = TextFormat {
            font_id: FontId::proportional(14.5),
            color: theme::TEXT,
            ..Default::default()
        };
        let resolved = match segment.style {
            Style::Normal => None,
            Style::Bold => {
                format.color = theme::WHITE;
                None
            }
            Style::Italic => {
                format.italics = true;
                None
            }
            Style::Code | Style::CodeBlock => {
                format.font_id = FontId::monospace(13.0);
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
                format.underline = egui::Stroke::new(1.0, theme::BLURPLE);
                None
            }
        };
        let text = resolved.unwrap_or(segment.text);
        job.append(&text, 0.0, format);
    }
    job
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
