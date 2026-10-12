//! Screen share views (docs/SCREENSHARE.md): the picker opened by the
//! share button, our own capture and webcam as 16:9 previews above the
//! "Voz conectada" block, and a watched live (FockyTV or Go Live) in place
//! of the chat.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Rect, RichText, Vec2, pos2};

use crate::app::{VesktopApp, VoiceConn};
use crate::backend::capture::{self, Capture, Frame, Source};
use crate::theme;

/// Margin around the preview boxes inside the sidebar.
const MARGIN: f32 = 8.0;

/// How many previews the sidebar shows: the screen, the camera or both.
fn previews(app: &VesktopApp) -> usize {
    usize::from(app.screen.is_some()) + usize::from(app.camera.is_some())
}

/// Height the previews take in the sidebar's bottom block (0 when idle);
/// two share one row.
pub fn height(app: &VesktopApp, width: f32) -> f32 {
    let count = previews(app) as f32;
    if count == 0.0 {
        return 0.0;
    }
    (width - (count + 1.0) * MARGIN) / count * 9.0 / 16.0 + MARGIN
}

/// Once per frame: drops an ended capture or camera and uploads their
/// newest frames.
pub fn poll(app: &mut VesktopApp, ctx: &egui::Context) {
    // The source went away, the portal was cancelled or FockyTV dropped us.
    if let Some(capture) = &app.screen
        && let Some(reason) = ended(capture, app.fockytv.as_ref())
    {
        log::info!("compartilhamento encerrado: {reason}");
        app.stop_screen_share();
    }
    if let Some(camera) = &app.camera
        && let Some(reason) = ended(camera, app.camera_publisher.as_ref())
    {
        log::info!("câmera encerrada: {reason}");
        app.stop_camera();
    }
    upload(
        ctx,
        &mut app.screen,
        &mut app.screen_texture,
        "screen_share",
    );
    upload(ctx, &mut app.camera, &mut app.camera_texture, "camera");
}

fn ended(
    capture: &Capture,
    publisher: Option<&crate::backend::fockytv::Publisher>,
) -> Option<String> {
    capture
        .ended
        .lock()
        .unwrap()
        .take()
        .or_else(|| publisher.and_then(|publisher| publisher.ended.lock().unwrap().take()))
}

fn upload(
    ctx: &egui::Context,
    capture: &mut Option<Capture>,
    texture: &mut Option<egui::TextureHandle>,
    name: &str,
) {
    let Some(capture) = capture else {
        return;
    };
    let Some(frame) = capture.frame.lock().unwrap().clone() else {
        return;
    };
    if frame.seq == capture.preview_seq {
        return;
    }
    capture.preview_seq = frame.seq;
    // The sidebar box is ~224 px wide; 640 keeps it sharp on HiDPI.
    let (size, rgba) = frame.preview_rgba(640);
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
    match texture {
        Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
        None => *texture = Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR)),
    }
}

/// The preview boxes, letterboxed, with "AO VIVO" / "CÂMERA" badges.
pub fn paint(app: &VesktopApp, ui: &mut egui::Ui) {
    let width = ui.available_width();
    let h = height(app, width);
    if h == 0.0 {
        return;
    }
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, h), egui::Sense::hover());
    let boxes = [
        (
            app.screen.is_some(),
            &app.screen_texture,
            "AO VIVO",
            theme::RED,
            "Escolha a tela…",
        ),
        (
            app.camera.is_some(),
            &app.camera_texture,
            "CÂMERA",
            theme::BLURPLE,
            "Ligando a câmera…",
        ),
    ];
    let count = previews(app) as f32;
    let box_width = (width - (count + 1.0) * MARGIN) / count;
    let active = boxes.into_iter().filter(|(on, ..)| *on);
    for (index, (_, texture, badge_text, badge_color, waiting)) in active.enumerate() {
        let left = row.left() + MARGIN + index as f32 * (box_width + MARGIN);
        let frame = Rect::from_min_max(
            pos2(left, row.top() + MARGIN),
            pos2(left + box_width, row.bottom()),
        );
        let painter = ui.painter();
        painter.rect_filled(frame, 6.0, egui::Color32::BLACK);
        match texture {
            Some(texture) => {
                let size = texture.size_vec2();
                let scale = (frame.width() / size.x).min(frame.height() / size.y);
                let fitted = Rect::from_center_size(frame.center(), size * scale);
                egui::Image::new((texture.id(), fitted.size()))
                    .corner_radius(4)
                    .paint_at(ui, fitted);
            }
            None => {
                painter.text(
                    frame.center(),
                    egui::Align2::CENTER_CENTER,
                    waiting,
                    egui::FontId::proportional(12.0),
                    theme::MUTED,
                );
            }
        }
        let badge = Rect::from_min_size(frame.min + Vec2::splat(6.0), Vec2::new(54.0, 18.0));
        painter.rect_filled(badge, 4.0, badge_color);
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            badge_text,
            egui::FontId::proportional(10.5),
            egui::Color32::WHITE,
        );
    }
}

