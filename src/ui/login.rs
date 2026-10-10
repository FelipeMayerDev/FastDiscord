//! Login screen. A native client can't render Discord's email/password page
//! (it's a browser page with captcha), so accounts are linked by scanning a
//! QR code with the phone app, or by pasting a token as an advanced option.
//! Before login the window shrinks to a small centered card holding only the
//! QR code and the token field (docs/UI.md §2); `compact_window` restores
//! the normal size once the account is in.

use egui::{RichText, Sense, Vec2};

use crate::app::{ConnState, QrState, VesktopApp};
use crate::theme;
use crate::ui::round_avatar;
use crate::util;

const QR: f32 = 180.0;
/// The pre-login window, and the main window's minimum (main.rs).
const LOGIN_SIZE: Vec2 = Vec2::new(400.0, 600.0);
const MAIN_MIN: Vec2 = Vec2::new(940.0, 600.0);
const MAIN_DEFAULT: Vec2 = Vec2::new(1280.0, 800.0);

/// Shrinks the window to the login card (`compact`), or puts back the size
/// it had before. Only acts on a change, so it's cheap to call every frame.
pub fn compact_window(ctx: &egui::Context, compact: bool) {
    let id = egui::Id::new("login_compact_restore");
    let restore: Option<Vec2> = ctx.data(|data| data.get_temp(id));
    if compact == restore.is_some() {
        return;
    }
    let size = if compact {
        // A window last closed on the login screen reopens small: restore
        // to the default size then, not to the card.
        let current = ctx.input(|input| input.viewport().inner_rect.map(|rect| rect.size()));
        let back = current
            .filter(|size| size.x >= MAIN_MIN.x && size.y >= MAIN_MIN.y)
            .unwrap_or(MAIN_DEFAULT);
        ctx.data_mut(|data| data.insert_temp(id, back));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(LOGIN_SIZE));
        LOGIN_SIZE
    } else {
        ctx.data_mut(|data| data.remove::<Vec2>(id));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(MAIN_MIN));
        restore.unwrap_or(MAIN_DEFAULT)
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
    // Centered on the monitor (compositors that place windows ignore this).
    if let Some(monitor) = ctx.input(|input| input.viewport().monitor_size) {
        let pos = ((monitor - size) / 2.0).max(Vec2::ZERO);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos.to_pos2()));
    }
}

pub fn show(app: &mut VesktopApp, root: &mut egui::Ui) {
    if app.settings.token.is_none() && matches!(app.qr, QrState::Idle) {
        app.start_qr();
    }
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(theme::CHAT).inner_margin(24.0))
        .show(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Entrar com código QR")
                            .size(20.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.label(
                        RichText::new("Escaneie com o app do Discord no celular")
                            .small()
                            .color(theme::MUTED),
                    );
                    ui.add_space(12.0);
                    qr_section(app, ui);
                    ui.add_space(20.0);
                    token_form(app, ui);
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(
                            "Clientes de terceiros podem violar os Termos de Serviço do \
                             Discord. Use por sua conta e risco.",
                        )
                        .small()
                        .color(theme::MUTED),
                    );
                });
            });
        });
}

fn token_form(app: &mut VesktopApp, ui: &mut egui::Ui) {
    if let ConnState::LoginError(message) = &app.conn {
        ui.label(RichText::new(message).small().color(theme::RED));
        ui.add_space(8.0);
    }
    let edit = egui::TextEdit::singleline(&mut app.login_token)
        .password(true)
        .hint_text("Ou cole o token do Discord")
        .desired_width(ui.available_width())
        .show(ui);
    ui.add_space(8.0);
    let pressed_enter =
        edit.response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    let clicked = ui
        .add_sized(
            [ui.available_width(), 36.0],
            egui::Button::new(RichText::new("Entrar").color(theme::WHITE)).fill(theme::BLURPLE),
        )
        .clicked();
    if clicked || pressed_enter {
        app.connect_from_login();
    }
    ui.add_space(4.0);
    ui.label(RichText::new("Como obter o token?").small().color(theme::MUTED))
        .on_hover_text(
            "1. Abra discord.com no navegador e entre na sua conta;\n\
             2. Aperte Ctrl+Shift+I para abrir o DevTools;\n\
             3. Na aba Console, rode: localStorage.token\n\
             4. Copie o valor entre aspas e cole acima.",
        );
}

fn qr_section(app: &mut VesktopApp, ui: &mut egui::Ui) {
    match app.qr.clone() {
        QrState::Idle | QrState::Loading => {
            ui.add_sized([QR, QR], egui::Spinner::new().size(40.0));
        }
        QrState::Code(url) => {
            paint_qr(ui, &url);
        }
        QrState::Scanned {
            username,
            user_id,
            avatar,
        } => {
            ui.add_sized([QR, QR], egui::Spinner::new().size(40.0));
            ui.add_space(8.0);
            // Avatar and name straight from the pending ticket.
            let texture = avatar.as_deref().and_then(|hash| {
                let url =
                    format!("https://cdn.discordapp.com/avatars/{user_id}/{hash}.png?size=64");
                app.images.get(ui.ctx(), &app.handle, &url)
            });
            round_avatar(
                ui,
                texture.as_ref(),
                40.0,
                &username,
                util::name_color(&username),
            );
            ui.add_space(4.0);
            ui.label(RichText::new("Confirme no seu celular").color(theme::TEXT));
            ui.label(RichText::new(&username).strong().color(theme::TEXT));
            ui.add_space(4.0);
            if ui
                .add(egui::Button::new(
                    RichText::new("Começar de novo").color(theme::BLURPLE),
                ))
                .clicked()
            {
                app.start_qr();
            }
        }
        QrState::Failed(reason) => {
            ui.label(RichText::new(reason).small().color(theme::RED));
            ui.add_space(8.0);
            if ui.button("Gerar novo QR code").clicked() {
                app.start_qr();
            }
        }
    }
}

/// Paints the code as plain rects, with the 4-module quiet zone scanners
/// expect, and FastDiscord's logo in the middle. `EcLevel::H` leaves enough
/// redundancy for the covered modules to still scan.
fn paint_qr(ui: &mut egui::Ui, data: &str) {
    let Ok(code) = qrcode::QrCode::with_error_correction_level(data.as_bytes(), qrcode::EcLevel::H)
    else {
        return;
    };
    let width = code.width();
    let cell = QR / (width + 8) as f32;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(QR), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, theme::RADIUS_MD, theme::WHITE);
    for (i, color) in code.to_colors().into_iter().enumerate() {
        if color == qrcode::Color::Dark {
            let x = (i % width + 4) as f32 * cell;
            let y = (i / width + 4) as f32 * cell;
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min + egui::vec2(x, y), egui::vec2(cell, cell)),
                0.0,
                egui::Color32::BLACK,
            );
        }
    }
    // FastDiscord's logo, on a white plate so the modules stay light there.
    let plate = QR * 0.22;
    let icon = egui::Rect::from_center_size(rect.center(), Vec2::splat(plate));
    painter.rect_filled(icon, 6.0, theme::WHITE);
    egui::Image::from_bytes("bytes://fastdiscord-logo.png", LOGO_PNG).paint_at(ui, icon.shrink(3.0));
}

pub(crate) const LOGO_PNG: &[u8] = include_bytes!("../../assets/tray.png");
