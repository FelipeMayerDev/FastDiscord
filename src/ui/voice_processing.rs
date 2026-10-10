//! Microphone processing options (bitrate, AGC, compressor, echo
//! cancellation), shown in the voice section of the settings.

use egui::RichText;

use crate::app::VesktopApp;
use crate::theme;

pub fn voice_processing_settings(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let settings = &mut app.settings;
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Qualidade do áudio (bitrate):");
        changed |= ui
            .add(egui::Slider::new(&mut settings.opus_bitrate_kbps, 8..=128).suffix(" kbps"))
            .on_hover_text("O Discord usa 64 kbps; servidores com boost aceitam até 128.")
            .changed();
    });
    changed |= ui
        .checkbox(&mut settings.auto_gain, "Ganho automático")
        .on_hover_text("Ajusta o volume do microfone para um nível de voz constante.")
        .changed();
    changed |= ui
        .checkbox(&mut settings.compressor, "Compressor e limitador")
        .on_hover_text("Segura picos e gritos, sem estourar o áudio.")
        .changed();
    ui.add_enabled_ui(cfg!(target_os = "linux"), |ui| {
        changed |= ui
            .checkbox(&mut settings.echo_cancellation, "Cancelamento de eco")
            .on_hover_text(
                "Remove do microfone o som que sai dos alto-falantes (WebRTC AEC do servidor de som).",
            )
            .on_disabled_hover_text("Ainda não disponível no Windows.")
            .changed();
    });
    if settings.echo_cancellation && cfg!(target_os = "linux") {
        ui.label(
            RichText::new("Usa o module-echo-cancel do PipeWire/PulseAudio.")
                .small()
                .color(theme::MUTED),
        );
    }
    if changed {
        app.settings.save();
        app.push_voice_audio_config();
    }
}
