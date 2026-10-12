//! Webcam capture (docs/SCREENSHARE.md, "Câmera"): GStreamer's device
//! monitor lists the cameras (PipeWire or V4L2 on Linux, Media Foundation
//! or KS on Windows) and makes the source; frames land in a capture slot
//! like the screen's, for the sidebar preview and the FockyTV publish.

use std::sync::OnceLock;

use gstreamer as gst;
use gstreamer::prelude::*;

use crate::backend::capture::{self, Capture};

/// Tallest camera mode we ask for: 720p is plenty for a face and keeps the
/// publish light next to a screen share.
const MAX_HEIGHT: i32 = 720;

/// Camera names, cached for the session like the audio devices. Two
/// cameras with the same name (a laptop's RGB and IR sensors) get " (2)".
pub fn devices() -> Vec<String> {
    static DEVICES: OnceLock<Vec<String>> = OnceLock::new();
    DEVICES
        .get_or_init(|| {
            if gst::init().is_err() {
                return Vec::new();
            }
            labels(&list())
        })
        .clone()
}

/// The cameras, colour ones first.
fn list() -> Vec<gst::Device> {
    // V4L2 straight on Linux: the device monitor prefers PipeWire's nodes,
    // and pipewiresrc can't negotiate caps ranges and ignored the exact
    // mode in testing (640x480 YUY2 instead of 720p MJPEG).
    #[cfg(target_os = "linux")]
    let provider = gst::DeviceProviderFactory::by_name("v4l2deviceprovider");
    #[cfg(not(target_os = "linux"))]
    let provider: Option<gst::DeviceProvider> = None;
    let mut devices: Vec<gst::Device> = match provider {
        Some(provider) => provider.devices().into_iter().collect(),
        None => {
            let monitor = gst::DeviceMonitor::new();
            monitor.add_filter(Some("Video/Source"), None);
            monitor.devices().into_iter().collect()
        }
    };
    // A laptop's IR sensor (face unlock) only has grey modes; keep it off
    // the default spot.
    devices.sort_by_key(|device| {
        device.caps().is_some_and(|caps| {
            caps.iter_with_features()
                .filter(|(_, features)| system_memory(features))
                .all(|(s, _)| {
                    s.get::<&str>("format")
                        .is_ok_and(|format| format.starts_with("GRAY"))
                })
        })
    });
    devices
}

fn labels(devices: &[gst::Device]) -> Vec<String> {
    let names: Vec<String> = devices
        .iter()
        .map(|device| device.display_name().to_string())
        .collect();
    names
        .iter()
        .enumerate()
        .map(
            |(index, name)| match names[..index].iter().filter(|n| *n == name).count() {
                0 => name.clone(),
                same => format!("{name} ({})", same + 1),
            },
        )
        .collect()
}

/// Starts the camera labelled `wanted` (`None` or gone: the first one).
pub fn start(wanted: Option<String>, on_frame: impl Fn() + Send + 'static) -> Capture {
    capture::start_gst(move || pipeline(wanted.as_deref()), on_frame)
}

fn pipeline(wanted: Option<&str>) -> Result<gst::Pipeline, String> {
    let devices = list();
    let labels = labels(&devices);
    let index = wanted
        .and_then(|wanted| labels.iter().position(|label| label == wanted))
        .unwrap_or(0);
    let device = devices.get(index).ok_or("nenhuma câmera encontrada")?;
    let source = device
        .create_element(None)
        .map_err(|err| format!("câmera: {err}"))?;
    let caps = device
        .caps()
        .and_then(|caps| pick_caps(&caps))
        .ok_or("a câmera não oferece um formato suportado")?;
    log::info!("câmera: {} {caps}", labels[index]);
    // decodebin handles MJPEG (most webcams above 640x480) and passes raw
    // through. All in the pipeline itself: behind a ghost-padded bin the
    // sink prerolled one frame and never reached PLAYING.
    let pipeline = gst::parse::launch(&format!(
        "capsfilter name=mode caps=\"{caps}\" ! decodebin ! videoconvert \
         ! video/x-raw,format=BGRx ! appsink name=frames sync=false max-buffers=1 drop=true"
    ))
    .map_err(|err| format!("pipeline: {err}"))?
    .dynamic_cast::<gst::Pipeline>()
    .map_err(|_| "pipeline não é um bin".to_string())?;
    let mode = pipeline
        .by_name("mode")
        .ok_or("capsfilter não encontrado")?;
    pipeline.add(&source).map_err(|err| err.to_string())?;
    source.link(&mode).map_err(|err| format!("câmera: {err}"))?;
    Ok(pipeline)
}

