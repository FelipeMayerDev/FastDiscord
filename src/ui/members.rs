//! Member list on the right of the chat, toggled from the channel header:
//! the guild's lazy member list (member_list.rs) grouped as Discord sends
//! it, or a DM's recipients.

use egui::{Align2, Color32, FontId, Image, RichText, Sense, Vec2, pos2};

use crate::app::{ChannelRef, VesktopApp};
use crate::backend::Command;
use crate::member_list::{ListItem, MAX_RANGES, RANGE_SIZE};
use crate::model::User;
use crate::theme;
use crate::util;

pub const WIDTH: f32 = 240.0;
const ROW: f32 = 42.0;
const AVATAR: f32 = 32.0;

fn shown_id() -> egui::Id {
    egui::Id::new("show_members")
}

/// Shown by default, as in Discord; the choice holds for every channel.
pub fn shown(ctx: &egui::Context) -> bool {
    ctx.data_mut(|data| *data.get_persisted_mut_or(shown_id(), true))
}

/// The header's "people" button.
pub fn toggle(ui: &mut egui::Ui) {
    let on = shown(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    ui.painter()
        .rect_filled(rect, 4.0, crate::ui::control_bg(ui, &response, false));
    draw_people(
        ui.painter(),
        rect.center(),
        if on { theme::WHITE } else { theme::MUTED },
    );
    let tooltip = if on {
        "Ocultar lista de membros"
    } else {
        "Mostrar lista de membros"
    };
    if response.on_hover_text(tooltip).clicked() {
        ui.ctx()
            .data_mut(|data| data.insert_persisted(shown_id(), !on));
    }
}

/// Two outlined busts, Discord's member list icon.
fn draw_people(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    for (dx, scale) in [(3.5, 0.8), (-2.0, 1.0)] {
        let head = center + Vec2::new(dx, -3.0 * scale);
        painter.circle_stroke(head, 2.6 * scale, stroke);
        let shoulders: Vec<egui::Pos2> = (0..=8)
            .map(|i| {
                let angle = std::f32::consts::PI * (1.0 + i as f32 / 8.0);
                head + Vec2::new(
                    5.0 * scale * angle.cos(),
                    9.0 * scale + 4.5 * scale * angle.sin(),
                )
            })
            .collect();
        painter.add(egui::Shape::line(shoulders, stroke));
    }
}

enum Row {
    Header(String),
    Member {
        user: User,
        name: String,
        color: Color32,
    },
}

pub fn panel(app: &mut VesktopApp, ui: &mut egui::Ui, channel_id: &str) {
    if !shown(ui.ctx()) {
        return;
    }
    let (rows, more) = match app.channel_index.get(channel_id).cloned() {
        Some(ChannelRef::Guild(guild_id)) => guild_rows(app, &guild_id, channel_id),
        Some(ChannelRef::Dm) => (dm_rows(app, channel_id), None),
        None => return,
    };
    let me = app.me.as_ref().map(|me| me.id.clone());
    let mut want_more = false;
    egui::Panel::right("members")
        .exact_size(WIDTH)
        .resizable(false)
        .frame(egui::Frame::new().fill(theme::SIDEBAR))
        .show(ui, |ui| {
            if rows.is_empty() {
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Carregando membros…")
                            .small()
                            .color(theme::MUTED),
                    );
                });
                return;
            }
            let scroll = egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(8.0);
                    for row in &rows {
                        match row {
                            Row::Header(label) => {
                                ui.add_space(16.0);
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label(
                                        RichText::new(label)
                                            .size(12.0)
                                            .family(theme::bold())
                                            .color(theme::MUTED),
                                    );
                                });
                                ui.add_space(4.0);
                            }
                            Row::Member { user, name, color } => {
                                member_row(app, ui, user, name, *color, me.as_deref())
                            }
                        }
                    }
                    ui.add_space(8.0);
                });
            let bottom = scroll.state.offset.y + scroll.inner_rect.height();
            want_more = bottom >= scroll.content_size.y - ROW * 4.0;
        });
    // Near the end of what's loaded: subscribe to the next 100 rows.
    if let (true, Some((guild_id, ranges))) = (want_more, more) {
        subscribe(app, guild_id, channel_id, ranges + 1);
    }
}

fn subscribe(app: &mut VesktopApp, guild_id: String, channel_id: &str, ranges: usize) {
    app.members.subscribed = Some((channel_id.to_string(), ranges));
    app.send(Command::SubscribeMemberList {
        guild_id,
        channel_id: channel_id.to_string(),
        ranges,
    });
}

