//! Shared UI helpers and the per-view modules.

pub mod channel_sidebar;
pub mod chat;
pub mod friends;
pub mod image_viewer;
pub mod login;
pub mod screen_share;
pub mod server_rail;
pub mod settings_window;
pub mod splash;
pub mod voice_extras;
pub mod voice_processing;

use egui::{Align2, Color32, CornerRadius, FontId, Image, Rect, Sense, TextureHandle, Vec2, pos2};

/// Discord's hover fade: `from` → `to` over 100 ms while `on` holds.
pub fn fade(ui: &egui::Ui, id: egui::Id, on: bool, from: Color32, to: Color32) -> Color32 {
    let t = ui.ctx().animate_bool_with_time(id.with("fade"), on, 0.1);
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

/// Presence defaults to offline until the gateway reports otherwise.
pub fn presence_dot(ui: &egui::Ui, avatar: Rect, status: Option<&str>, background: Color32) {
    let radius = (avatar.width() * 0.17).clamp(3.0, 6.0);
    let center = avatar.max - Vec2::splat(radius);
    let color = match status.unwrap_or("offline") {
        "online" => crate::theme::GREEN,
        "idle" => crate::theme::YELLOW,
        "dnd" => crate::theme::RED,
        _ => crate::theme::MUTED,
    };
    ui.painter().circle_filled(center, radius + 1.5, background);
    ui.painter().circle_filled(center, radius, color);
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
    let selection = ui
        .ctx()
        .animate_bool_with_time(response.id.with("selection"), selected, 0.12);
    let press = ui.ctx().animate_bool_with_time(
        response.id.with("press"),
        response.is_pointer_button_down_on(),
        0.08,
    );
    hover
        .lerp_to_gamma(crate::theme::SELECTED, selection)
        .lerp_to_gamma(crate::theme::BLURPLE.gamma_multiply(0.35), press)
}
