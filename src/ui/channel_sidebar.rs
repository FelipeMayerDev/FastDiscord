//! Channel sidebar: guild channels grouped by category, or the DM list when
//! the rail's home button is selected.

use egui::{RichText, Sense, Vec2, pos2};

use crate::app::{ConnState, VesktopApp, VoiceConn};
use crate::model::{CHANNEL_KIND_CATEGORY, Channel, VoiceState};
use crate::settings::CustomStatus;
use crate::theme;
use crate::ui::{draw_texture_round, fade, initial_circle};
use crate::util;

/// Height of the user panel row, pinned so the bottom block never clips.
const USER_PANEL_H: f32 = 52.0;
const VOICE_STATUS_H: f32 = 40.0;
const VOICE_ACTIONS_H: f32 = 34.0;

pub fn paint(app: &mut VesktopApp, ui: &mut egui::Ui) {
    ui.painter().rect_filled(ui.max_rect(), 0.0, theme::RAIL);

    // The bottom block (voice status + actions + user panel) is laid out
    // manually instead of through `Panel::bottom`: egui panels reuse their
    // persisted size across runs, which clipped the growing content. Both
    // regions get explicit rects — an `allocate_rect` here would push the
    // layout cursor past the panel's bottom and empty everything above.
    let in_voice = matches!(
        app.voice,
        VoiceConn::Connecting { .. } | VoiceConn::Connected { .. }
    );
    let voice_failed = matches!(app.voice, VoiceConn::Failed { .. });
    let voice_h = if in_voice {
        VOICE_STATUS_H + 2.0 + VOICE_ACTIONS_H + 4.0
    } else if voice_failed {
        VOICE_STATUS_H + 4.0
    } else {
        0.0
    };
    let available = ui.available_rect_before_wrap();
    let preview_h = crate::ui::screen_share::height(app, available.width());
    let dj_h = crate::ui::voice_extras::music_height(app);
    let total = preview_h + dj_h + voice_h + USER_PANEL_H;
    let block = egui::Rect::from_min_max(
        pos2(available.left(), available.bottom() - total),
        pos2(available.right(), available.bottom()),
    );
    let content_rect =
        egui::Rect::from_min_max(available.min, pos2(available.right(), block.top()));

    // Channel list: title plus scroll area, above the bottom block.
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    if app.selected_guild.is_none() {
        dm_sidebar_top(app, &mut content);
    } else {
        guild_sidebar_top(app, &mut content);
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(&mut content, |ui| {
            if app.selected_guild.is_none() {
                paint_dm_list(app, ui);
            } else {
                paint_guild_channels(app, ui);
            }
        });

    // Bottom block: screen share preview, voice status row, action row,
    // user panel.
    ui.painter().rect_filled(block, 0.0, theme::RAIL);
    let mut bottom = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(block)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    // Explicit add_space calls do the gaps; the default 8px item spacing
    // would overflow the reserved height and get clipped.
    bottom.spacing_mut().item_spacing = Vec2::ZERO;
    crate::ui::screen_share::paint(app, &mut bottom);
    crate::ui::voice_extras::music_panel(app, &mut bottom);
    voice_panel(app, &mut bottom);
    user_panel(app, &mut bottom);
}

/// Sidebar top on the Home view, Discord-style: the conversation search
/// box, the Amigos / Solicitações rows and the "Mensagens diretas" header.
fn dm_sidebar_top(app: &mut VesktopApp, ui: &mut egui::Ui) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        let width = ui.available_width() - 10.0;
        egui::Frame::default()
            .fill(theme::RAIL)
            .stroke(egui::Stroke::new(1.0, theme::DIVIDER))
            .corner_radius(theme::RADIUS_SM)
            .inner_margin(egui::Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_width(width - 16.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let (icon_slot, _) =
                        ui.allocate_exact_size(Vec2::new(16.0, 18.0), Sense::hover());
                    draw_search_icon(ui.painter(), icon_slot.center(), theme::MUTED);
                    ui.add_space(4.0);
                    let _ = ui.add(
                        egui::TextEdit::singleline(&mut app.dm_search)
                            .hint_text(
                                RichText::new("Encontre ou comece uma conversa")
                                    .color(theme::MUTED),
                            )
                            .frame(egui::Frame::NONE)
                            .desired_width(ui.available_width())
                            .font(egui::FontId::proportional(13.0)),
                    );
                });
            });
    });
    ui.add_space(10.0);
    if sidebar_nav_row(
        ui,
        draw_person_icon,
        "Amigos",
        app.home_page == crate::ui::friends::HomePage::Friends,
    ) {
        app.home_page = crate::ui::friends::HomePage::Friends;
        app.selected_channel = None;
    }
    if sidebar_nav_row(
        ui,
        draw_mail_icon,
        "Solicitações de amizade",
        app.home_page == crate::ui::friends::HomePage::Pending,
    ) {
        app.home_page = crate::ui::friends::HomePage::Pending;
        app.selected_channel = None;
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.add_space(16.0);
        ui.label(
            RichText::new("Mensagens diretas")
                .small()
                .family(theme::bold())
                .color(theme::MUTED),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            if nav_plus_button(ui) {
                app.home_page = crate::ui::friends::HomePage::NewConversation;
                app.selected_channel = None;
            }
        });
    });
    ui.add_space(6.0);
}

