//! Local profile import; no private keys or VPN passwords enter the settings file.

#[derive(Default)]
pub struct State {
    importing: Option<tokio::sync::oneshot::Receiver<Result<Option<std::path::PathBuf>, String>>>,
    pub error: Option<String>,
}

pub fn configure(
    settings: &mut super::Settings,
    state: &mut State,
    handle: &tokio::runtime::Handle,
    ui: &mut egui::Ui,
) -> bool {
    let mut changed = false;
    if let Some(receiver) = &mut state.importing {
        match receiver.try_recv() {
            Ok(Ok(Some(path))) => {
                settings.profile = Some(path);
                state.error = None;
                state.importing = None;
                changed = true;
            }
            Ok(Ok(None)) => state.importing = None,
            Ok(Err(error)) => {
                state.error = Some(error);
                state.importing = None;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                state.error = Some("A importação foi interrompida.".into());
                state.importing = None;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => ui
                .ctx()
                .request_repaint_after(std::time::Duration::from_millis(200)),
        }
    }
    ui.label("Importe um perfil WireGuard com servidor fora do Brasil. O arquivo fica somente neste computador.");
    if let Some(path) = &settings.profile {
        ui.label(format!("Perfil: {}", path.display()));
    }
    #[cfg(not(target_os = "linux"))]
    ui.colored_label(
        crate::theme::RED,
        "Disponível apenas no Linux nesta versão.",
    );
    if ui
        .add_enabled(
            cfg!(target_os = "linux") && state.importing.is_none(),
            egui::Button::new("Importar .conf"),
        )
        .clicked()
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.importing = Some(rx);
        let ctx = ui.ctx().clone();
        handle.spawn(async move {
            let result = match crate::backend::file_picker::pick_file().await {
                Ok(Some(path)) => tokio::task::spawn_blocking(move || super::import_profile(&path))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map(Some)),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
    if let Some(error) = &state.error {
        ui.colored_label(crate::theme::RED, error);
    }
    ui.label(
        "A ativação pede a senha do sistema e reinicia o app. O túnel cobre também voz e vídeo.",
    );
    changed
}
