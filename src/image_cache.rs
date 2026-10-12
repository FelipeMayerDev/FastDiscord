//! URL → egui texture cache. Avatars and guild icons are fetched off the UI
//! thread on a tokio worker and decoded with the `image` crate; the UI only
//! ever polls a plain channel. Animated GIFs and WebPs keep every frame and
//! `get` hands out the one due now.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use egui::TextureHandle;
use image::AnimationDecoder;
use tokio::runtime::Handle;

/// Frames kept per GIF. ponytail: long GIFs stop at this frame and loop;
/// stream frames instead if that ever shows.
const MAX_FRAMES: usize = 300;

type Frames = Vec<(egui::ColorImage, Duration)>;

struct Decoded {
    url: String,
    frames: Frames,
}

/// One texture per frame; a still image is a single frame.
struct Entry {
    frames: Vec<TextureHandle>,
    delays: Vec<Duration>,
}

pub struct ImageCache {
    textures: HashMap<String, Entry>,
    pending: HashSet<String>,
    rx: std::sync::mpsc::Receiver<Decoded>,
    tx: std::sync::mpsc::Sender<Decoded>,
    http: reqwest::Client,
    max_side: u32,
}

impl ImageCache {
    pub fn new(max_side: u32) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let http = reqwest::Client::builder()
            .user_agent(crate::backend::api::USER_AGENT)
            .build()
            .expect("failed to build the image HTTP client");
        Self {
            textures: HashMap::new(),
            pending: HashSet::new(),
            rx,
            tx,
            http,
            max_side,
        }
    }

    /// Returns the texture for `url` (the current frame of an animation),
    /// kicking off a background fetch on the first call. Call every frame;
    /// the returned `Option` turns into `Some` once the download and decode
    /// finish.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        handle: &Handle,
        url: &str,
    ) -> Option<TextureHandle> {
        while let Ok(decoded) = self.rx.try_recv() {
            self.pending.remove(&decoded.url);
            let (images, delays): (Vec<_>, Vec<_>) = decoded.frames.into_iter().unzip();
            let frames = images
                .into_iter()
                .enumerate()
                .map(|(index, image)| {
                    let name = format!("{}#{index}", decoded.url);
                    ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
                })
                .collect();
            self.textures.insert(decoded.url, Entry { frames, delays });
        }

        if !self.textures.contains_key(url) && !self.pending.contains(url) {
            self.pending.insert(url.to_string());
            let tx = self.tx.clone();
            let http = self.http.clone();
            let url = url.to_string();
            let max_side = self.max_side;
            handle.spawn(async move {
                if let Some(frames) = fetch(&http, &url, max_side).await {
                    let _ = tx.send(Decoded { url, frames });
                }
            });
            return None;
        }

        let entry = self.textures.get(url)?;
        if entry.frames.len() == 1
            || ctx.data(|data| {
                data.get_temp::<bool>(egui::Id::new("reduce_motion"))
                    .unwrap_or(false)
            })
        {
            return entry.frames.first().cloned();
        }
        let now = Duration::from_secs_f64(ctx.input(|input| input.time));
        let (index, wait) = frame_at(&entry.delays, now);
        ctx.request_repaint_after(wait);
        entry.frames.get(index).cloned()
    }
}

/// The frame showing at `now` in a looping animation, and how long until
/// the next one.
fn frame_at(delays: &[Duration], now: Duration) -> (usize, Duration) {
    let total: Duration = delays.iter().sum();
    if total.is_zero() {
        return (0, Duration::from_millis(100));
    }
    let mut left = Duration::from_nanos((now.as_nanos() % total.as_nanos()) as u64);
    for (index, delay) in delays.iter().enumerate() {
        if left < *delay {
            return (index, *delay - left);
        }
        left -= *delay;
    }
    (0, delays[0])
}

