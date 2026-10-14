//! Shared UI helpers and the per-view modules.

pub mod channel_sidebar;
pub mod chat;
pub mod emoji;
pub mod friends;
pub mod icons;
pub mod image_viewer;
pub mod login;
pub mod members;
pub mod plugins;
pub mod profile_settings;
pub mod screen_share;
pub mod server_rail;
pub mod settings_window;
pub mod splash;
pub mod threads;
pub mod voice_extras;
pub mod voice_processing;

use egui::{Align2, Color32, CornerRadius, FontId, Image, Rect, Sense, TextureHandle, Vec2, pos2};

/// Reduced motion changes state immediately and schedules no animation frames.
pub fn animate_bool(ctx: &egui::Context, id: egui::Id, on: bool, seconds: f32) -> f32 {
    if ctx.data(|data| {
        data.get_temp::<bool>(egui::Id::new("reduce_motion"))
            .unwrap_or(false)
    }) {
        if on { 1.0 } else { 0.0 }
    } else {
        ctx.animate_bool_with_time(id, on, seconds)
    }
}

/// Our short hover fade: `from` → `to` over 100 ms while `on` holds.
pub fn fade(ui: &egui::Ui, id: egui::Id, on: bool, from: Color32, to: Color32) -> Color32 {
    let t = animate_bool(ui.ctx(), id.with("fade"), on, 0.1);
    from.lerp_to_gamma(to, t)
}

pub fn draw_texture(ui: &mut egui::Ui, texture: &TextureHandle, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().image(
        texture.id(),
        rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// A loaded texture clipped to a circle — Discord renders every avatar round.
pub fn draw_texture_round(ui: &mut egui::Ui, texture: &TextureHandle, size: f32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    Image::new((texture.id(), Vec2::splat(size)))
        .corner_radius(CornerRadius::same((size / 2.0) as u8))
        .paint_at(ui, rect);
    rect
}

/// Round avatar when loaded, letter circle otherwise.
pub fn round_avatar(
    ui: &mut egui::Ui,
    texture: Option<&TextureHandle>,
    size: f32,
    name: &str,
    color: Color32,
) -> Rect {
    match texture {
        Some(texture) => draw_texture_round(ui, texture, size),
        None => initial_circle(ui, size, name, color),
    }
}

/// A colored circle with the first letter — the fallback while an avatar or
/// icon hasn't loaded (or the account has none).
pub fn initial_circle(ui: &mut egui::Ui, size: f32, label: &str, color: Color32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
    let letter: String = label
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        letter,
        FontId::proportional(size * 0.5),
        Color32::WHITE,
    );
    rect
}

/// Presence defaults to offline until the gateway reports otherwise; the
/// dot becomes a tiny phone when the mobile client is the only one online.
pub fn presence_dot(
    ui: &egui::Ui,
    avatar: Rect,
    status: Option<&str>,
    device: Option<&str>,
    background: Color32,
) {
    let radius = (avatar.width() * 0.17).clamp(3.0, 6.0);
    let center = avatar.max - Vec2::splat(radius);
    let color = status_color(status.unwrap_or("offline"));
    ui.painter().circle_filled(center, radius + 1.5, background);
    if device == Some("mobile") {
        let size = Vec2::new(radius * 1.4, radius * 1.9);
        let phone = Rect::from_center_size(center, size);
        let painter = ui.painter();
        painter.rect_filled(phone, radius * 0.4, color);
        // The home indicator, in the ring's color so it reads on the phone.
        painter.line_segment(
            [
                pos2(center.x - size.x * 0.28, phone.bottom() - size.y * 0.14),
                pos2(center.x + size.x * 0.28, phone.bottom() - size.y * 0.14),
            ],
            egui::Stroke::new(1.0, background),
        );
    } else if device == Some("console") {
        // A gamepad: a wide body with round grips, buttons in the ring's
        // color.
        let painter = ui.painter();
        let body = Rect::from_center_size(center, Vec2::new(radius * 2.1, radius * 1.2));
        painter.rect_filled(body, radius * 0.5, color);
        for side in [-1.0, 1.0] {
            painter.circle_filled(
                center + Vec2::new(side * radius * 0.7, radius * 0.35),
                radius * 0.5,
                color,
            );
            painter.circle_filled(
                center + Vec2::new(side * radius * 0.55, -radius * 0.05),
                radius * 0.18,
                background,
            );
        }
    } else {
        ui.painter().circle_filled(center, radius, color);
    }
}

pub fn status_color(status: &str) -> Color32 {
    match status {
        "online" => crate::theme::GREEN,
        "idle" => crate::theme::YELLOW,
        "dnd" => crate::theme::RED,
        _ => crate::theme::MUTED,
    }
}

/// Discord's blue "NOVO" chip on channels and threads created after the
/// user joined the guild, until they visit them (issue #54).
pub fn new_chip(ui: &mut egui::Ui) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(" NOVO ")
                .size(9.5)
                .family(crate::theme::bold())
                .color(Color32::WHITE)
                .background_color(crate::theme::BLURPLE),
        )
        .selectable(false),
    );
}

