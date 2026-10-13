//! Plugin catalog; each plugin owns its configuration UI and implementation.

use crate::app::VesktopApp;
use crate::plugins::{Id, golive_bypass};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub(crate) struct State {
    configuring: Option<Id>,
    bypass: golive_bypass::ui::State,
    restart: Option<Restart>,
    error: Option<String>,
}

struct Restart {
    ready: PathBuf,
    child: Option<std::process::Child>,
    unit: Option<String>,
    checked: Instant,
}

fn restart() -> Result<Restart, String> {
    let directory = crate::paths::data_dir().join("plugins/restart");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let name = format!("fastdiscord-restart-{}-{nonce}", std::process::id());
    let ready = directory.join(&name);
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut child = None;
    let mut unit = None;
    if golive_bypass::active() {
        // The user manager starts the replacement outside the old VPN namespace.
        let mut command = std::process::Command::new("systemd-run");
        command
            .args([
                "--user",
                "--collect",
                "--quiet",
                "--service-type=exec",
                "--unit",
            ])
            .arg(&name);
        for key in [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "XAUTHORITY",
            "GST_PLUGIN_PATH",
            "GST_PLUGIN_SYSTEM_PATH_1_0",
            "LD_LIBRARY_PATH",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.arg(format!("--setenv={key}={}", value.to_string_lossy()));
            }
        }
        let output = command
            .arg(executable)
            .arg("--ready-file")
            .arg(&ready)
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(
                "Não foi possível reiniciar fora do túnel. Verifique a sessão systemd --user."
                    .into(),
            );
        }
        unit = Some(name);
    } else {
        child = Some(
            std::process::Command::new(executable)
                .arg("--ready-file")
                .arg(&ready)
                .env_remove("FASTDISCORD_READY_FILE")
                .spawn()
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(Restart {
        ready,
        child,
        unit,
        checked: Instant::now(),
    })
}

/// Keep the current app until the replacement window exists, even if authorization is cancelled.
pub(crate) fn poll(app: &mut VesktopApp, ctx: &egui::Context) -> bool {
    let Some(restart) = &mut app.plugins_ui.restart else {
        return false;
    };
    ctx.request_repaint_after(Duration::from_millis(250));
    match restart.poll() {
        Ok(true) => {
            app.plugins_ui.restart = None;
            true
        }
        Ok(false) => false,
        Err(error) => {
            app.plugins_ui.error = Some(error);
            app.plugins_ui.restart = None;
            false
        }
    }
}

impl Restart {
    fn poll(&mut self) -> Result<bool, String> {
        if self.ready.is_file() {
            let _ = std::fs::remove_file(&self.ready);
            return Ok(true);
        }
        let mut failed = false;
        if let Some(child) = &mut self.child {
            failed = !matches!(child.try_wait(), Ok(None));
        }
        if let Some(unit) = &self.unit
            && self.checked.elapsed() >= Duration::from_secs(1)
        {
            self.checked = Instant::now();
            let output = std::process::Command::new("systemctl")
                .args(["--user", "show", unit, "--property=ActiveState", "--value"])
                .output();
            failed = !matches!(output, Ok(output) if output.status.success() && matches!(String::from_utf8_lossy(&output.stdout).trim(), "active" | "activating"));
        }
        if failed {
            Err("A nova sessão não iniciou. Confira o perfil e as dependências; se cancelou a senha do sistema, a sessão atual foi mantida.".into())
        } else {
            Ok(false)
        }
    }
}

pub fn show(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let fockytv_was_enabled = app.settings.plugins.fockytv.enabled;
    let mut changed = false;
    for plugin in Id::ALL {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(plugin.name());
                if ui.button("Configurar").clicked() {
                    app.plugins_ui.configuring = Some(plugin);
                }
                let enabled = match plugin {
                    Id::GoLiveBypass => &mut app.settings.plugins.golive_bypass.enabled,
                    Id::FockyTv => &mut app.settings.plugins.fockytv.enabled,
                };
                let configured = plugin != Id::GoLiveBypass
                    || app.settings.plugins.golive_bypass.profile.is_some();
                changed |= ui
                    .add_enabled(
                        (plugin != Id::GoLiveBypass || cfg!(target_os = "linux")) && configured,
                        egui::Checkbox::new(enabled, "Ativado"),
                    )
                    .on_hover_text(if configured {
                        "Ativar ou desativar plugin"
                    } else {
                        "Importe um .conf em Configurar antes de ativar"
                    })
                    .changed();
            });
            match plugin {
                Id::GoLiveBypass => {
                    ui.label(
                        "Go Live do Discord através de WireGuard, para transmitir e assistir.",
                    );
                    if golive_bypass::active() {
                        ui.colored_label(
                            crate::theme::GREEN,
                            "Túnel ativo nesta sessão; tem prioridade sobre o FockyTV.",
                        );
                    } else {
                        ui.label("Alterações na ativação do bypass exigem reiniciar o aplicativo.");
                    }
                }
                Id::FockyTv => {
                    ui.label("Transmissões e câmera através de um servidor FockyTV.");
                }
            }
            ui.hyperlink_to("Projeto original", plugin.source());
        });
        ui.add_space(8.0);
    }
    if app.settings.plugins.golive_bypass.enabled
        && app.settings.plugins.golive_bypass.profile.is_none()
    {
        ui.colored_label(
            crate::theme::RED,
            "Configure o GoLiveBypass e importe um .conf antes de reiniciar.",
        );
        app.plugins_ui.configuring = Some(Id::GoLiveBypass);
    }
    if let Some(plugin) = app.plugins_ui.configuring {
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading(format!("Configurar {}", plugin.name()));
            if ui.button("Fechar configuração").clicked() {
                app.plugins_ui.configuring = None;
            }
        });
        match plugin {
            Id::GoLiveBypass => {
                changed |= golive_bypass::ui::configure(
                    &mut app.settings.plugins.golive_bypass,
                    &mut app.plugins_ui.bypass,
                    &app.handle,
                    ui,
                )
            }
            Id::FockyTv => {
                let username = app
                    .me
                    .as_ref()
                    .map(|me| me.username.as_str())
                    .unwrap_or_default();
                changed |= crate::plugins::fockytv::ui::configure(
                    &mut app.settings.plugins.fockytv,
                    username,
                    ui,
                );
            }
        }
    }
    ui.separator();
    if ui
        .add_enabled(
            app.plugins_ui.restart.is_none()
                && (!app.settings.plugins.golive_bypass.enabled
                    || app.settings.plugins.golive_bypass.profile.is_some()),
            egui::Button::new("Reiniciar para aplicar o bypass"),
        )
        .clicked()
    {
        app.settings.save();
        match restart() {
            Ok(restart) => {
                app.plugins_ui.restart = Some(restart);
                app.plugins_ui.error = None;
            }
            Err(error) => app.plugins_ui.error = Some(error),
        }
    }
    if app.plugins_ui.restart.is_some() {
        ui.label("Aguardando a nova janela ou a autorização administrativa. Esta sessão será encerrada quando a nova abrir.");
    }
    if let Some(error) = &app.plugins_ui.error {
        ui.colored_label(crate::theme::RED, error);
    }
    if changed {
        if fockytv_was_enabled && !app.settings.plugins.fockytv.enabled {
            if app.fockytv.is_some() {
                app.stop_screen_share();
            }
            if app.camera_publisher.is_some() {
                app.stop_camera();
            }
            if app
                .watch
                .as_ref()
                .is_some_and(|watch| watch.fockytv.is_some())
            {
                app.stop_watching();
            }
            app.fockytv_live.clear();
        }
        app.settings.save();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn failed_launch_does_not_close_the_current_session() {
        let mut child = std::process::Command::new("/bin/false").spawn().unwrap();
        child.wait().unwrap();
        let mut restart = Restart {
            ready: PathBuf::from("/nonexistent/fastdiscord-ready"),
            child: Some(child),
            unit: None,
            checked: Instant::now(),
        };
        assert!(restart.poll().is_err());
    }

    #[test]
    fn replacement_window_marker_is_required_before_closing_the_current_session() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ready =
            std::env::temp_dir().join(format!("fastdiscord-ready-{}-{nonce}", std::process::id()));
        let mut restart = Restart {
            ready: ready.clone(),
            child: None,
            unit: None,
            checked: Instant::now(),
        };
        assert!(!restart.poll().unwrap());
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&ready)
            .unwrap();
        assert!(restart.poll().unwrap());
        assert!(!ready.exists());
    }
}
