//! Screen capture on Windows (docs/SCREENSHARE.md): GStreamer's
//! `d3d11screencapturesrc`, cursor included, into the shared slot as BGRx.
//! There is no system picker as on Linux: `sources()` lists monitors
//! (Desktop Duplication, by `HMONITOR`) and top-level windows (Windows
//! Graphics Capture, by `HWND`) for the app's own picker.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowTextW, IsIconic,
    IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

use super::{Frame, Source};

/// `MONITORINFOF_PRIMARY`.
const PRIMARY: u32 = 1;

/// Monitors (primary first) then visible, titled, uncloaked top-level
/// windows, as Discord's picker shows them.
pub(super) fn sources() -> Vec<(Source, String)> {
    let mut monitors: Vec<(bool, Source, String)> = Vec::new();
    unsafe extern "system" fn monitor(
        handle: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<(bool, Source, String)>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(handle, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
            let rect = info.monitorInfo.rcMonitor;
            let name = format!(
                "Tela {} ({}×{})",
                list.len() + 1,
                rect.right - rect.left,
                rect.bottom - rect.top
            );
            let primary = info.monitorInfo.dwFlags & PRIMARY != 0;
            list.push((primary, Source::Monitor(handle.0 as u64), name));
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor),
            LPARAM(&mut monitors as *mut _ as isize),
        );
    }
    monitors.sort_by_key(|(primary, ..)| !primary);

    let mut windows: Vec<(Source, String)> = Vec::new();
    unsafe extern "system" fn window(hwnd: HWND, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<(Source, String)>) };
        let mut cloaked = 0u32;
        let skip = unsafe {
            !IsWindowVisible(hwnd).as_bool()
                || IsIconic(hwnd).as_bool()
                || GetWindow(hwnd, GW_OWNER).is_ok()
                || GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
                // Suspended UWP apps are "visible" but cloaked.
                || DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED,
                    &mut cloaked as *mut u32 as *mut _,
                    size_of::<u32>() as u32,
                )
                .is_ok_and(|()| cloaked != 0)
        };
        if !skip {
            let mut title = [0u16; 256];
            let len = unsafe { GetWindowTextW(hwnd, &mut title) }.max(0) as usize;
            let title = String::from_utf16_lossy(&title[..len]);
            if !title.trim().is_empty() {
                list.push((Source::Window(hwnd.0 as u64), title));
            }
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(window), LPARAM(&mut windows as *mut _ as isize));
    }

    monitors
        .into_iter()
        .map(|(_, source, name)| (source, name))
        .chain(windows)
        .collect()
}

/// `d3d11screencapturesrc`'s properties for `source`.
fn source_props(source: Option<Source>) -> String {
    match source {
        // No monitor: the source's default, the primary one.
        None => String::new(),
        Some(Source::Monitor(handle)) => format!("monitor-handle={handle}"),
        // window-handle needs WGC (GStreamer ≥ 1.24); DXGI is monitors only.
        Some(Source::Window(hwnd)) => format!("capture-api=wgc window-handle={hwnd}"),
    }
}

/// The capture pipeline for `source`, ending in `appsink name=frames`.
pub(super) fn pipeline(source: Option<Source>) -> Result<gst::Pipeline, String> {
    gst::parse::launch(&format!(
        "d3d11screencapturesrc show-cursor=true {} ! d3d11download ! videoconvert \
         ! video/x-raw,format=BGRx ! appsink name=frames sync=false max-buffers=1 drop=true",
        source_props(source)
    ))
    .map_err(|err| format!("pipeline: {err}"))?
    .dynamic_cast::<gst::Pipeline>()
    .map_err(|_| "pipeline não é um bin".to_string())
}

/// One 320-px-wide frame of `source`, or `None` within 2 s (a window that
/// draws nothing, a monitor that's off).
pub(super) fn thumbnail(source: Source) -> Option<Frame> {
    gst::init().ok()?;
    let pipeline = gst::parse::launch(&format!(
        "d3d11screencapturesrc show-cursor=false num-buffers=1 {} ! d3d11download \
         ! videoconvert ! videoscale ! video/x-raw,format=BGRx,width=320,pixel-aspect-ratio=1/1 \
         ! appsink name=frames sync=false",
        source_props(Some(source))
    ))
    .ok()?
    .dynamic_cast::<gst::Pipeline>()
    .ok()?;
    let sink = pipeline
        .by_name("frames")?
        .dynamic_cast::<gst_app::AppSink>()
        .ok()?;
    let frame = pipeline
        .set_state(gst::State::Playing)
        .ok()
        .and_then(|_| sink.try_pull_sample(gst::ClockTime::from_seconds(2)))
        .and_then(|sample| super::sample_frame(&sample, 1));
    let _ = pipeline.set_state(gst::State::Null);
    frame
}