/// Default group avatar, shared by the DM list and conversation header.
pub fn group_avatar(ui: &mut egui::Ui, size: f32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_group_avatar(ui.painter(), rect);
    rect
}

pub fn paint_group_avatar(painter: &egui::Painter, rect: Rect) {
    let c = rect.center();
    let s = rect.width();
    painter.circle_filled(c, s / 2.0, crate::theme::BLURPLE);
    for (x, y, radius) in [(-0.15, -0.12, 0.105), (0.15, -0.06, 0.12)] {
        let head = c + Vec2::new(s * x, s * y);
        painter.circle_filled(head, s * radius, Color32::WHITE);
        painter.rect_filled(
            Rect::from_center_size(
                head + Vec2::new(0.0, s * 0.20),
                Vec2::new(s * radius * 2.8, s * 0.20),
            ),
            s * 0.10,
            Color32::WHITE,
        );
    }
}

/// Smooth hover, selection and press feedback for custom painted controls.
pub fn control_bg(ui: &egui::Ui, response: &egui::Response, selected: bool) -> Color32 {
    let hover = fade(
        ui,
        response.id,
        response.hovered(),
        Color32::TRANSPARENT,
        crate::theme::HOVER,
    );
    let selection = animate_bool(ui.ctx(), response.id.with("selection"), selected, 0.12);
    let press = animate_bool(
        ui.ctx(),
        response.id.with("press"),
        response.is_pointer_button_down_on(),
        0.08,
    );
    hover
        .lerp_to_gamma(crate::theme::SELECTED, selection)
        .lerp_to_gamma(crate::theme::BLURPLE.gamma_multiply(0.35), press)
}

#[cfg(test)]
mod motion_tests {
    #[test]
    fn reduced_motion_uses_final_state_immediately() {
        let ctx = egui::Context::default();
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("reduce_motion"), true));
        let id = egui::Id::new("test_motion");
        assert_eq!(super::animate_bool(&ctx, id, true, 0.12), 1.0);
        assert_eq!(super::animate_bool(&ctx, id, false, 0.12), 0.0);
    }
}

#[cfg(test)]
mod popup_sizing_tests {
    use egui::{Rect, ScrollArea, Vec2, pos2};

    /// Issue #42: the pickers open while their content is still loading, and
    /// egui feeds a popup area's remembered size back as the available height
    /// of the ScrollArea inside it, so the popup used to stay stuck at the
    /// loading size even after the content arrived. The pickers pin
    /// `min_scrolled_height` to their `max_height` to break that loop; this
    /// test pins the egui behavior that makes the pattern work.
    #[test]
    fn popup_grows_to_full_size_when_its_content_arrives() {
        let ctx = egui::Context::default();
        let popup_id = egui::Id::new("picker_repro");
        let screen = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(1280.0, 800.0));
        let height = |ctx: &egui::Context| {
            ctx.memory(|memory| memory.area_rect(popup_id).map_or(0.0, |rect| rect.height()))
        };

        let mut open = true;
        let frame = |ctx: &egui::Context, loaded: bool, open: &mut bool| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |root| {
                    root.vertical(|ui| {
                        let button = ui.button("picker");
                        egui::Popup::from_response(&button)
                            .id(popup_id)
                            .open_bool(open)
                            .align(egui::RectAlign::TOP_START)
                            .gap(8.0)
                            .show(|ui| {
                                ui.set_width(440.0);
                                ui.label("Soundboard");
                                if !loaded {
                                    ui.label("Carregando…");
                                    return;
                                }
                                let mut query = String::new();
                                ui.text_edit_singleline(&mut query);
                                ScrollArea::vertical()
                                    .min_scrolled_height(420.0)
                                    .max_height(420.0)
                                    .show(ui, |ui| {
                                        for row in 0..40 {
                                            ui.label(format!("som {row}"));
                                        }
                                    });
                            });
                    });
                },
            )
            .textures_delta
            .clear();
        };

        // Loading frames are small; the content arriving must grow the popup
        // to its full height on that very frame, not only after a reopen.
        frame(&ctx, false, &mut open);
        assert!(height(&ctx) < 100.0, "loading: {}", height(&ctx));
        frame(&ctx, true, &mut open);
        assert!(height(&ctx) >= 460.0, "loaded: {}", height(&ctx));

        // Reopening with the content cached must also start full-sized.
        open = false;
        frame(&ctx, true, &mut open);
        open = true;
        frame(&ctx, true, &mut open);
        assert!(height(&ctx) >= 460.0, "reopened: {}", height(&ctx));
    }
}