/// The share picker, Discord-style: on Windows "Telas" / "Janelas" tabs
/// with a still of each source; on Linux (Wayland) only the options, since
/// the ScreenCast portal's dialog picks the source after "Compartilhar".
pub(crate) struct Picker {
    sources: Vec<(Source, String)>,
    windows_tab: bool,
    selected: Option<Source>,
    /// Filled by a background thread, one still per source.
    stills: Arc<Mutex<HashMap<Source, Frame>>>,
    textures: HashMap<Source, egui::TextureHandle>,
}

impl Picker {
    pub(crate) fn new() -> Self {
        let sources = capture::sources();
        let stills: Arc<Mutex<HashMap<Source, Frame>>> = Arc::default();
        if !sources.is_empty() {
            let pending: Vec<Source> = sources.iter().map(|(source, _)| *source).collect();
            let weak = Arc::downgrade(&stills);
            let _ = std::thread::Builder::new()
                .name("fastdiscord-thumbs".into())
                .spawn(move || {
                    for source in pending {
                        // The picker closed: stop grabbing.
                        if weak.strong_count() == 0 {
                            return;
                        }
                        if let (Some(frame), Some(stills)) =
                            (capture::thumbnail(source), weak.upgrade())
                        {
                            stills.lock().unwrap().insert(source, frame);
                        }
                    }
                });
        }
        Self {
            selected: sources.first().map(|(source, _)| *source),
            sources,
            windows_tab: false,
            stills,
            textures: HashMap::new(),
        }
    }
}

/// Size of a source card in the picker.
const CARD: Vec2 = Vec2::new(164.0, 122.0);