/// Sidebar top on a guild: its name.
fn guild_sidebar_top(app: &mut VesktopApp, ui: &mut egui::Ui) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.add_space(16.0);
        let title = app
            .selected_guild
            .as_deref()
            .and_then(|id| app.guilds.iter().find(|guild| guild.id == id))
            .map(|guild| guild.name.clone())
            .unwrap_or_default();
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
        ui.painter().text(
            pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            &title,
            egui::FontId::new(15.5, theme::bold()),
            theme::TEXT,
        );
    });
    ui.add_space(6.0);
}

/// A muted navigation row with a painter icon, matching Discord's
/// "Amigos" / "Solicitações de mensagens" entries.
fn sidebar_nav_row(
    ui: &mut egui::Ui,
    icon: fn(&egui::Painter, egui::Pos2, egui::Color32),
    label: &str,
    selected: bool,
) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
    ui.painter().rect_filled(
        rect,
        theme::RADIUS_SM,
        crate::ui::control_bg(ui, &response, selected),
    );
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    content.spacing_mut().item_spacing = Vec2::ZERO;
    content.add_space(16.0);
    icon(
        content.painter(),
        pos2(content.cursor().left() + 7.0, rect.center().y),
        theme::MUTED,
    );
    content.add_space(26.0);
    content.label(RichText::new(label).size(14.0).color(theme::MUTED));
    response.clicked()
}

/// The "+" next to "Mensagens diretas": a small circle that brightens on
/// hover, like Discord's new-conversation button.
fn nav_plus_button(ui: &mut egui::Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
    let color = fade(
        ui,
        response.id,
        response.hovered() || response.is_pointer_button_down_on(),
        theme::MUTED,
        theme::TEXT,
    );
    let stroke = egui::Stroke::new(1.6, color);
    let c = rect.center();
    ui.painter()
        .line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], stroke);
    ui.painter()
        .line_segment([pos2(c.x, c.y - 5.0), pos2(c.x, c.y + 5.0)], stroke);
    response.on_hover_text("Nova conversa").clicked()
}

/// The bottom block, kept in Discord's pattern (docs/UI.md §5): a "Voz
/// conectada" status row, the mic/screen/sound button row, and
/// the user panel with mic/headset + chevrons.
fn user_panel(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        Vec2::new(width, USER_PANEL_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            // Every gap below is an explicit add_space; the default 8px item
            // spacing is what pushed the gear off the panel edge.
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            // Control cluster: [mic ⌄] [headset ⌄] [gear] — 20px icons, 12px
            // chevrons, 2px inside a pair and 4px between pairs, plus 8px of
            // right padding.
            let buttons_width = 5.0 * 2.0 + 3.0 * 20.0 + 2.0 * 12.0 + 8.0;
            let name_width = (width - 8.0 - 32.0 - 8.0 - buttons_width).max(40.0);
            // Avatar and name together open the status menu, as in Discord.
            let panel = ui.max_rect();
            let profile_rect = egui::Rect::from_min_max(
                pos2(panel.left() + 4.0, panel.center().y - 20.0),
                pos2(
                    panel.left() + 8.0 + 32.0 + 8.0 + name_width,
                    panel.center().y + 20.0,
                ),
            );
            let profile = ui
                .interact(profile_rect, ui.id().with("own_status"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            ui.painter().rect_filled(
                profile_rect,
                4.0,
                crate::ui::control_bg(ui, &profile, false),
            );
            status_popup(app, &profile);
            ui.add_space(8.0);
            let name = app
                .me
                .as_ref()
                .map(|me| me.display_name().to_string())
                .unwrap_or_default();
            let avatar_url = app.me.as_ref().map(util::user_avatar_url);
            let texture = avatar_url
                .as_deref()
                .and_then(|url| app.images.get(ui.ctx(), &app.handle, url));
            // One allocation owns the avatar slot: painting the texture at a
            // second allocation put the photo and the status dot in
            // different places.
            let (avatar_rect, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
            match texture {
                Some(texture) => {
                    ui.painter().image(
                        texture.id(),
                        avatar_rect,
                        egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                None => {
                    ui.painter()
                        .circle_filled(avatar_rect.center(), 16.0, util::name_color(&name));
                }
            }
            crate::ui::presence_dot(ui, avatar_rect, Some(&app.settings.status), theme::RAIL);
            ui.add_space(8.0);

            // Name over the status line: "● Em voz" while connected, else the
            // gateway state. Truncated so the buttons never overlap it.
            let in_voice = matches!(
                app.voice,
                VoiceConn::Connecting { .. } | VoiceConn::Connected { .. }
            );
            let (line, color) = if in_voice {
                ("● Em voz".to_string(), theme::GREEN)
            } else {
                match &app.conn {
                    ConnState::Disconnected(..) => ("Reconectando…".to_string(), theme::YELLOW),
                    ConnState::Connecting => ("Conectando…".to_string(), theme::MUTED),
                    _ => (status_line(&app.settings), theme::MUTED),
                }
            };
            ui.allocate_ui_with_layout(
                Vec2::new(name_width, 32.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add_space(2.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&name)
                                .size(14.0)
                                .family(theme::bold())
                                .color(theme::TEXT),
                        )
                        .truncate()
                        .selectable(false),
                    );
                    ui.add(
                        egui::Label::new(RichText::new(line).small().color(color))
                            .truncate()
                            .selectable(false),
                    );
                },
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                ui.add_space(8.0);
                if compact_button(ui, "Configurações", |painter, center, color| {
                    crate::ui::icons::draw(
                        painter,
                        crate::ui::icons::Icon::Settings,
                        center,
                        color,
                        0.0,
                    );
                })
                .clicked()
                {
                    app.settings_open = !app.settings_open;
                }
                ui.add_space(4.0);
                // Right-to-left layout places each new widget to the left of
                // the previous one, so a chevron is added *before* its icon
                // to render on the icon's right, Discord-style.
                let deaf = app.voice_deaf;
                let chevron = compact_button(ui, "Dispositivos de áudio", draw_chevron);
                devices_popup(app, &chevron, true);
                if compact_button(
                    ui,
                    if deaf {
                        "Ativar áudio"
                    } else {
                        "Silenciar áudio (surdo)"
                    },
                    |painter, center, color| {
                        let color = if deaf { theme::RED } else { color };
                        draw_headset(painter, center, color, deaf.then_some(theme::RAIL), 1.0)
                    },
                )
                .clicked()
                {
                    app.set_voice_deaf(!deaf);
                }
                let muted = app.voice_muted;
                let chevron = compact_button(ui, "Dispositivos de voz", draw_chevron);
                devices_popup(app, &chevron, false);
                if compact_button(
                    ui,
                    if muted {
                        "Ativar microfone"
                    } else {
                        "Silenciar microfone"
                    },
                    |painter, center, color| {
                        let color = if muted { theme::RED } else { color };
                        draw_mic(painter, center, color, muted.then_some(theme::RAIL), 1.0)
                    },
                )
                .clicked()
                {
                    app.set_voice_mute(!muted);
                }
            });
        },
    );
}

/// The mic/headset chevron's popup: device, input controls and a link to
/// the voice settings, as in Discord. Stays open while adjusting a slider.
fn devices_popup(app: &mut VesktopApp, chevron: &egui::Response, output: bool) {
    egui::Popup::menu(chevron)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::TOP_START)
        .width(260.0)
        .show(|ui| crate::ui::settings_window::voice_devices_menu(app, ui, output));
}

