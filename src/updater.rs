//! Self-update from GitHub releases. Only the release builds update
//! themselves: the AppImage (`$APPIMAGE`) and the Windows exe. The new file
//! replaces the old one in place and takes effect on the next start.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/FelipeMayerDev/FastDiscord/releases/latest";
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// An update already written to disk, waiting for a restart.
#[derive(Clone)]
pub struct Installed {
    pub version: String,
    pub path: PathBuf,
}

pub type Slot = Arc<Mutex<Option<Installed>>>;

/// The release asset this build updates from, and the file it replaces.
fn target() -> Option<(&'static str, PathBuf)> {
    if cfg!(target_os = "linux") {
        let appimage = std::env::var_os("APPIMAGE")?;
        Some(("FastDiscord-x86_64.AppImage", appimage.into()))
    } else if cfg!(windows) {
        Some((
            "FastDiscord-windows-x86_64.exe",
            std::env::current_exe().ok()?,
        ))
    } else {
        None
    }
}

/// Checks now and every few hours; fills the slot once an update is installed.
pub fn spawn(handle: &tokio::runtime::Handle, ctx: egui::Context) -> Slot {
    let slot = Slot::default();
    let Some((asset, path)) = target() else {
        return slot;
    };
    // Windows can't delete a running exe, so the last update left it here.
    let _ = std::fs::remove_file(path.with_extension("old"));
    let out = Arc::clone(&slot);
    handle.spawn(async move {
        loop {
            match check(asset, &path).await {
                Ok(Some(version)) => {
                    log::info!("update {version} installed");
                    *out.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Installed { version, path });
                    ctx.request_repaint();
                    return;
                }
                Ok(None) => {}
                Err(err) => log::warn!("update check failed: {err:#}"),
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
    slot
}

async fn check(asset: &str, path: &Path) -> anyhow::Result<Option<String>> {
    let http = reqwest::Client::builder()
        .user_agent(concat!("FastDiscord/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(600))
        .build()?;
    let release: serde_json::Value = http
        .get(LATEST)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let tag = release["tag_name"].as_str().unwrap_or_default();
    if !is_newer(tag, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let Some(found) = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"] == asset)
    else {
        return Ok(None);
    };
    let url = found["browser_download_url"].as_str().unwrap_or_default();
    let size = found["size"].as_u64().unwrap_or_default();

    let tmp = path.with_extension("update");
    let mut response = http.get(url).send().await?.error_for_status()?;
    let mut file = std::fs::File::create(&tmp)?;
    let mut written = 0;
    while let Some(chunk) = response.chunk().await? {
        std::io::Write::write_all(&mut file, &chunk)?;
        written += chunk.len() as u64;
    }
    file.sync_all()?;
    drop(file);
    if written == 0 || written != size {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("download truncated: {written} of {size} bytes");
    }
    install(&tmp, path)?;
    Ok(Some(tag.trim_start_matches('v').to_string()))
}

#[cfg(unix)]
fn install(tmp: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o755))?;
    // The running AppImage keeps its old inode; rename is atomic.
    std::fs::rename(tmp, path)
}

#[cfg(windows)]
fn install(tmp: &Path, path: &Path) -> std::io::Result<()> {
    // A running exe can be renamed but not overwritten.
    let old = path.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(path, &old)?;
    std::fs::rename(tmp, path).inspect_err(|_| {
        let _ = std::fs::rename(&old, path);
    })
}

/// Starts the updated build; the caller then quits.
pub fn restart(installed: &Installed) {
    if let Err(err) = std::process::Command::new(&installed.path).spawn() {
        log::error!("failed to start the updated build: {err}");
    }
}

/// `v0.10.0` > `0.9.1`; anything unparsable is never newer.
fn is_newer(tag: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().ok())
            .collect()
    };
    matches!((parse(tag), parse(current)), (Some(t), Some(c)) if t > c)
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("v0.10.0", "0.9.1"));
        assert!(is_newer("v0.2.1", "0.2.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
        assert!(!is_newer("v0.3.0-rc1", "0.2.0"));
        assert!(!is_newer("", "0.2.0"));
    }
}