/// Shows the open picker; "Compartilhar" starts the capture, "Cancelar",
/// Escape or a click outside close it with nothing started.
pub fn picker(app: &mut VesktopApp, ctx: &egui::Context) {
    if app.share_picker.is_none() {
        return;
    }
    if !matches!(app.voice, VoiceConn::Connected { .. }) {
        app.share_picker = None;
        return;
    }
    let (Some(picker), settings) = (app.share_picker.as_mut(), &mut app.settings) else {
        return;
    };
    for (source, frame) in picker.stills.lock().unwrap().drain() {
        let (size, rgba) = frame.preview_rgba(frame.width);
        let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
        let texture = ctx.load_texture(
            format!("share_still_{source:?}"),
            image,
            egui::TextureOptions::LINEAR,
        );
        picker.textures.insert(source, texture);
    }
    if picker.textures.len() < picker.sources.len() {
        ctx.request_repaint_after(Duration::from_millis(250));
    }
    let (mut confirm, mut cancel, mut changed) = (false, false, false);
    let response = egui::Modal::new(egui::Id::new("share_picker"))
        .backdrop_color(egui::Color32::from_black_alpha(160))
        .frame(
            egui::Frame::popup(&ctx.global_style())
                .inner_margin(20)
                .corner_radius(12),
        )
        .show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 80.0).clamp(240.0, 3.0 * CARD.x + 40.0));
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            ui.heading("Compartilhar tela");
            if picker.sources.is_empty() {
                ui.label(
                    "A tela ou janela é escolhida no diálogo do sistema, \
                     que abre ao clicar em Compartilhar.",
                );
            } else {
                ui.horizontal(|ui| {
                    for (windows, label) in [(false, "Telas"), (true, "Janelas")] {
                        if ui
                            .selectable_label(picker.windows_tab == windows, label)
                            .clicked()
                            && picker.windows_tab != windows
                        {
                            picker.windows_tab = windows;
                            picker.selected = picker
                                .sources
                                .iter()
                                .map(|(source, _)| *source)
                                .find(|source| matches!(source, Source::Window(_)) == windows);
                        }
                    }
                });
                egui::ScrollArea::vertical()
                    .max_height(2.0 * CARD.y + 16.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            let tab = picker.sources.iter().filter(|(source, _)| {
                                matches!(source, Source::Window(_)) == picker.windows_tab
                            });
                            for (source, name) in tab {
                                let card = source_card(
                                    ui,
                                    name,
                                    picker.textures.get(source),
                                    picker.selected == Some(*source),
                                );
                                if card.clicked() {
                                    picker.selected = Some(*source);
                                }
                                if card.double_clicked() {
                                    confirm = true;
                                }
                            }
                        });
                    });
            }
            ui.separator();
            ui.add_enabled_ui(settings.fockytv_share, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Resolução:");
                    for (height, label) in [(0, "Original"), (1080, "1080p"), (720, "720p")] {
                        if ui
                            .radio(settings.fockytv_max_height == height, label)
                            .clicked()
                        {
                            settings.fockytv_max_height = height;
                            changed = true;
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Taxa de quadros:");
                    for fps in [60, 30, 15] {
                        if ui
                            .radio(settings.fockytv_fps == fps, format!("{fps} fps"))
                            .clicked()
                        {
                            settings.fockytv_fps = fps;
                            changed = true;
                        }
                    }
                });
            });
            if !settings.fockytv_share {
                ui.label(
                    RichText::new("O Go Live do Discord envia 720p a 30 fps.")
                        .small()
                        .color(theme::MUTED),
                );
            }
            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = picker.sources.is_empty() || picker.selected.is_some();
                confirm |= ui
                    .add_enabled(
                        ready,
                        egui::Button::new("Compartilhar").fill(theme::BLURPLE),
                    )
                    .clicked();
                cancel = ui.button("Cancelar").clicked();
            });
        });
    let source = picker.selected;
    if changed {
        app.settings.save();
    }
    if confirm {
        app.share_picker = None;
        app.start_screen_share(ctx, source);
    } else if cancel || response.should_close() {
        app.share_picker = None;
    }
}

/// One source in the picker: its still (or a placeholder) and its name.
fn source_card(
    ui: &mut egui::Ui,
    name: &str,
    still: Option<&egui::TextureHandle>,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(CARD, egui::Sense::click());
    let painter = ui.painter();
    let fill = if selected {
        theme::SELECTED
    } else if response.hovered() {
        theme::HOVER
    } else {
        egui::Color32::TRANSPARENT
    };
    painter.rect_filled(rect, theme::RADIUS_SM, fill);
    if selected {
        painter.rect_stroke(
            rect,
            theme::RADIUS_SM,
            egui::Stroke::new(2.0, theme::BLURPLE),
            egui::StrokeKind::Inside,
        );
    }
    let thumb = Rect::from_min_size(rect.min + Vec2::splat(6.0), Vec2::new(CARD.x - 12.0, 86.0));
    painter.rect_filled(thumb, 4.0, egui::Color32::BLACK);
    match still {
        Some(texture) => {
            let size = texture.size_vec2();
            let scale = (thumb.width() / size.x).min(thumb.height() / size.y);
            egui::Image::new((texture.id(), size * scale))
                .paint_at(ui, Rect::from_center_size(thumb.center(), size * scale));
        }
        None => {
            painter.text(
                thumb.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                egui::FontId::proportional(14.0),
                theme::MUTED,
            );
        }
    }
    let label = Rect::from_min_max(pos2(rect.left() + 6.0, thumb.bottom() + 4.0), rect.max);
    ui.new_child(egui::UiBuilder::new().max_rect(label)).add(
        egui::Label::new(RichText::new(name).size(12.0).color(theme::TEXT))
            .truncate()
            .selectable(false),
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(name)
}

/// Once per frame: closes an ended FockyTV live and uploads the watched
/// stream's newest frame, docked or floating.
pub fn poll_watch(app: &mut VesktopApp, ctx: &egui::Context) {
    let Some(watch) = &app.watch else {
        return;
    };
    // A FockyTV live that ended (or never started) closes the view.
    let ended = watch
        .fockytv
        .as_ref()
        .and_then(|viewer| viewer.ended.lock().unwrap().take());
    if let Some(reason) = ended {
        log::info!("fockytv: {reason}");
        app.stop_watching();
        return;
    }
    let Some(frame) = watch.frames.lock().unwrap().take() else {
        return;
    };
    // Decoded frames are already RGBA; only captures need swizzling.
    let image = if frame.bgr {
        let (size, rgba) = frame.preview_rgba(frame.width);
        egui::ColorImage::from_rgba_unmultiplied(size, &rgba)
    } else {
        egui::ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.pixels,
        )
    };
    match &mut app.watch_texture {
        Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
        None => {
            app.watch_texture =
                Some(ctx.load_texture("watch_stream", image, egui::TextureOptions::LINEAR));
        }
    }
}