/// One fixed camera mode: the largest up to 720p (else the smallest), raw
/// or MJPEG in system memory, at the rate nearest 30 fps. Exact caps,
/// because pipewiresrc fails to negotiate ranges ("error set output
/// format").
fn pick_caps(caps: &gst::CapsRef) -> Option<gst::Caps> {
    let modes = caps.iter_with_features().filter_map(|(s, features)| {
        let usable = system_memory(features);
        let kind = ["video/x-raw", "image/jpeg"].contains(&s.name().as_str());
        let height = s.get::<i32>("height").ok()?;
        let width = s.get::<i32>("width").ok()?;
        (usable && kind).then_some((width * height, height, s))
    });
    let (_, _, best) = modes.fold(None, |best: Option<(i32, i32, _)>, mode| {
        let fits = |height: i32| height <= MAX_HEIGHT;
        match best {
            None => Some(mode),
            Some(best) => Some(match (fits(mode.1), fits(best.1)) {
                (true, false) => mode,
                (false, true) => best,
                (true, true) if mode.0 > best.0 => mode,
                (false, false) if mode.0 < best.0 => mode,
                _ => best,
            }),
        }
    })?;
    let mut structure = best.to_owned();
    if structure.has_field("framerate") {
        structure.fixate_field_nearest_fraction("framerate", gst::Fraction::new(30, 1));
    }
    structure.fixate();
    Some(gst::Caps::builder_full().structure(structure).build())
}

/// Plain buffers, not DMA-BUF modes, which need a GPU path downstream.
fn system_memory(features: &gst::CapsFeaturesRef) -> bool {
    features.is_empty() || features.contains(gst::CAPS_FEATURE_MEMORY_SYSTEM_MEMORY)
}

#[cfg(test)]
mod tests {
    use super::pick_caps;
    use gstreamer as gst;

    #[test]
    fn camera_mode_prefers_720p_at_30_fps() {
        gst::init().unwrap();
        // A laptop webcam as PipeWire lists it (trimmed).
        let caps: gst::Caps = "image/jpeg, width=640, height=480, framerate={ 30/1, 15/1 }; \
             image/jpeg, width=1280, height=720, framerate={ 15/1, 30/1 }; \
             image/jpeg, width=1920, height=1080, framerate={ 30/1, 15/1 }; \
             video/x-raw(memory:DMABuf), format=DMA_DRM, width=640, height=480, framerate=30/1; \
             video/x-raw, format=YUY2, width=640, height=480, framerate={ 30/1, 15/1 }"
            .parse()
            .unwrap();
        let picked = pick_caps(&caps).unwrap();
        let s = picked.structure(0).unwrap();
        assert_eq!(s.name(), "image/jpeg");
        assert_eq!(s.get::<i32>("height").unwrap(), 720);
        assert_eq!(
            s.get::<gst::Fraction>("framerate").unwrap(),
            gst::Fraction::new(30, 1)
        );
        // Only modes above 720p: the smallest of them.
        let caps: gst::Caps = "video/x-raw, format=NV12, width=3840, height=2160; \
             video/x-raw, format=NV12, width=1920, height=1080"
            .parse()
            .unwrap();
        let picked = pick_caps(&caps).unwrap();
        assert_eq!(
            picked.structure(0).unwrap().get::<i32>("height").unwrap(),
            1080
        );
    }
}