/// The guild's rows, subscribing to the channel's list when it isn't yet.
/// Also returns the guild and range count when more rows can be asked for.
fn guild_rows(
    app: &mut VesktopApp,
    guild_id: &str,
    channel_id: &str,
) -> (Vec<Row>, Option<(String, usize)>) {
    let ranges = match &app.members.subscribed {
        Some((id, ranges)) if id == channel_id => *ranges,
        _ => {
            subscribe(app, guild_id.to_string(), channel_id, 1);
            1
        }
    };
    let Some(list) = app.members.lists.get(guild_id) else {
        return (Vec::new(), None);
    };
    let roles = app.members.roles.get(guild_id);
    let mut rows = Vec::new();
    for item in list.items.iter().flatten() {
        match item {
            ListItem::Group(group) => {
                let name = match group.id.as_str() {
                    "online" => "Online".to_string(),
                    "offline" => "Offline".to_string(),
                    id => roles
                        .and_then(|roles| roles.get(id))
                        .map_or_else(|| "Cargo".to_string(), |role| role.name.clone()),
                };
                rows.push(Row::Header(format!(
                    "{} — {}",
                    name.to_uppercase(),
                    group.count
                )));
            }
            ListItem::Member(member) => {
                let color = app
                    .members
                    .color(guild_id, member)
                    .map_or(theme::TEXT, |rgb| {
                        Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
                    });
                rows.push(Row::Member {
                    user: member.user.clone(),
                    name: member.display_name().to_string(),
                    color,
                });
            }
        }
    }
    // ponytail: three ranges (300 rows) is where Discord's client stops
    // too; a longer list needs windows that follow the scroll position.
    let more = (list.items.len() >= ranges * RANGE_SIZE && ranges < MAX_RANGES)
        .then(|| (guild_id.to_string(), ranges));
    (rows, more)
}

/// A DM's recipients plus us.
fn dm_rows(app: &VesktopApp, channel_id: &str) -> Vec<Row> {
    let Some(channel) = app.dm_channels.iter().find(|c| c.id == channel_id) else {
        return Vec::new();
    };
    let users: Vec<&User> = channel.recipients.iter().chain(app.me.as_ref()).collect();
    let mut rows = vec![Row::Header(format!("MEMBROS — {}", users.len()))];
    rows.extend(users.into_iter().map(|user| Row::Member {
        user: user.clone(),
        name: user.display_name().to_string(),
        color: theme::TEXT,
    }));
    rows
}

fn member_row(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    user: &User,
    name: &str,
    color: Color32,
    me: Option<&str>,
) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW), Sense::click());
    let status = app.presence.get(&user.id).cloned();
    let offline = status.as_deref().is_none_or(|status| status == "offline");
    let inner = rect.shrink2(Vec2::new(8.0, 1.0));
    ui.painter()
        .rect_filled(inner, 4.0, crate::ui::control_bg(ui, &response, false));
    let avatar = egui::Rect::from_min_size(
        pos2(inner.left() + 8.0, inner.center().y - AVATAR / 2.0),
        Vec2::splat(AVATAR),
    );
    // Offline members are dimmed, like Discord's list.
    let tint = if offline {
        Color32::from_white_alpha(110)
    } else {
        Color32::WHITE
    };
    match app
        .images
        .get(ui.ctx(), &app.handle, &util::user_avatar_url(user))
    {
        Some(texture) => {
            Image::new((texture.id(), avatar.size()))
                .corner_radius(egui::CornerRadius::same((AVATAR / 2.0) as u8))
                .tint(tint)
                .paint_at(ui, avatar);
        }
        None => {
            ui.painter()
                .circle_filled(avatar.center(), AVATAR / 2.0, util::name_color(name));
        }
    }
    if !offline {
        crate::ui::presence_dot(
            ui,
            avatar,
            status.as_deref(),
            app.presence_device.get(&user.id).map(String::as_str),
            theme::SIDEBAR,
        );
    }
    let text_left = avatar.right() + 12.0;
    ui.painter()
        .with_clip_rect(inner.with_min_x(text_left))
        .text(
            pos2(text_left, inner.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::new(15.0, theme::bold()),
            if offline {
                color.gamma_multiply(0.5)
            } else {
                color
            },
        );

    let mut open_dm = false;
    egui::Popup::menu(&response)
        .align(egui::RectAlign::LEFT_START)
        .width(220.0)
        .show(|ui| {
            ui.label(RichText::new(name).family(theme::bold()).size(16.0));
            ui.label(
                RichText::new(format!("@{}", user.username))
                    .small()
                    .color(theme::MUTED),
            );
            if me != Some(user.id.as_str()) {
                ui.separator();
                open_dm = ui.button("Enviar mensagem").clicked();
            }
        });
    if open_dm {
        app.open_dm(user.id.clone());
    }
}
