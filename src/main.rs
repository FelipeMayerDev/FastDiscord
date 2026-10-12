//! FastDiscord — a native Discord client built with Rust and egui.

use clap::Parser;

/// A native Discord client built with Rust and egui.
#[derive(Parser, Debug)]
#[command(name = "fastdiscord", version, about)]
struct Args {
    /// Discord account token, overriding the one saved in the settings file.
    #[arg(long, value_name = "TOKEN")]
    token: Option<String>,
    /// Start with experimental Discord Go Live instead of FockyTV.
    #[arg(long)]
    go_live: bool,
    /// Linux: launch through an isolated WireGuard tunnel using this .conf.
    #[arg(long, value_name = "FILE", conflicts_with = "token")]
    wireguard: Option<std::path::PathBuf>,
}

fn main() -> anyhow::Result<()> {
    // The portable Windows build ships its GStreamer plugins next to the
    // exe (the DLLs they need sit beside it too, where the loader looks).
    #[cfg(windows)]
    if let Some(plugins) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("lib").join("gstreamer-1.0")))
        .filter(|dir| dir.is_dir())
    {
        // SAFETY: still single-threaded here, before any GStreamer use.
        unsafe { std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", plugins) };
    }

    // tracing-subscriber instead of env_logger: it captures both our `log`
    // records and songbird's `tracing` internals (voice WS, DAVE, mixer).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .without_time()
        .init();
    let args = Args::parse();

    if let Some(profile) = args.wireguard {
        #[cfg(target_os = "linux")]
        {
            let profile = profile.canonicalize()?;
            for tool in ["ip", "wg", "wg-quick", "runuser", "pkexec"] {
                let found = std::process::Command::new("/bin/bash")
                    .args(["-c", "command -v \"$1\" >/dev/null", "fastdiscord", tool])
                    .status()?;
                anyhow::ensure!(
                    found.success(),
                    "dependência ausente: {tool}; instale wireguard-tools, iproute2 e polkit"
                );
            }
            let checked = std::process::Command::new("/bin/bash")
                .args([
                    "-c",
                    include_str!("../packaging/golive-bypass.sh"),
                    "fastdiscord-bypass",
                    "--check",
                ])
                .arg(&profile)
                .status()?;
            anyhow::ensure!(checked.success(), "perfil WireGuard inválido");
            let user = std::process::Command::new("id").arg("-un").output()?;
            anyhow::ensure!(
                user.status.success(),
                "não foi possível identificar o usuário"
            );
            let user = String::from_utf8(user.stdout)?;
            let mut command = std::process::Command::new("pkexec");
            command
                .args([
                    "/bin/bash",
                    "-c",
                    include_str!("../packaging/golive-bypass.sh"),
                    "fastdiscord-bypass",
                ])
                .arg(profile)
                .arg(std::env::current_exe()?)
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
            let status = command.status()?;
            anyhow::ensure!(
                status.success(),
                "bypass não iniciado ou encerrado com erro: {status}"
            );
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!(
            "--wireguard está disponível apenas no Linux: {}",
            profile.display()
        );
    }

    let mut settings = fastdiscord::settings::Settings::load().unwrap_or_default();
    if let Some(token) = args.token {
        settings.token = Some(token);
        settings.save();
    }
    if args.go_live {
        settings.fockytv_share = false;
        log::info!("Go Live experimental selecionado");
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([940.0, 600.0])
        .with_title("FastDiscord")
        // Matches packaging/fastdiscord.desktop, so Wayland docks find the icon.
        .with_app_id("fastdiscord");
    if let Some(icon) = fastdiscord::window::load_icon() {
        viewport = viewport.with_icon(icon);
    }

    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport,
        // With vsync, eglSwapBuffers waits for the compositor's frame
        // callback, which Hyprland withholds from windows that aren't
        // visible (other workspace): any repaint while hidden (voice events,
        // the FockyTV poll) blocked the main thread, pings went unanswered
        // and Hyprland showed "Application Not Responding". egui repaints
        // only on demand, so no vsync costs nothing.
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: false,
            ..Default::default()
        },
        ..Default::default()
    };

    #[cfg(all(feature = "tray", target_os = "linux"))]
    if settings.tray && std::env::var_os("DISPLAY").is_some() {
        use winit::platform::x11::EventLoopBuilderExtX11;
        // winit cannot hide or restore Wayland windows; XWayland supports both.
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_x11();
        }));
    }

    eframe::run_native(
        "FastDiscord",
        options,
        Box::new(move |cc| {
            let app = fastdiscord::app::VesktopApp::new(cc, settings, handle, event_tx, event_rx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow::anyhow!("eframe exited with an error: {err}"))?;

    // Background tasks die with the runtime once the UI loop returns.
    drop(runtime);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_args_keep_token_out_of_the_privileged_launcher() {
        assert!(
            Args::try_parse_from([
                "fastdiscord",
                "--wireguard",
                "vpn.conf",
                "--token",
                "secret"
            ])
            .is_err()
        );
        let args = Args::try_parse_from(["fastdiscord", "--wireguard", "vpn.conf"]).unwrap();
        assert_eq!(
            args.wireguard.unwrap(),
            std::path::PathBuf::from("vpn.conf")
        );
        assert!(
            Args::try_parse_from(["fastdiscord", "--go-live"])
                .unwrap()
                .go_live
        );
    }
}