async fn fetch(http: &reqwest::Client, url: &str, max_side: u32) -> Option<Frames> {
    let bytes = http
        .get(url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .bytes()
        .await
        .ok()?;
    decode(&bytes, max_side)
}

/// Every frame of an animated GIF or WebP, or the single frame of anything
/// else, shrunk to fit `max_side` (never enlarged: `thumbnail` upscales, and
/// a 300-frame GIF blown up to 800 px is hundreds of MB).
fn decode(bytes: &[u8], max_side: u32) -> Option<Frames> {
    let to_color = |img: image::DynamicImage| {
        let img = if img.width().max(img.height()) > max_side {
            img.thumbnail(max_side, max_side)
        } else {
            img
        }
        .into_rgba8();
        let (width, height) = img.dimensions();
        egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &img.into_raw())
    };
    let cursor = std::io::Cursor::new(bytes);
    let animation = match image::guess_format(bytes).ok() {
        Some(image::ImageFormat::Gif) => image::codecs::gif::GifDecoder::new(cursor)
            .ok()
            .map(AnimationDecoder::into_frames),
        Some(image::ImageFormat::WebP) => image::codecs::webp::WebPDecoder::new(cursor)
            .ok()
            .filter(|decoder| decoder.has_animation())
            .map(AnimationDecoder::into_frames),
        _ => None,
    };
    if let Some(animation) = animation {
        let frames: Frames = animation
            .take(MAX_FRAMES)
            .map_while(Result::ok)
            .map(|frame| {
                let (numer, denom) = frame.delay().numer_denom_ms();
                let ms = numer / denom.max(1);
                // Browsers play near-zero delays at 100 ms; so do we.
                let ms = if ms <= 10 { 100 } else { ms };
                let image = to_color(image::DynamicImage::ImageRgba8(frame.into_buffer()));
                (image, Duration::from_millis(ms as u64))
            })
            .collect();
        if !frames.is_empty() {
            return Some(frames);
        }
    }
    let img = image::load_from_memory(bytes).ok()?;
    Some(vec![(to_color(img), Duration::ZERO)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_motion_keeps_gif_on_first_frame() {
        let ctx = egui::Context::default();
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("reduce_motion"), true));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let first = ctx.load_texture(
            "first",
            egui::ColorImage::new([1, 1], vec![egui::Color32::RED]),
            egui::TextureOptions::LINEAR,
        );
        let second = ctx.load_texture(
            "second",
            egui::ColorImage::new([1, 1], vec![egui::Color32::BLUE]),
            egui::TextureOptions::LINEAR,
        );
        let mut cache = ImageCache::new(64);
        cache.textures.insert(
            "cached".into(),
            Entry {
                frames: vec![first.clone(), second],
                delays: vec![Duration::from_millis(50); 2],
            },
        );
        assert_eq!(
            cache.get(&ctx, rt.handle(), "cached").unwrap().id(),
            first.id()
        );
    }

    #[test]
    fn gif_frames_decode_and_play_in_order() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for shade in [0u8, 255] {
                let buffer = image::RgbaImage::from_pixel(4, 4, image::Rgba([shade, 0, 0, 255]));
                let delay = image::Delay::from_numer_denom_ms(50, 1);
                encoder
                    .encode_frame(image::Frame::from_parts(buffer, 0, 0, delay))
                    .unwrap();
            }
        }
        let frames = decode(&bytes, 64).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].0.pixels[0].r(), 255);

        let delays: Vec<_> = frames.iter().map(|(_, delay)| *delay).collect();
        assert_eq!(delays, [Duration::from_millis(50); 2]);
        assert_eq!(
            frame_at(&delays, Duration::from_millis(10)),
            (0, Duration::from_millis(40))
        );
        assert_eq!(
            frame_at(&delays, Duration::from_millis(60)),
            (1, Duration::from_millis(40))
        );
        assert_eq!(
            frame_at(&delays, Duration::from_millis(110)),
            (0, Duration::from_millis(40))
        );
    }

    #[test]
    fn animated_webp_keeps_frames_and_is_never_enlarged() {
        let bytes = include_bytes!("../static/splash.webp"); // 128×128
        let frames = decode(bytes, 800).unwrap();
        assert!(frames.len() > 1);
        assert_eq!(frames[0].0.size, [128, 128]);
        assert_eq!(decode(bytes, 64).unwrap()[0].0.size, [64, 64]);
    }
}
