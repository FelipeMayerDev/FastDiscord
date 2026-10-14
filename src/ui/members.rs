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
    match app.channel_index.get(channel_id).cloned() {
        Some(ChannelRef::Guild(guild_id)) => {
            let (rows, more) = guild_rows(app, &guild_id, channel_id);
            rows_panel(app, ui, rows, more, channel_id);
        }
        Some(ChannelRef::Dm) => {
            // Issue #49: a 1:1 DM shows Discord's profile card in place of
            // the member list; group DMs keep their recipients.
            let partner = app
                .dm_channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .and_then(|channel| channel.dm_partner().cloned());
            match partner {
                Some(user) => profile_card(app, ui, &user),
                None => {
                    let rows = dm_rows(app, channel_id);
                    rows_panel(app, ui, rows, None, channel_id);
                }
            }
        }
        None => {}
    }
}

fn rows_panel(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    rows: Vec<Row>,
    more: Option<(String, usize)>,
    channel_id: &str,
) {
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
        subscribe(app, guild_id, channel_id, ranges);
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
    let name_font = FontId::new(15.0, theme::bold());
    let name_color = if offline {
        color.gamma_multiply(0.5)
    } else {
        color
    };
    ui.painter()
        .with_clip_rect(inner.with_min_x(text_left))
        .text(
            pos2(text_left, inner.center().y),
            Align2::LEFT_CENTER,
            name,
            name_font.clone(),
            name_color,
        );
    // The clan tag rides right after the name, as in chat (issue #52).
    if let Some(clan) = user.clan_tag() {
        let galley = ui
            .painter()
            .layout_no_wrap(name.to_string(), name_font, name_color);
        let chip_left = text_left + galley.rect.width() + 6.0;
        if chip_left < inner.right() {
            let mut chip = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_size(
                        pos2(chip_left, inner.center().y - 9.0),
                        Vec2::new(inner.right() - chip_left, 18.0),
                    ))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            crate::ui::clan_tag(app, &mut chip, clan);
        }
    }

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