/// The line under the own name: the custom status, else the status label.
fn status_line(settings: &crate::settings::Settings) -> String {
    let custom = &settings.custom_status;
    if !custom.is_empty() {
        return format!("{} {}", custom.emoji.trim(), custom.text.trim())
            .trim()
            .to_string();
    }
    crate::settings::STATUSES
        .iter()
        .find(|(status, _)| *status == settings.status)
        .map_or("Online", |(_, label)| label)
        .to_string()
}

/// The avatar/name menu: the four statuses and the custom status, saved as
/// soon as one is picked (Discord's own status menu, in one popover).
fn status_popup(app: &mut VesktopApp, anchor: &egui::Response) {
    let draft_id = anchor.id.with("custom_status_draft");
    if anchor.clicked() {
        // Each opening starts from the saved custom status.
        anchor
            .ctx
            .data_mut(|data| data.remove::<CustomStatus>(draft_id));
    }
    egui::Popup::menu(anchor)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::TOP_START)
        .width(240.0)
        .show(|ui| {
            for (status, label) in crate::settings::STATUSES {
                let selected = app.settings.status == status;
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
                ui.painter()
                    .rect_filled(rect, 4.0, crate::ui::control_bg(ui, &response, selected));
                ui.painter().circle_filled(
                    pos2(rect.left() + 14.0, rect.center().y),
                    5.0,
                    crate::ui::status_color(status),
                );
                ui.painter().text(
                    pos2(rect.left() + 28.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::proportional(14.0),
                    theme::TEXT,
                );
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    let custom = app.settings.custom_status.clone();
                    app.set_own_status(status, custom);
                    ui.close();
                }
            }
            ui.separator();
            ui.label(
                RichText::new("Status personalizado")
                    .small()
                    .color(theme::MUTED),
            );
            let mut draft = ui
                .data(|data| data.get_temp::<CustomStatus>(draft_id))
                .unwrap_or_else(|| app.settings.custom_status.clone());
            let mut submit = false;
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.emoji)
                        .hint_text("🙂")
                        .char_limit(8)
                        .desired_width(28.0),
                )
                .on_hover_text("Emoji (opcional)");
                let text = ui.add(
                    egui::TextEdit::singleline(&mut draft.text)
                        .hint_text("O que está rolando?")
                        .char_limit(128)
                        .desired_width(f32::INFINITY),
                );
                submit = text.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            });
            ui.horizontal(|ui| {
                submit |= ui.button("Salvar").clicked();
                if !app.settings.custom_status.is_empty() && ui.button("Limpar").clicked() {
                    draft = CustomStatus::default();
                    submit = true;
                }
            });
            if submit {
                let status = app.settings.status.clone();
                app.set_own_status(&status, draft);
                ui.data_mut(|data| data.remove::<CustomStatus>(draft_id));
                ui.close();
            } else {
                ui.data_mut(|data| data.insert_temp(draft_id, draft));
            }
        });
}

