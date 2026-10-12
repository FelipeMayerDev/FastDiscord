//! Linux bypass dependencies, checked by both the UI and the CLI before authentication.

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
