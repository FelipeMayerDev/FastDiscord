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
    /// Start without the saved bypass plugin, for recovery or comparison.
    #[arg(long, conflicts_with = "wireguard")]
    no_bypass: bool,
    #[arg(long, hide = true)]
    ready_file: Option<std::path::PathBuf>,
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

    let mut settings = fastdiscord::settings::Settings::load().unwrap_or_default();
    let ready = args
        .ready_file
        .or_else(|| std::env::var_os("FASTDISCORD_READY_FILE").map(std::path::PathBuf::from));
    if args.no_bypass {
        settings.plugins.golive_bypass.enabled = false;
    }
    if let Some(token) = args.token {
        settings.token = Some(token);
        settings.save();
    }
    if let Some(profile) = args.wireguard {
        let imported = fastdiscord::plugins::golive_bypass::import_profile(&profile)
            .map_err(anyhow::Error::msg)?;
        settings.plugins.golive_bypass.profile = Some(imported);
        settings.plugins.golive_bypass.enabled = true;
        settings.save();
    }
    if settings.plugins.golive_bypass.enabled && !fastdiscord::plugins::golive_bypass::active() {
        if let Some(profile) = &settings.plugins.golive_bypass.profile {
            fastdiscord::plugins::golive_bypass::launch(profile, ready.as_deref())
                .map_err(anyhow::Error::msg)?;
            return Ok(());
        }
    }
    if args.go_live && !fastdiscord::plugins::golive_bypass::active() {
        settings.plugins.fockytv.enabled = false;
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
            if let Some(ready) = &ready {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(ready)?;
            }
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