/// Frameless icon button, Discord-style: a subtle box only while hovered.
/// The glyph is a painter callback so icons stay crisp — emoji fonts miss
/// half these symbols and render tofu boxes.
fn compact_button(
    ui: &mut egui::Ui,
    tooltip: &str,
    draw: impl FnOnce(&egui::Painter, egui::Pos2, egui::Color32),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(20.0, 26.0), Sense::click());
    let hovered = response.hovered();
    ui.painter()
        .rect_filled(rect, 4.0, crate::ui::control_bg(ui, &response, false));
    let color = fade(
        ui,
        response.id.with("glyph"),
        hovered,
        theme::TEXT,
        theme::WHITE,
    );
    let motion = crate::ui::animate_bool(
        ui.ctx(),
        response.id.with("icon_hover"),
        response.hovered(),
        0.12,
    );
    draw(ui.painter(), rect.center() - Vec2::new(0.0, motion), color);
    response.on_hover_text(tooltip)
}

/// Discord's microphone in a 20px box times `scale`: filled capsule,
/// U-shaped holder, stem and foot. `slash` crosses it out, cutting the gap
/// around the stroke in that background color.
fn draw_mic(
    painter: &egui::Painter,
    center: egui::Pos2,
    color: egui::Color32,
    slash: Option<egui::Color32>,
    scale: f32,
) {
    let at = |x: f32, y: f32| center + Vec2::new(x, y) * scale;
    let stroke = egui::Stroke::new(1.8 * scale, color);
    painter.rect_filled(
        egui::Rect::from_min_max(at(-3.5, -9.0), at(3.5, 2.5)),
        3.5 * scale,
        color,
    );
    let holder: Vec<egui::Pos2> = (0..=12)
        .map(|i| {
            let angle = std::f32::consts::PI * i as f32 / 12.0;
            at(6.5 * angle.cos(), -1.5 + 6.5 * angle.sin())
        })
        .collect();
    painter.add(egui::Shape::line(holder, stroke));
    painter.line_segment([at(0.0, 5.0), at(0.0, 8.5)], stroke);
    painter.line_segment([at(-4.0, 8.5), at(4.0, 8.5)], stroke);
    if let Some(background) = slash {
        draw_slash(painter, center, color, background, scale);
    }
}

/// Discord's headphones: headband arc and two filled ear cups; `slash` as
/// in [`draw_mic`].
fn draw_headset(
    painter: &egui::Painter,
    center: egui::Pos2,
    color: egui::Color32,
    slash: Option<egui::Color32>,
    scale: f32,
) {
    let at = |x: f32, y: f32| center + Vec2::new(x, y) * scale;
    let stroke = egui::Stroke::new(1.8 * scale, color);
    let mut band: Vec<egui::Pos2> = vec![at(7.5, 5.0)];
    band.extend((0..=12).map(|i| {
        let angle = std::f32::consts::PI * i as f32 / 12.0;
        at(7.5 * angle.cos(), 1.0 - 7.5 * angle.sin())
    }));
    band.push(at(-7.5, 5.0));
    painter.add(egui::Shape::line(band, stroke));
    for x in [-8.5, 4.5] {
        painter.rect_filled(
            egui::Rect::from_min_max(at(x, 2.0), at(x + 4.0, 9.0)),
            1.5 * scale,
            color,
        );
    }
    if let Some(background) = slash {
        draw_slash(painter, center, color, background, scale);
    }
}

/// The "/" strike of Discord's slashed icons, with a knocked-out margin so
/// it reads over the filled parts.
fn draw_slash(
    painter: &egui::Painter,
    center: egui::Pos2,
    color: egui::Color32,
    background: egui::Color32,
    scale: f32,
) {
    let ends = [
        center + Vec2::new(8.0, -8.5) * scale,
        center + Vec2::new(-8.0, 8.5) * scale,
    ];
    painter.line_segment(ends, egui::Stroke::new(4.6 * scale, background));
    painter.line_segment(ends, egui::Stroke::new(1.8 * scale, color));
}

/// A mute/deafen marker in a voice member row: red glyph when the member
/// did it, white glyph on a red badge when the server did.
fn voice_state_icon(
    ui: &mut egui::Ui,
    draw: fn(&egui::Painter, egui::Pos2, egui::Color32, Option<egui::Color32>, f32),
    server: bool,
    tooltip: &str,
) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
    if server {
        ui.painter().rect_filled(rect, 4.0, theme::RED);
        draw(
            ui.painter(),
            rect.center(),
            theme::WHITE,
            Some(theme::RED),
            0.7,
        );
    } else {
        draw(
            ui.painter(),
            rect.center(),
            theme::RED,
            Some(theme::RAIL),
            0.8,
        );
    }
    response.on_hover_text(tooltip);
}

/// Small downward options chevron.
fn draw_chevron(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.6, color);
    painter.line_segment(
        [
            pos2(center.x - 3.0, center.y - 1.5),
            pos2(center.x, center.y + 1.5),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(center.x, center.y + 1.5),
            pos2(center.x + 3.0, center.y - 1.5),
        ],
        stroke,
    );
}

