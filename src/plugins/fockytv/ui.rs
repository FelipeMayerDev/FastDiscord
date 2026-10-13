//! Configuration owned by the FockyTV plugin.

pub fn configure(settings: &mut super::Settings, username: &str, ui: &mut egui::Ui) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("URL do servidor:");
        changed |= ui.text_edit_singleline(&mut settings.url).changed();
    });
    if !matches!(reqwest::Url::parse(&settings.url), Ok(url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
    {
        ui.colored_label(
            crate::theme::RED,
            "Informe uma URL http:// ou https:// válida.",
        );
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Taxa de quadros:");
        for fps in [60, 30, 15] {
            if ui
                .radio(settings.fps == fps, format!("{fps} fps"))
                .clicked()
            {
                settings.fps = fps;
                changed = true;
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Resolução:");
        for (height, label) in [(0, "Original"), (1080, "1080p"), (720, "720p")] {
            if ui.radio(settings.max_height == height, label).clicked() {
                settings.max_height = height;
                changed = true;
            }
        }
    });
    let mut nick = settings.nick.clone().unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label("Nick:");
        if ui
            .add(egui::TextEdit::singleline(&mut nick).hint_text(username))
            .changed()
        {
            settings.nick = Some(nick.clone()).filter(|nick| !nick.trim().is_empty());
            changed = true;
        }
    });
    let nick = settings
        .nick
        .as_deref()
        .filter(|nick| !nick.trim().is_empty())
        .unwrap_or(username)
        .trim();
    if let Err(error) = super::validate_stream_key(nick) {
        ui.colored_label(crate::theme::RED, error);
    }
    ui.label("As opções de vídeo valem para a próxima transmissão.");
    changed
}
