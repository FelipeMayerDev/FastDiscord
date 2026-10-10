//! Image viewer: a chat image opened in its own window, with zoom, save to
//! Downloads and open in the browser.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::{Image, RichText, ScrollArea, Vec2};
use tokio::runtime::Handle;

use crate::app::VesktopApp;
use crate::image_cache::ImageCache;
use crate::theme;

pub struct ImageViewer {
    url: String,
    /// `None` fits the image to the window.
    zoom: Option<f32>,
    /// Full-resolution textures, separate from the chat's 800 px cache.
    images: ImageCache,
    /// Last save result, written by the download task.
    status: Arc<Mutex<Option<String>>>,
}

impl ImageViewer {
    pub fn new(url: String) -> Self {
        Self {
            url,
            zoom: None,
            images: ImageCache::new(4096),
            status: Arc::default(),
        }
    }
}

pub fn show(app: &mut VesktopApp, ctx: &egui::Context) {
    let Some(viewer) = app.viewer.as_mut() else {
        return;
    };
    let handle = app.handle.clone();
    let mut close = false;
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("image_viewer"),
        egui::ViewportBuilder::default()
            .with_title("Imagem — FastDiscord")
            .with_inner_size([960.0, 720.0]),
        |ui, _| {
            close = ui.input(|input| {
                input.viewport().close_requested() || input.key_pressed(egui::Key::Escape)
            });
            paint(viewer, &handle, ui);
        },
    );
    if close {
        app.viewer = None;
    }
}

fn paint(viewer: &mut ImageViewer, handle: &Handle, root: &mut egui::Ui) {
    let ctx = root.ctx().clone();
    let texture = viewer.images.get(&ctx, handle, &viewer.url);

    egui::Panel::top("image_viewer_bar").show(root, |ui| {
        ui.horizontal(|ui| {
            let zoom = viewer.zoom.unwrap_or(1.0);
            if ui.button("−").on_hover_text("Diminuir").clicked() {
                viewer.zoom = Some((zoom / 1.25).max(0.05));
            }
            let label = match viewer.zoom {
                Some(zoom) => format!("{:.0}%", zoom * 100.0),
                None => "Ajustado".to_string(),
            };
            ui.label(RichText::new(label).color(theme::MUTED));
            if ui.button("+").on_hover_text("Aumentar").clicked() {
                viewer.zoom = Some((zoom * 1.25).min(16.0));
            }
            if ui.button("Ajustar").clicked() {
                viewer.zoom = None;
            }
            if ui.button("100%").clicked() {
                viewer.zoom = Some(1.0);
            }
            ui.separator();
            if ui.button("💾 Salvar").clicked() {
                save(viewer, handle, &ctx);
            }
            if ui.button("🌐 Abrir no navegador").clicked() {
                ctx.open_url(egui::OpenUrl::new_tab(&viewer.url));
            }
            if let Some(status) = viewer.status.lock().unwrap().as_deref() {
                ui.label(RichText::new(status).small().color(theme::MUTED));
            }
        });
    });

    egui::CentralPanel::default().show(root, |ui| {
        let Some(texture) = texture else {
            ui.centered_and_justified(|ui| ui.spinner());
            return;
        };
        let size = texture.size_vec2();
        let avail = ui.available_size();
        let fit = (avail.x / size.x).min(avail.y / size.y).min(1.0);
        let mut zoom = viewer.zoom.unwrap_or(fit);
        // Ctrl+scroll and pinch zoom; plain scroll pans.
        let delta = ui.input(|input| input.zoom_delta());
        if delta != 1.0 {
            zoom = (zoom * delta).clamp(0.05, 16.0);
            viewer.zoom = Some(zoom);
        }
        let shown = size * zoom;
        let pad = ((avail - shown) / 2.0).max(Vec2::ZERO);
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.add_space(pad.y);
            ui.horizontal(|ui| {
                ui.add_space(pad.x);
                let response = ui.add(Image::new((texture.id(), shown)).sense(egui::Sense::click()));
                if response.double_clicked() {
                    viewer.zoom = if viewer.zoom.is_some() { None } else { Some(1.0) };
                }
            });
        });
    });
}

fn save(viewer: &ImageViewer, handle: &Handle, ctx: &egui::Context) {
    let url = viewer.url.clone();
    let status = viewer.status.clone();
    let ctx = ctx.clone();
    *status.lock().unwrap() = Some("Salvando…".to_string());
    handle.spawn(async move {
        let message = match download(&url).await {
            Ok(path) => format!("Salvo em {}", path.display()),
            Err(error) => format!("Falha ao salvar: {error}"),
        };
        *status.lock().unwrap() = Some(message);
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    });
}

async fn download(url: &str) -> anyhow::Result<PathBuf> {
    let bytes = reqwest::Client::builder()
        .user_agent(crate::backend::api::USER_AGENT)
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let dirs = directories::UserDirs::new();
    let dir = dirs
        .as_ref()
        .and_then(|dirs| dirs.download_dir().or(Some(dirs.home_dir())))
        .ok_or_else(|| anyhow::anyhow!("pasta de downloads desconhecida"))?;
    let path = free_path(dir, file_name(url));
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

/// The URL's last path segment, without the query.
fn file_name(url: &str) -> &str {
    url.split('?')
        .next()
        .and_then(|path| path.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("imagem")
}

/// `dir/name`, or `dir/stem (n).ext` when that is taken.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (name, String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or(path)
}