/// Magnifying glass for the conversation search box.
pub(crate) fn draw_search_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    painter.circle_stroke(pos2(center.x - 0.5, center.y - 0.5), 4.5, stroke);
    painter.line_segment(
        [
            pos2(center.x + 3.0, center.y + 3.0),
            pos2(center.x + 6.5, center.y + 6.5),
        ],
        stroke,
    );
}

/// Person silhouette for the "Amigos" row: head plus shoulders.
fn draw_person_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    painter.circle_filled(pos2(center.x, center.y - 3.5), 3.0, color);
    let shoulders =
        egui::Rect::from_min_size(pos2(center.x - 5.0, center.y + 0.5), Vec2::new(10.0, 6.0));
    painter.rect_filled(shoulders, 4.0, color);
}

/// Envelope for the "Solicitações de mensagens" row.
fn draw_mail_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.4, color);
    let rect = egui::Rect::from_center_size(center, Vec2::new(12.0, 9.0));
    painter.rect_stroke(rect, 1.5, stroke, egui::StrokeKind::Inside);
    painter.line_segment(
        [
            pos2(rect.left() + 0.5, rect.top() + 0.5),
            pos2(center.x, center.y + 1.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(center.x, center.y + 1.0),
            pos2(rect.right() - 0.5, rect.top() + 0.5),
        ],
        stroke,
    );
}

/// The "Voz conectada" block above the user panel, Discord-style: a status
/// row (signal icon, green title, "canal / servidor") plus the mic / screen
/// share / soundboard / music button row. Absent while disconnected;
/// the phone button also dismisses a failed attempt.
fn voice_panel(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let status = app.voice.clone();
    let (accent, title, channel_id, guild_id, error) = match &status {
        VoiceConn::Disconnected => return,
        VoiceConn::Failed { error } => (
            theme::RED,
            "Falha na voz".to_string(),
            None,
            None,
            Some(error.clone()),
        ),
        VoiceConn::Connecting {
            guild_id,
            channel_id,
        } => (
            theme::YELLOW,
            "Conectando…".to_string(),
            Some(channel_id.clone()),
            Some(guild_id.clone()),
            None,
        ),
        VoiceConn::Connected {
            guild_id,
            channel_id,
        } => (
            theme::GREEN,
            "Voz conectada".to_string(),
            Some(channel_id.clone()),
            Some(guild_id.clone()),
            None,
        ),
    };

    let in_voice = channel_id.is_some();
    let subtitle = match (&channel_id, &guild_id) {
        (Some(channel_id), Some(guild_id)) => {
            let guild = app
                .guilds
                .iter()
                .find(|guild| &guild.id == guild_id)
                .map(|guild| guild.name.clone())
                .unwrap_or_default();
            let channel = app.channel_name(channel_id);
            if channel.is_empty() {
                guild
            } else if guild.is_empty() {
                channel
            } else {
                format!("{channel} / {guild}")
            }
        }
        _ => error.clone().unwrap_or_default(),
    };

    // Status row.
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::RAIL);
    signal_icon(ui, theme::GREEN, pos2(rect.left() + 12.0, rect.center().y));
    ui.painter().text(
        pos2(rect.left() + 34.0, rect.center().y - 8.0),
        egui::Align2::LEFT_CENTER,
        &title,
        egui::FontId::proportional(13.5),
        accent,
    );
    if !subtitle.is_empty() {
        ui.painter().text(
            pos2(rect.left() + 34.0, rect.center().y + 9.0),
            egui::Align2::LEFT_CENTER,
            &subtitle,
            egui::FontId::proportional(11.5),
            theme::MUTED,
        );
    }
    let mut leave = false;
    let disconnect = egui::Rect::from_center_size(
        pos2(rect.right() - 22.0, rect.center().y),
        Vec2::splat(28.0),
    );
    let mut disconnect_ui = ui.new_child(egui::UiBuilder::new().max_rect(disconnect));
    if crate::ui::icons::button(
        &mut disconnect_ui,
        crate::ui::icons::Icon::Disconnect,
        Vec2::splat(28.0),
        false,
        "Desconectar da voz",
    )
    .clicked()
    {
        leave = true;
    }

    // Action row: screen share, soundboard, DJ (mute lives in
    // the user panel below, as in Discord).
    if in_voice {
        ui.add_space(2.0);
        let width = ui.available_width();
        ui.allocate_ui_with_layout(
            Vec2::new(width, VOICE_ACTIONS_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_space(8.0);
                let gap = 6.0;
                ui.spacing_mut().item_spacing.x = gap;
                let button_width = (width - 16.0 - 3.0 * gap) / 4.0;
                let action = |ui: &mut egui::Ui, icon, selected, tooltip: &str| {
                    crate::ui::icons::button(
                        ui,
                        icon,
                        Vec2::new(button_width, VOICE_ACTIONS_H),
                        selected,
                        tooltip,
                    )
                    .clicked()
                };
                let sharing = app.screen.is_some();
                if action(
                    ui,
                    crate::ui::icons::Icon::Screen,
                    sharing,
                    if sharing {
                        "Parar de compartilhar"
                    } else {
                        "Compartilhar tela"
                    },
                ) {
                    app.toggle_screen_share();
                }
                let camera = app.camera.is_some();
                if action(
                    ui,
                    crate::ui::icons::Icon::Camera,
                    camera,
                    if camera {
                        "Desligar câmera"
                    } else {
                        "Ligar câmera"
                    },
                ) {
                    app.toggle_camera(ui.ctx());
                }
                let soundboard = crate::ui::icons::button(
                    ui,
                    crate::ui::icons::Icon::Soundboard,
                    Vec2::new(button_width, VOICE_ACTIONS_H),
                    app.extras.soundboard_open,
                    "Soundboard",
                );
                crate::ui::voice_extras::soundboard(app, &soundboard);
                let extras = &mut app.extras;
                if action(
                    ui,
                    crate::ui::icons::Icon::Music,
                    extras.music_open,
                    "Música (DJ)",
                ) {
                    extras.music_open = !extras.music_open;
                }
                ui.add_space(8.0);
            },
        );
    }
    ui.add_space(4.0);

    if leave {
        app.leave_voice();
    }
}