/// Issue #49: Discord's DM profile card in the right sidebar — banner,
/// avatar, add-friend button, about, member since and mutuals.
fn profile_card(app: &mut VesktopApp, ui: &mut egui::Ui, user: &User) {
    app.request_dm_profile(&user.id);
    let profile = app.dm_profiles.get(&user.id).cloned().flatten();
    let meta = profile
        .as_ref()
        .and_then(|profile| profile.user_profile.clone())
        .unwrap_or_default();
    let me = app.me.as_ref().map(|me| me.id.clone());
    let friend = app
        .relationships
        .iter()
        .any(|relation| relation.id == user.id && relation.kind == 1);

    egui::Panel::right("members")
        .exact_size(WIDTH)
        .resizable(false)
        .frame(egui::Frame::new().fill(theme::SIDEBAR))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let scroll = egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let width = ui.available_width();
                // Banner: the image when there is one, the accent color
                // behind it otherwise, Discord's blurple as a last resort.
                let (banner, _) =
                    ui.allocate_exact_size(Vec2::new(width, 100.0), Sense::hover());
                let accent = meta.accent_color.map(|rgb| {
                    Color32::from_rgb(
                        ((rgb >> 16) & 0xFF) as u8,
                        ((rgb >> 8) & 0xFF) as u8,
                        (rgb & 0xFF) as u8,
                    )
                });
                let banner_url = meta
                    .banner
                    .as_deref()
                    .map(|hash| util::user_banner_url(&user.id, hash));
                match banner_url.and_then(|url| app.images_large.get(ui.ctx(), &app.handle, &url))
                {
                    Some(texture) => {
                        Image::new((texture.id(), banner.size())).paint_at(ui, banner);
                    }
                    None => {
                        ui.painter()
                            .rect_filled(banner, 0.0, accent.unwrap_or(theme::BLURPLE));
                    }
                }
                // The avatar rides the banner's bottom edge, with a ring in
                // the sidebar color and the presence dot.
                let avatar = egui::Rect::from_min_size(
                    pos2(banner.left() + 16.0, banner.bottom() - 36.0),
                    Vec2::splat(72.0),
                );
                ui.painter()
                    .circle_filled(avatar.center(), avatar.width() / 2.0 + 3.0, theme::SIDEBAR);
                match app
                    .images
                    .get(ui.ctx(), &app.handle, &util::user_avatar_url(user))
                {
                    Some(texture) => {
                        Image::new((texture.id(), avatar.size()))
                            .corner_radius(egui::CornerRadius::same(36))
                            .paint_at(ui, avatar);
                    }
                    None => {
                        ui.painter().circle_filled(
                            avatar.center(),
                            avatar.width() / 2.0,
                            util::name_color(user.display_name()),
                        );
                        ui.painter().text(
                            avatar.center(),
                            Align2::CENTER_CENTER,
                            user.display_name()
                                .chars()
                                .next()
                                .map(|c| c.to_uppercase().to_string())
                                .unwrap_or_else(|| "?".into()),
                            FontId::proportional(28.0),
                            Color32::WHITE,
                        );
                    }
                }
                crate::ui::presence_dot(
                    ui,
                    avatar,
                    app.presence.get(&user.id).map(String::as_str),
                    app.presence_device.get(&user.id).map(String::as_str),
                    theme::SIDEBAR,
                );
                ui.add_space(40.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(user.display_name())
                            .family(theme::bold())
                            .size(20.0)
                            .color(theme::TEXT),
                    );
                    if let Some(clan) = user.clan_tag() {
                        crate::ui::clan_tag(app, ui, clan);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(format!("@{}", user.username))
                            .size(12.5)
                            .color(theme::MUTED),
                    );
                });
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);
                ui.add_space(6.0);
                ui.set_clip_rect(ui.max_rect());
                if me.as_deref() != Some(user.id.as_str()) && !friend {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Adicionar amigo").color(theme::WHITE),
                                )
                                .fill(theme::BLURPLE),
                            )
                            .clicked()
                        {
                            app.send(crate::backend::Command::RequestFriend {
                                username: user.username.clone(),
                            });
                        }
                    });
                    ui.add_space(6.0);
                }
                section(ui, "SOBRE MIM");
                match meta.bio.as_deref().filter(|bio| !bio.trim().is_empty()) {
                    Some(bio) => {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.add(
                                egui::Label::new(RichText::new(bio).size(13.0).color(theme::TEXT))
                                    .wrap(),
                            );
                        });
                    }
                    None if profile.is_some() => {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(
                                RichText::new("Sem sobre mim.")
                                    .size(13.0)
                                    .color(theme::MUTED),
                            );
                        });
                    }
                    None => {}
                }
                ui.add_space(10.0);
                section(ui, "MEMBRO DO DISCORD DESDE");
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    let joined = util::snowflake_ms(&user.id)
                        .map(util::account_created)
                        .unwrap_or_else(|| "—".into());
                    ui.label(RichText::new(joined).size(13.0).color(theme::TEXT));
                });
                let Some(profile) = profile else {
                    ui.add_space(10.0);
                    if !app.dm_profiles.contains_key(&user.id) {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(
                                RichText::new("Carregando perfil…")
                                    .size(12.0)
                                    .color(theme::MUTED),
                            );
                        });
                    }
                    return;
                };
                ui.add_space(10.0);
                if profile.mutual_friends_count.is_some_and(|count| count > 0) {
                    section(
                        ui,
                        &format!("AMIGOS EM COMUM — {}", profile.mutual_friends_count.unwrap()),
                    );
                }
                let shared: Vec<_> = profile
                    .mutual_guilds
                    .iter()
                    .filter_map(|mutual| {
                        app.guilds
                            .iter()
                            .find(|guild| guild.id == mutual.id)
                            .cloned()
                    })
                    .collect();
                if !shared.is_empty() {
                    section(
                        ui,
                        &format!("SERVIDORES EM COMUM — {}", shared.len()),
                    );
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for (index, guild) in shared.iter().enumerate().take(12) {
                            let texture = util::guild_icon_url(guild)
                                .and_then(|url| app.images.get(ui.ctx(), &app.handle, &url));
                            let icon = crate::ui::round_avatar(
                                ui,
                                texture.as_ref(),
                                28.0,
                                &guild.name,
                                theme::BLURPLE,
                            );
                            ui.interact(
                                icon,
                                ui.id().with(("mutual_guild", index)),
                                Sense::hover(),
                            )
                            .on_hover_text(&guild.name);
                        }
                    });
                }
                ui.add_space(12.0);
            });
            let _ = scroll;
        });
}

/// A small muted section header of the profile card.
fn section(ui: &mut egui::Ui, label: &str) {
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.label(
            RichText::new(label)
                .size(11.0)
                .family(theme::bold())
                .color(theme::MUTED),
        );
    });
    ui.add_space(3.0);
}
