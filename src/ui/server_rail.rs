//! Left rail: the DMs home button and the guild icon list, Discord-style —
//! rounded square icons with short hover responses, pill indicators on
//! the left edge and a red badge for mentions.

use egui::Id;
use egui::{Align2, CornerRadius, FontId, Image, RichText, Sense, TextureHandle, Vec2, pos2};

use crate::app::{ChannelRef, VesktopApp};
use crate::theme;
use crate::util;

const BUTTON: f32 = 48.0;
/// Servers keep rounded square silhouettes, with a small hover response.
const RADIUS_IDLE: f32 = 16.0;
const RADIUS_ACTIVE: f32 = 14.0;
const ANIM: f32 = 0.12;

pub fn paint(app: &mut VesktopApp, ui: &mut egui::Ui) {
    ui.painter().rect_filled(ui.max_rect(), 0.0, theme::RAIL);
    ui.add_space(12.0);

    let dm_unread: u64 = app
        .unread
        .iter()
        .filter(|(id, _)| matches!(app.channel_index.get(*id), Some(ChannelRef::Dm)))
        .map(|(_, count)| *count)
        .sum();
    let home_selected = app.selected_guild.is_none();
    let home = rail_button(
        ui,
        Id::new("rail_home"),
        home_selected,
        dm_unread > 0,
        0,
        "",
        None,
        "Mensagens diretas",
    );
    egui::Image::from_bytes("bytes://fastdiscord-logo.png", crate::ui::login::LOGO_PNG)
        .paint_at(ui, home.rect.shrink(9.0));
    if home.clicked() {
        app.select_home();
    }

    ui.add_space(10.0);
    let width = ui.max_rect().width();
    let center = ui.max_rect().center().x;
    let top = ui.cursor().top();
    ui.painter().line_segment(
        [
            pos2(center - width * 0.25, top),
            pos2(center + width * 0.25, top),
        ],
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
    ui.add_space(10.0);

    let mut clicked: Option<String> = None;
    // Favorites float to the top, keeping Discord's order inside each group.
    let mut guilds = app.guilds.clone();
    let favorites = &app.settings.favorite_guilds;
    guilds.sort_by_key(|guild| !favorites.contains(&guild.id));
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .scroll_bar_visibility(egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            for guild in guilds {
                let selected = app.selected_guild.as_deref() == Some(guild.id.as_str());
                let guild_id = guild.id.clone();
                let muted = app.settings.muted_guilds.contains(&guild.id);
                let unread = !muted && app.unread.iter().any(|(id, count)| {
                    *count > 0
                        && matches!(app.channel_index.get(id), Some(ChannelRef::Guild(owner)) if owner == &guild_id)
                });
                let mention_count: u64 = if muted {
                    0
                } else {
                    app.unread
                        .iter()
                        .filter(|(id, _)| {
                            matches!(app.channel_index.get(*id), Some(ChannelRef::Guild(owner)) if owner == &guild_id)
                        })
                        .map(|(id, _)| app.mentions.get(id).copied().unwrap_or(0))
                        .sum()
                };
                let texture = util::guild_icon_url(&guild)
                    .and_then(|url| app.images.get(ui.ctx(), &app.handle, &url));
                let response = rail_button(
                    ui,
                    Id::new(("rail_guild", guild.id.clone())),
                    selected,
                    unread,
                    mention_count,
                    &initial(&guild.name),
                    texture.as_ref(),
                    &guild.name,
                );
                if response.clicked() {
                    clicked = Some(guild.id.clone());
                }
                guild_context_menu(app, &response, &guild);
                ui.add_space(8.0);
            }
        });
    if let Some(id) = clicked {
        app.select_guild(&id);
    }
}

/// Right-click menu on a rail icon: pin it to the top, mute it, or leave.
/// Leaving asks for a second click within a few seconds, since it kicks
/// you out of the server for good.
fn guild_context_menu(app: &mut VesktopApp, response: &egui::Response, guild: &crate::model::Guild) {
    let mut leave = false;
    egui::Popup::context_menu(response).show(|ui| {
        ui.set_min_width(190.0);
        let favorite = app.settings.favorite_guilds.contains(&guild.id);
        let label = if favorite {
            "Remover dos favoritos"
        } else {
            "Favoritar"
        };
        if ui.button(label).clicked() {
            app.toggle_favorite_guild(&guild.id);
            ui.close();
        }
        let muted = app.settings.muted_guilds.contains(&guild.id);
        let label = if muted {
            "Dessilenciar servidor"
        } else {
            "Silenciar servidor"
        };
        if ui.button(label).clicked() {
            app.toggle_guild_mute(&guild.id);
            ui.close();
        }
        ui.separator();
        let arm_id = Id::new(("rail_leave_arm", &guild.id));
        let armed_at = ui
            .ctx()
            .data(|data| data.get_temp::<std::time::Instant>(arm_id));
        let armed = armed_at.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(5));
        let label = if armed {
            "Confirmar saída do servidor"
        } else {
            "Sair do servidor"
        };
        if ui.button(RichText::new(label).color(theme::RED)).clicked() {
            if armed {
                leave = true;
                ui.close();
            } else {
                ui.ctx()
                    .data_mut(|data| data.insert_temp(arm_id, std::time::Instant::now()));
            }
        }
    });
    if leave {
        app.leave_guild(&guild.id);
    }
}

fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string())
}

/// How many unread mentions a guild has, shown as the red badge.
#[allow(clippy::too_many_arguments)]
fn rail_button(
    ui: &mut egui::Ui,
    id: Id,
    selected: bool,
    unread: bool,
    mentions: u64,
    label: &str,
    texture: Option<&TextureHandle>,
    tooltip: &str,
) -> egui::Response {
    let ctx = ui.ctx().clone();
    // Reserve the slot in the flow, then center the button in the rail's
    // width; interaction follows the painted rect.
    let (slot, _) = ui.allocate_exact_size(Vec2::splat(BUTTON), Sense::hover());
    let rect = egui::Rect::from_min_size(
        pos2(ui.max_rect().center().x - BUTTON / 2.0, slot.top()),
        Vec2::splat(BUTTON),
    );
    let response = ui.interact(rect, id, Sense::click());
    let press = crate::ui::animate_bool(
        &ctx,
        id.with("press"),
        response.is_pointer_button_down_on(),
        0.08,
    );
    let rect = rect.shrink(2.0 * press);

    let t = crate::ui::animate_bool(&ctx, id.with("shape"), selected || response.hovered(), ANIM);
    let radius = RADIUS_IDLE + (RADIUS_ACTIVE - RADIUS_IDLE) * t;

    // Discord's background only appears while hovered or selected.
    if t > 0.01 {
        let bg = if selected {
            theme::SELECTED
        } else {
            theme::HOVER
        };
        ui.painter()
            .rect_filled(rect, radius, theme::RAIL.lerp_to_gamma(bg, t));
    }

    match texture {
        Some(texture) => {
            Image::new((texture.id(), Vec2::splat(BUTTON)))
                .corner_radius(CornerRadius::same(radius as u8))
                .paint_at(ui, rect);
        }
        None => {
            // Icon-less guilds (and Home) sit on a gray tile that turns
            // blurple on hover, as in Discord.
            ui.painter()
                .rect_filled(rect, radius, theme::CHAT.lerp_to_gamma(theme::BLURPLE, t));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(17.0),
                theme::WHITE,
            );
        }
    }

    // Left pill: 8px dot when unread, 20px bar on hover, 40px when selected.
    let t_sel = crate::ui::animate_bool(&ctx, id.with("pill_sel"), selected, ANIM);
    let t_hov = crate::ui::animate_bool(&ctx, id.with("pill_hov"), response.hovered(), ANIM);
    let base = if unread { 8.0 } else { 0.0 };
    let height = t_sel * 40.0 + (1.0 - t_sel) * (base + t_hov * (20.0 - base));
    if height > 1.0 {
        let pill = egui::Rect::from_center_size(
            pos2(rect.left() - 10.0, rect.center().y),
            Vec2::new(4.0, height),
        );
        ui.painter().rect_filled(
            pill,
            2.0,
            theme::WHITE.gamma_multiply((height / 8.0).min(1.0)),
        );
    }

    if mentions > 0 {
        let center = pos2(rect.right() - 4.0, rect.bottom() - 4.0);
        ui.painter().circle_filled(center, 11.0, theme::RAIL);
        ui.painter().circle_filled(center, 9.5, theme::RED);
        ui.painter().text(
            center,
            Align2::CENTER_CENTER,
            badge_text(mentions),
            FontId::proportional(11.0),
            theme::WHITE,
        );
    }

    if tooltip.is_empty() {
        response
    } else {
        // Discord pins rail tooltips to the right of the icon, vertically
        // centered; no fallback aligns, so it never flips below.
        let mut hover = egui::Tooltip::for_enabled(&response);
        hover.popup = hover
            .popup
            .align(egui::RectAlign::RIGHT)
            .align_alternatives(&[])
            .gap(12.0);
        hover.show(|ui| {
            ui.set_max_width(ui.spacing().tooltip_width);
            ui.label(tooltip);
        });
        response
    }
}

fn badge_text(mentions: u64) -> String {
    if mentions > 99 {
        "99+".to_string()
    } else {
        mentions.to_string()
    }
}