/// Three ascending signal bars, Discord's connected-voice glyph.
fn signal_icon(ui: &mut egui::Ui, color: egui::Color32, center: egui::Pos2) {
    let painter = ui.painter();
    for (index, height) in [5.0, 9.0, 13.0].into_iter().enumerate() {
        let x = center.x + index as f32 * 5.0 - 5.0;
        painter.rect_filled(
            egui::Rect::from_min_size(pos2(x, center.y - height / 2.0), Vec2::new(3.0, height)),
            1.0,
            color,
        );
    }
}

/// A member under a voice channel: avatar (green ring while speaking),
/// name, mute/deaf icons and a menu (click or right-click) with their
/// volume and local mute.
fn voice_member_row(app: &mut VesktopApp, ui: &mut egui::Ui, guild_id: &str, member: &VoiceState) {
    let fetched = app.voice_member_name(guild_id, &member.user_id);
    let label = if fetched.is_empty() {
        "…".to_string()
    } else {
        fetched
    };
    let speaking = app.is_speaking(&member.user_id);
    let width = ui.available_width();
    // A live member is clickable: it opens their stream.
    let live = app.is_live(&member.user_id, member.self_stream);
    // Their webcam on FockyTV: a click opens it when there's no live.
    let camera = app.has_camera(&member.user_id);
    let is_me = app.me.as_ref().is_some_and(|me| me.id == member.user_id);
    let watchable = (live || camera) && !is_me;
    let sense = if is_me {
        Sense::hover()
    } else {
        Sense::click()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 34.0), sense);
    ui.painter().rect_filled(
        rect,
        4.0,
        fade(
            ui,
            response.id,
            !is_me && response.hovered(),
            egui::Color32::TRANSPARENT,
            theme::HOVER,
        ),
    );
    let response = if watchable && live {
        response.on_hover_text("Assistir à transmissão (clique direito: volume)")
    } else if watchable {
        response.on_hover_text("Ver câmera (clique direito: volume)")
    } else {
        response
    };
    if watchable && response.clicked() {
        if live {
            app.watch(&member.user_id);
        } else {
            app.watch_camera(&member.user_id);
        }
    }
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(
                pos2(rect.left() + 34.0, rect.top() + 1.0),
                pos2(rect.right() - 8.0, rect.bottom() - 1.0),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    // The avatar comes from the resolved member; the letter circle is only
    // the fallback while the member lookup is still in flight.
    let member_user = app.user_cache.get(&member.user_id).cloned();
    let avatar_texture = member_user.as_ref().and_then(|user| {
        let url = util::user_avatar_url(user);
        app.images.get(content.ctx(), &app.handle, &url)
    });
    let avatar = match avatar_texture {
        Some(texture) => draw_texture_round(&mut content, &texture, 24.0),
        None => initial_circle(&mut content, 24.0, &label, util::name_color(&label)),
    };
    crate::ui::presence_dot(
        &content,
        avatar,
        app.presence.get(&member.user_id).map(String::as_str),
        theme::RAIL,
    );
    if speaking {
        content.painter().circle_stroke(
            avatar.center(),
            avatar.width() / 2.0 + 1.5,
            egui::Stroke::new(2.0, theme::GREEN),
        );
    }
    content.add_space(6.0);
    let locally_muted = app.settings.muted_users.contains(&member.user_id);
    // Right to left, Discord's order: [AO VIVO] [mic] [headset] at the row
    // end; the name takes what is left and truncates. A server mute/deafen
    // replaces the member's own marker.
    content.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
        if member.deaf {
            voice_state_icon(ui, draw_headset, true, "Ensurdecido pelo servidor");
        } else if member.self_deaf {
            voice_state_icon(ui, draw_headset, false, "Áudio desativado");
        }
        if member.mute {
            voice_state_icon(ui, draw_mic, true, "Silenciado pelo servidor");
        } else if member.self_mute {
            voice_state_icon(ui, draw_mic, false, "Microfone silenciado");
        }
        if live {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(" AO VIVO ")
                        .size(9.5)
                        .family(theme::bold())
                        .color(egui::Color32::WHITE)
                        .background_color(theme::RED),
                )
                .selectable(false),
            );
        }
        if camera {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(" CÂMERA ")
                        .size(9.5)
                        .family(theme::bold())
                        .color(egui::Color32::WHITE)
                        .background_color(theme::BLURPLE),
                )
                .selectable(false),
            );
        }
        if locally_muted {
            ui.add(
                egui::Label::new(RichText::new("🔇").size(13.0).color(theme::RED))
                    .selectable(false),
            )
            .on_hover_text("Silenciado por você");
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(
                egui::Label::new(RichText::new(&label).size(13.0).color(theme::MUTED))
                    .truncate()
                    .selectable(false),
            );
        });
    });

    if is_me {
        return;
    }
    let mut percent = *app
        .settings
        .user_volumes
        .get(&member.user_id)
        .unwrap_or(&100);
    // Discord-style: right-click anywhere, or left-click when the click
    // isn't taken by "watch stream".
    let open = if response.secondary_clicked() || (response.clicked() && !watchable) {
        Some(egui::SetOpenCommand::Bool(true))
    } else {
        None
    };
    egui::Popup::context_menu(&response)
        .open_memory(open)
        .show(|ui| {
            ui.set_min_width(200.0);
            ui.label(RichText::new(&label).strong().size(13.0));
            // A click opens the live; the camera next to it is here.
            if live && camera && ui.button("Ver câmera").clicked() {
                app.watch_camera(&member.user_id);
                ui.close();
            }
            ui.separator();
            ui.label(
                RichText::new("Volume do usuário")
                    .small()
                    .color(theme::MUTED),
            );
            if ui
                .add(egui::Slider::new(&mut percent, 0..=200).suffix("%"))
                .changed()
            {
                app.set_user_volume(member.user_id.clone(), percent);
            }
            let mut muted = locally_muted;
            if ui.checkbox(&mut muted, "Silenciar").changed() {
                app.set_user_muted(member.user_id.clone(), muted);
            }
        });
}

