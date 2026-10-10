//! Single-file Windows build. The portable zip (fastdiscord.exe, the
//! GStreamer DLLs and plugins) is embedded here; the first run of each
//! build extracts it to %LOCALAPPDATA%\FastDiscord\versions\<build>, later runs
//! start it straight away. packaging/windows.sh sets FASTDISCORD_ZIP and
//! FASTDISCORD_BUILD (version plus the zip's hash).

#![windows_subsystem = "windows"]

use std::ffi::OsStr;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const PAYLOAD: &[u8] = include_bytes!(env!("FASTDISCORD_ZIP"));
const BUILD: &str = env!("FASTDISCORD_BUILD");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    if let Err(err) = run() {
        message_box(&format!("Não foi possível iniciar o FastDiscord:\n{err}"));
    }
}

fn run() -> std::io::Result<()> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        std::io::Error::other("LOCALAPPDATA não definido")
    })?)
    // Its own folder: the app keeps its data under FastDiscord\FastDiscord,
    // which pruning old builds must never touch.
    .join("FastDiscord")
    .join("versions");
    let dir = base.join(BUILD);
    // The zip holds a top-level FastDiscord\ folder.
    let exe = dir.join("FastDiscord").join("fastdiscord.exe");
    if !exe.is_file() {
        extract(&base, &dir)?;
        prune(&base);
    }
    Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .current_dir(exe.parent().unwrap())
        .spawn()?;
    Ok(())
}

/// Extracts into a scratch folder and renames it into place, so a crash
/// halfway never leaves a build that looks complete.
fn extract(base: &Path, dir: &Path) -> std::io::Result<()> {
    let tmp = base.join(format!("{BUILD}.tmp"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    let zip = tmp.join("payload.zip");
    fs::write(&zip, PAYLOAD)?;
    // Windows' own bsdtar (System32, Windows 10 1803+) reads zip; a Git
    // Bash GNU tar earlier on PATH would not.
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let status = Command::new(Path::new(&system_root).join(r"System32\tar.exe"))
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&tmp)
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!("tar.exe falhou ({status})")));
    }
    fs::remove_file(&zip)?;
    let _ = fs::remove_dir_all(dir);
    fs::rename(&tmp, dir)
}

/// Drops the other builds' folders. One still running keeps its files
/// locked; it goes on the next update.
fn prune(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() != OsStr::new(BUILD) && entry.path().is_dir() {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn message_box(text: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(hwnd: *mut u8, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let wide = |s: &str| OsStr::new(s).encode_wide().chain([0]).collect::<Vec<u16>>();
    const MB_ICONERROR: u32 = 0x10;
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide("FastDiscord").as_ptr(),
            MB_ICONERROR,
        );
    }
}
