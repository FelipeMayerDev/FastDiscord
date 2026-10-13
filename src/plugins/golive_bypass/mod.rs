//! Native adaptation of https://github.com/bezumiya/GoLiveBypass.

pub mod ui;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Path only: the private key remains in the imported file, never in settings JSON.
    pub profile: Option<PathBuf>,
}

pub fn active() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("FASTDISCORD_WIREGUARD").is_some()
}

#[cfg(target_os = "linux")]
pub fn validate_profile(path: &Path) -> Result<(), String> {
    use base64::Engine;
    let bytes = read_profile(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "O .conf deve ser um arquivo de texto.")?;
    let (mut interface, mut peer, mut private_key, mut public_key, mut default_route) =
        (false, false, false, false, false);
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') {
            interface = line == "[Interface]";
            peer = line == "[Peer]";
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if (interface && key == "PrivateKey")
            || (peer && matches!(key, "PublicKey" | "PresharedKey"))
        {
            if !matches!(base64::engine::general_purpose::STANDARD.decode(value), Ok(bytes) if bytes.len() == 32)
            {
                return Err(format!("Chave {key} inválida no perfil WireGuard."));
            }
            private_key |= interface && key == "PrivateKey";
            public_key |= peer && key == "PublicKey";
        }
        if peer && key == "AllowedIPs" {
            default_route |= value.split(',').any(|route| route.trim() == "0.0.0.0/0");
        }
    }
    if !private_key || !public_key || !default_route {
        return Err(
            "O perfil precisa de PrivateKey, PublicKey e AllowedIPs incluindo 0.0.0.0/0.".into(),
        );
    }
    let output = std::process::Command::new("/bin/bash")
        .args([
            "-c",
            include_str!("linux.sh"),
            "fastdiscord-bypass",
            "--check",
        ])
        .arg(path)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[cfg(target_os = "linux")]
pub fn import_profile(source: &Path) -> Result<PathBuf, String> {
    import_profile_to(
        source,
        &crate::paths::data_dir().join("plugins/golive-bypass"),
    )
}

#[cfg(target_os = "linux")]
fn read_profile(source: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    if !std::fs::metadata(source)
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("Selecione um arquivo .conf local.".into());
    }
    let file = std::fs::File::open(source).map_err(|error| error.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("Selecione um arquivo .conf local.".into());
    }
    let mut bytes = Vec::new();
    file.take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 65_536 {
        return Err("O perfil WireGuard deve ter até 64 KiB.".into());
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn import_profile_to(source: &Path, directory: &Path) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let bytes = read_profile(source)?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let temporary = directory.join(format!("import-{}-{nonce}.conf", std::process::id()));
    let target = directory.join("wireguard.conf");
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        validate_profile(&temporary)?;
        std::fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
        Ok(target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use base64::Engine;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn imports_are_private_and_failed_import_preserves_the_last_profile() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "fastdiscord-profile-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.conf");
        let directory = root.join("plugin");
        let key = base64::engine::general_purpose::STANDARD.encode([7; 32]);
        let profile = format!(
            "[Interface]\nPrivateKey = {key}\nAddress = 10.0.0.2/32\nDNS = 1.1.1.1\n[Peer]\nPublicKey = {key}\nAllowedIPs = 0.0.0.0/0\nEndpoint = example.com:51820\n"
        );
        std::fs::write(&source, &profile).unwrap();
        let imported = import_profile_to(&source, &directory).unwrap();
        assert_eq!(std::fs::read_to_string(&imported).unwrap(), profile);
        assert_eq!(
            std::fs::metadata(&imported).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let json = serde_json::to_string(&Settings {
            enabled: true,
            profile: Some(imported.clone()),
        })
        .unwrap();
        assert!(!json.contains(&key));
        std::fs::write(&source, profile.replace(&key, "invalid-secret")).unwrap();
        let error = import_profile_to(&source, &directory).unwrap_err();
        assert!(!error.contains("invalid-secret"));
        assert_eq!(std::fs::read_to_string(&imported).unwrap(), profile);
        std::fs::write(&source, vec![b'x'; 65_537]).unwrap();
        assert!(import_profile_to(&source, &directory).is_err());
        assert_eq!(std::fs::read_to_string(&imported).unwrap(), profile);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(not(target_os = "linux"))]
pub fn import_profile(_: &Path) -> Result<PathBuf, String> {
    Err("O bypass está disponível apenas no Linux nesta versão.".into())
}

#[cfg(target_os = "linux")]
pub fn launch(profile: &Path, ready: Option<&Path>) -> Result<(), String> {
    let profile = profile.canonicalize().map_err(|error| error.to_string())?;
    check_dependencies()?;
    validate_profile(&profile)?;
    let user = std::process::Command::new("id")
        .arg("-un")
        .output()
        .map_err(|error| error.to_string())?;
    if !user.status.success() {
        return Err("Não foi possível identificar o usuário.".into());
    }
    let user = String::from_utf8(user.stdout).map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new("pkexec");
    command
        .args([
            "/bin/bash",
            "-c",
            include_str!("linux.sh"),
            "fastdiscord-bypass",
        ])
        .arg(profile)
        .arg(std::env::current_exe().map_err(|error| error.to_string())?)
        .arg(user.trim());
    for name in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XAUTHORITY",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.arg(format!("{name}={}", value.to_string_lossy()));
        }
    }
    if let Some(ready) = ready {
        command.arg(format!("FASTDISCORD_READY_FILE={}", ready.display()));
    }
    let status = command.status().map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Bypass não iniciado ou encerrado com erro: {status}"
        ))
    }
}

#[cfg(not(target_os = "linux"))]
pub fn launch(_: &Path, _: Option<&Path>) -> Result<(), String> {
    Err("O bypass está disponível apenas no Linux nesta versão.".into())
}

#[cfg(target_os = "linux")]
pub fn check_dependencies() -> Result<(), String> {
    for tool in ["ip", "wg", "wg-quick", "runuser", "pkexec"] {
        let found = std::process::Command::new("/bin/bash")
            .env("PATH", "/usr/bin:/bin")
            .args(["-c", "command -v \"$1\" >/dev/null", "fastdiscord", tool])
            .status()
            .map_err(|error| error.to_string())?;
        if !found.success() {
            return Err(format!(
                "Dependência ausente: {tool}. Instale com: sudo pacman -S --needed wireguard-tools iproute2 polkit"
            ));
        }
    }
    Ok(())
}