fn paint_dm_list(app: &mut VesktopApp, ui: &mut egui::Ui) {
    if app.dm_channels.is_empty() {
        ui.add_space(8.0);
        ui.label(
            RichText::new("Nenhuma conversa ainda.")
                .small()
                .color(theme::MUTED),
        );
        return;
    }
    let dms = app.dm_channels.clone();
    let mut clicked: Option<String> = None;
    let needle = app.dm_search.trim().to_lowercase();
    for channel in dms {
        let name = channel.display_name();
        if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
            continue;
        }
        let selected = app.selected_channel.as_deref() == Some(channel.id.as_str());
        let unread = app.unread.get(&channel.id).copied().unwrap_or(0) > 0;
        let response = row(
            app,
            ui,
            Some(&channel),
            None,
            &name,
            selected,
            unread,
            false,
            None,
        );
        if response.clicked() {
            clicked = Some(channel.id.clone());
        }
    }
    if let Some(id) = clicked {
        app.select_channel(id);
    }
}

fn paint_guild_channels(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let Some(guild_id) = app.selected_guild.clone() else {
        return;
    };
    if !app.guild_channels.contains_key(&guild_id) {
        ui.add_space(8.0);
        ui.label(
            RichText::new("Carregando canais…")
                .small()
                .color(theme::MUTED),
        );
        return;
    }
    let channels = app
        .guild_channels
        .get(&guild_id)
        .cloned()
        .unwrap_or_default();
    let selected = app.selected_channel.clone();
    let mut clicked: Option<String> = None;
    let mut clicked_voice: Option<String> = None;

    let render = |app: &mut VesktopApp,
                  ui: &mut egui::Ui,
                  channel: &Channel,
                  clicked: &mut Option<String>,
                  clicked_voice: &mut Option<String>| {
        let is_selected = selected.as_deref() == Some(channel.id.as_str());
        let unread = app.unread.get(&channel.id).copied().unwrap_or(0) > 0;
        let name = channel.display_name();
        if channel.is_voice() {
            // Highlight the channel we are connecting to or in.
            let active = match &app.voice {
                VoiceConn::Connecting { channel_id, .. }
                | VoiceConn::Connected { channel_id, .. } => channel_id == &channel.id,
                _ => false,
            };
            let connected = matches!(&app.voice, VoiceConn::Connected { channel_id, .. } if channel_id == &channel.id);
            let elapsed = connected
                .then(|| {
                    app.voice_connected_at
                        .map(|since| since.elapsed().as_secs())
                })
                .flatten();
            if row(
                app,
                ui,
                None,
                Some(crate::ui::icons::Icon::Voice),
                &name,
                active,
                unread,
                false,
                elapsed,
            )
            .clicked()
            {
                *clicked_voice = Some(channel.id.clone());
            }
            for member in app.voice_members(&channel.id) {
                voice_member_row(app, ui, &guild_id, &member);
            }
        } else {
            let prefix = if channel.kind == crate::model::CHANNEL_KIND_ANNOUNCEMENT {
                crate::ui::icons::Icon::Announcement
            } else {
                crate::ui::icons::Icon::TextChannel
            };
            if row(
                app,
                ui,
                None,
                Some(prefix),
                &name,
                is_selected,
                unread,
                false,
                None,
            )
            .clicked()
            {
                *clicked = Some(channel.id.clone());
            }
            crate::ui::threads::paint_sidebar_threads(
                app,
                ui,
                &guild_id,
                &channel.id,
                selected.as_deref(),
                clicked,
            );
        }
    };

    // Uncategorized channels first, then each category with its children.
    let mut ungrouped: Vec<&Channel> = channels
        .iter()
        .filter(|channel| channel.parent_id.is_none() && channel.kind != CHANNEL_KIND_CATEGORY)
        .collect();
    ungrouped.sort_by_key(|channel| (channel.position, channel.name.clone().unwrap_or_default()));
    for channel in ungrouped {
        render(app, ui, channel, &mut clicked, &mut clicked_voice);
    }

    let mut categories: Vec<&Channel> = channels
        .iter()
        .filter(|channel| channel.kind == CHANNEL_KIND_CATEGORY)
        .collect();
    categories.sort_by_key(|channel| (channel.position, channel.name.clone().unwrap_or_default()));
    for category in categories {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let (chevron, _) = ui.allocate_exact_size(Vec2::new(14.0, 14.0), Sense::hover());
            draw_chevron(ui.painter(), chevron.center(), theme::MUTED);
            ui.add_space(2.0);
            ui.label(
                RichText::new(category.display_name().to_uppercase())
                    .size(12.0)
                    .family(theme::bold())
                    .color(theme::MUTED),
            );
        });
        ui.add_space(2.0);
        let mut children: Vec<&Channel> = channels
            .iter()
            .filter(|channel| channel.parent_id.as_deref() == Some(category.id.as_str()))
            .collect();
        children
            .sort_by_key(|channel| (channel.position, channel.name.clone().unwrap_or_default()));
        for channel in children {
            render(app, ui, channel, &mut clicked, &mut clicked_voice);
        }
    }

    if let Some(id) = clicked {
        app.select_channel(id);
    }
    if let Some(id) = clicked_voice {
        app.join_voice(guild_id.clone(), id);
    }
}

