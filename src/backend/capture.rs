//! Screen capture for both share backends (docs/SCREENSHARE.md): a
//! latest-frame slot that the preview, the Go Live encoder and the WHIP
//! feeder read at their own pace. The capture itself is per platform:
//! `linux.rs` (ScreenCast portal + PipeWire), `windows.rs` (GStreamer's
//! Desktop Duplication / Windows Graphics Capture source). The webcam
//! (`backend::camera`) fills the same kind of slot through `start_gst`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

/// One captured frame: tightly packed 32-bit pixels, BGRx or RGBx order.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// Blue first (BGRx/BGRA, the usual XRGB8888 desktop format).
    pub bgr: bool,
    /// Bumps on every new frame so readers can skip ones they've seen.
    pub seq: u64,
}

impl Frame {
    /// Opaque RGBA, nearest-neighbour downscaled by an integer step so it
    /// is at most `max_width` wide: cheap enough for a per-frame preview.
    pub fn preview_rgba(&self, max_width: u32) -> ([usize; 2], Vec<u8>) {
        let step = self.width.div_ceil(max_width.max(1)).max(1) as usize;
        let (width, height) = (self.width as usize, self.height as usize);
        let (out_w, out_h) = (width.div_ceil(step), height.div_ceil(step));
        let mut out = Vec::with_capacity(out_w * out_h * 4);
        for y in (0..height).step_by(step) {
            let row = &self.pixels[y * width * 4..(y + 1) * width * 4];
            for px in row.as_chunks::<4>().0.iter().step_by(step) {
                if self.bgr {
                    out.extend_from_slice(&[px[2], px[1], px[0], 255]);
                } else {
                    out.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
        }
        ([out_w, out_h], out)
    }
}

/// The latest-frame slot.
pub type Shared = Arc<Mutex<Option<Arc<Frame>>>>;

/// A running capture. Dropping it stops the capture (and, on Linux, closes
/// the portal session).
pub struct Capture {
    /// Latest frame, shared: the preview and the encoder both read it and
    /// tell new frames apart by `seq`.
    pub frame: Shared,
    /// `seq` of the frame the preview last uploaded.
    pub preview_seq: u64,
    /// Set by the capture thread when the stream ends or fails.
    pub ended: Arc<Mutex<Option<String>>>,
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

/// A screen or window from `sources()`, chosen in the app's picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// An `HMONITOR`.
    Monitor(u64),
    /// An `HWND`, captured through Windows Graphics Capture.
    Window(u64),
}

/// Screens and windows for the picker, primary screen first. Only Windows
/// lists them: on Linux (Wayland) apps can't see other windows, and the
/// ScreenCast portal's own dialog is the picker.
pub fn sources() -> Vec<(Source, String)> {
    #[cfg(windows)]
    return platform::sources();
    #[cfg(not(windows))]
    Vec::new()
}

/// A small still of `source` for the picker (Windows only).
pub fn thumbnail(source: Source) -> Option<Frame> {
    #[cfg(windows)]
    return platform::thumbnail(source);
    #[cfg(not(windows))]
    {
        let _ = source;
        None
    }
}

/// Starts streaming `source`: Linux ignores it and opens the portal dialog;
/// Windows without one takes the primary monitor. `on_frame` runs on the
/// capture thread after each new frame (the UI passes a repaint request).
/// `runtime` must outlive every capture: on Linux ashpd caches its D-Bus
/// connection in a static, bound to the runtime that first opened it — a
/// per-capture runtime left every later picker hanging forever.
pub fn start(
    runtime: tokio::runtime::Handle,
    source: Option<Source>,
    on_frame: impl Fn() + Send + 'static,
) -> Capture {
    #[cfg(windows)]
    {
        let _ = runtime;
        start_gst(move || platform::pipeline(source), on_frame)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = source;
        let frame = Arc::new(Mutex::new(None));
        let ended = Arc::new(Mutex::new(None));
        let stop = platform::start(runtime, Arc::clone(&frame), Arc::clone(&ended), on_frame);
        Capture {
            frame,
            preview_seq: 0,
            ended,
            stop: Some(stop),
        }
    }
}

/// Runs the pipeline `build` makes on its own thread, feeding its BGRx
/// `appsink name=frames` into a new slot (the Windows screen, the webcam).
pub(crate) fn start_gst(
    build: impl FnOnce() -> Result<gst::Pipeline, String> + Send + 'static,
    on_frame: impl Fn() + Send + 'static,
) -> Capture {
    let frame: Shared = Arc::default();
    let ended = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let (slot, thread_ended, thread_stop) =
        (Arc::clone(&frame), Arc::clone(&ended), Arc::clone(&stop));
    let spawned = std::thread::Builder::new()
        .name("fastdiscord-capture".into())
        .spawn(move || {
            let result = gst::init()
                .map_err(|err| format!("gstreamer: {err}"))
                .and_then(|()| build())
                .and_then(|pipeline| run_gst(&pipeline, &slot, &thread_stop, on_frame));
            let reason = match result {
                Ok(()) => "captura encerrada".to_string(),
                Err(err) => {
                    log::warn!("captura: {err}");
                    err
                }
            };
            *thread_ended.lock().unwrap() = Some(reason);
        });
    if let Err(err) = spawned {
        *ended.lock().unwrap() = Some(format!("thread de captura: {err}"));
    }
    Capture {
        frame,
        preview_seq: 0,
        ended,
        stop: Some(Box::new(move || stop.store(true, Ordering::Relaxed))),
    }
}

fn run_gst(
    pipeline: &gst::Pipeline,
    slot: &Shared,
    stop: &AtomicBool,
    on_frame: impl Fn() + Send + 'static,
) -> Result<(), String> {
    let sink = pipeline
        .by_name("frames")
        .and_then(|sink| sink.dynamic_cast::<gst_app::AppSink>().ok())
        .ok_or("appsink não encontrado")?;
    let slot = Arc::clone(slot);
    let mut seq = 0;
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                seq += 1;
                if let Some(frame) = sample_frame(&sample, seq) {
                    *slot.lock().unwrap() = Some(Arc::new(frame));
                    on_frame();
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|err| format!("play: {err}"))?;
    let bus = pipeline.bus().ok_or("pipeline sem bus")?;
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        let message = bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(100),
            &[gst::MessageType::Error, gst::MessageType::Eos],
        );
        if let Some(message) = message {
            break Err(match message.view() {
                gst::MessageView::Error(err) => format!("{} ({:?})", err.error(), err.debug()),
                _ => "a captura terminou".to_string(),
            });
        }
    };
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// A BGRx appsink sample as a frame. BGRx rows are 4-byte aligned already:
/// no stride padding.
pub(crate) fn sample_frame(sample: &gst::Sample, seq: u64) -> Option<Frame> {
    let s = sample.caps()?.structure(0)?;
    let (width, height) = (s.get::<i32>("width").ok()?, s.get::<i32>("height").ok()?);
    let map = sample.buffer()?.map_readable().ok()?;
    Some(Frame {
        width: width as u32,
        height: height as u32,
        pixels: map.as_slice().to_vec(),
        bgr: true,
        seq,
    })
}

#[cfg(test)]
mod tests {
    use super::Frame;

    #[test]
    fn preview_swizzles_and_downscales() {
        // 2x2 BGRx.
        let pixels = vec![1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
        let frame = Frame {
            width: 2,
            height: 2,
            pixels,
            bgr: true,
            seq: 1,
        };
        let (size, rgba) = frame.preview_rgba(640);
        assert_eq!(size, [2, 2]);
        assert_eq!(
            rgba,
            [3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
        );
        // Downscale keeps every 2nd pixel of every 2nd row.
        assert_eq!(frame.preview_rgba(1), ([1, 1], vec![3, 2, 1, 255]));
    }
}