/// The video letterboxed into `area`, or a placeholder until it arrives.
fn paint_video(app: &VesktopApp, ui: &mut egui::Ui, area: Rect) {
    ui.painter().rect_filled(area, 6.0, egui::Color32::BLACK);
    match &app.watch_texture {
        Some(texture) => {
            let size = texture.size_vec2();
            let scale = (area.width() / size.x).min(area.height() / size.y);
            egui::Image::new((texture.id(), size * scale))
                .paint_at(ui, Rect::from_center_size(area.center(), size * scale));
        }
        None => {
            // Discord cuts Go Live video for Brazil (ANPD, Aug 2026) after
            // a few seconds; FockyTV isn't affected.
            let blocked = app.watch.as_ref().is_some_and(|watch| {
                watch.fockytv.is_none() && watch.started.elapsed() > Duration::from_secs(10)
            });
            let text = if blocked {
                "Sem vídeo: o Discord suspendeu o Go Live no Brasil.\nUse o FockyTV para assistir."
            } else {
                ui.ctx().request_repaint_after(Duration::from_secs(1));
                "Conectando à transmissão…"
            };
            ui.painter().text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(14.0),
                theme::MUTED,
            );
        }
    }
}

/// A watched stream in place of the chat (its voice channel's view): the
/// video, letterboxed, with a bar to leave it.
pub fn paint_watch(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let Some(watch) = &app.watch else {
        return;
    };
    let title = match watch.key.strip_prefix("fockytv:") {
        Some(key) => match key.strip_suffix("-cam") {
            Some(nick) => format!("Câmera de {nick} no FockyTV"),
            None => format!("Assistindo {key} no FockyTV"),
        },
        None => "Assistindo à transmissão".to_string(),
    };
    let mut leave = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).strong().color(theme::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            leave = ui.button("Parar de assistir").clicked();
        });
    });
    let area = ui.available_rect_before_wrap();
    paint_video(app, ui, area);
    if leave {
        app.stop_watching();
    }
}

/// Floating size of a live watched from another view.
const FLOATING: Vec2 = Vec2::new(320.0, 180.0);

/// The watched stream in a corner while another chat is open; a click
/// returns to its voice channel, where it docks again.
pub fn paint_floating(app: &mut VesktopApp, ctx: &egui::Context) {
    let response = egui::Area::new(egui::Id::new("watch_floating"))
        .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::splat(-16.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let (area, response) = ui.allocate_exact_size(FLOATING, egui::Sense::click());
            paint_video(app, ui, area);
            if response.hovered() {
                ui.painter().rect_stroke(
                    area,
                    6.0,
                    egui::Stroke::new(2.0, theme::TEXT),
                    egui::StrokeKind::Inside,
                );
            }
            response.on_hover_text("Voltar para a transmissão")
        })
        .inner;
    if response.clicked() {
        app.dock_watch();
    }
}