#[allow(clippy::too_many_arguments)]
fn row(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    dm: Option<&Channel>,
    prefix: Option<crate::ui::icons::Icon>,
    label: &str,
    selected: bool,
    unread: bool,
    dimmed: bool,
    elapsed: Option<u64>,
) -> egui::Response {
    let width = ui.available_width();
    // Discord: 42px DM rows, 34px channel rows.
    let height = if dm.is_some() { 42.0 } else { 34.0 };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let hovered = response.hovered() && !dimmed;
    let bg = crate::ui::control_bg(ui, &response, selected);
    ui.painter().rect_filled(rect, theme::RADIUS_SM, bg);
    // Unread: the white pill on the sidebar's left edge, grown in.
    let pill = crate::ui::animate_bool(
        ui.ctx(),
        response.id.with("pill"),
        unread && !selected,
        0.12,
    );
    if pill > 0.0 {
        let left = ui.clip_rect().left();
        ui.painter().rect_filled(
            egui::Rect::from_center_size(pos2(left, rect.center().y), Vec2::new(8.0, 8.0 * pill)),
            4.0,
            theme::WHITE,
        );
    }

    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(
                pos2(rect.left() + 10.0, rect.top() + 3.0),
                pos2(rect.right() - 8.0, rect.bottom() - 3.0),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    if let Some(dm) = dm {
        let texture =
            util::dm_icon_url(dm).and_then(|url| app.images.get(ui.ctx(), &app.handle, &url));
        let avatar = match texture {
            Some(texture) => draw_texture_round(&mut content, &texture, 26.0),
            None if dm.dm_partner().is_none() => crate::ui::group_avatar(&mut content, 26.0),
            None => initial_circle(&mut content, 26.0, label, util::name_color(label)),
        };
        if let Some(user) = dm.dm_partner() {
            crate::ui::presence_dot(
                &content,
                avatar,
                app.presence.get(&user.id).map(String::as_str),
                theme::RAIL.blend(bg),
            );
        }
        content.add_space(8.0);
    }
    // Read channels sit muted and brighten on hover; selected and unread
    // ones are white, unread in the bold weight.
    let color = if selected || unread {
        theme::WHITE
    } else {
        fade(
            ui,
            response.id.with("text"),
            hovered,
            theme::MUTED,
            theme::TEXT,
        )
    };
    content.spacing_mut().item_spacing = Vec2::ZERO;
    if let Some(icon) = prefix {
        if elapsed.is_some() {
            let center = pos2(content.cursor().left() + 8.0, rect.center().y);
            signal_icon(&mut content, theme::GREEN, center);
            content.add_space(16.0);
        } else {
            let (icon_rect, _) = content.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
            crate::ui::icons::draw(
                content.painter(),
                icon,
                icon_rect.center(),
                theme::MUTED,
                0.0,
            );
        }
        content.add_space(6.0);
    }
    let mut text = RichText::new(label).size(15.0).color(color);
    if unread {
        text = text.family(theme::bold());
    }
    if let Some(seconds) = elapsed {
        let timer = format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        );
        let timer_width = 66.0;
        content.add_sized(
            [content.available_width() - timer_width, height - 6.0],
            egui::Label::new(text).truncate().selectable(false),
        );
        ui.painter().text(
            pos2(rect.right() - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            timer,
            egui::FontId::monospace(11.0),
            theme::GREEN,
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    } else {
        content.add(egui::Label::new(text).truncate().selectable(false));
    }
    response
}
