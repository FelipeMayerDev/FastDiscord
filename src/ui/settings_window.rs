//! Settings window, Discord-style: a section nav on the left (account,
//! appearance, voice, screen share, behavior) and one page at a time. Mirrors
//! the subset of Vesktop's Electron settings that a native client can honor.

use egui::RichText;

use crate::app::VesktopApp;
use crate::paths;
use crate::settings::Theme;
use crate::theme;

/// The window's sections, Discord-style: a nav on the left, one page at a
/// time on the right.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTab {
    #[default]
    Account,
    Appearance,
    Voice,
    ScreenShare,
    Behavior,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            Self::Account => "Minha conta",
            Self::Appearance => "Aparência",
            Self::Voice => "Voz e vídeo",
            Self::ScreenShare => "Compartilhamento de tela",
            Self::Behavior => "Comportamento",
        }
    }
}

/// Opens the window straight on `tab` (the voice popups' settings link).
pub fn open_at(app: &mut VesktopApp, tab: SettingsTab) {
    app.settings_tab = tab;
    app.settings_open = true;
}

pub fn show(app: &mut VesktopApp, ctx: &egui::Context) {
    let mut open = app.settings_open;
    let mut logged_out = false;
    egui::Window::new(RichText::new("Configurações").strong())
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .fixed_size([720.0, 460.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(180.0, 460.0),
                    egui::Layout::top_down_justified(egui::Align::Min),
                    |ui| nav(app, ui),
                );
                ui.separator();
                ui.vertical(|ui| {
                    ui.heading(app.settings_tab.label());
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| match app.settings_tab {
                            SettingsTab::Account => logged_out = account_settings(app, ui),
                            SettingsTab::Appearance => appearance_settings(app, ui),
                            SettingsTab::Voice => {
                                voice_settings(app, ui);
                                crate::ui::voice_processing::voice_processing_settings(app, ui);
                            }
                            SettingsTab::ScreenShare => screen_share_settings(app, ui),
                            SettingsTab::Behavior => behavior_settings(app, ui),
                        });
                });
            });
        });

    if (!open || logged_out) && app.settings_open {
        app.settings_open = false;
        app.settings.save();
    }
}

fn nav(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let groups: [(&str, &[SettingsTab]); 2] = [
        ("CONFIGURAÇÕES DE USUÁRIO", &[SettingsTab::Account]),
        (
            "CONFIGURAÇÕES DO APLICATIVO",
            &[
                SettingsTab::Appearance,
                SettingsTab::Voice,
                SettingsTab::ScreenShare,
                SettingsTab::Behavior,
            ],
        ),
    ];
    for (title, tabs) in groups {
        ui.label(RichText::new(title).small().strong().color(theme::MUTED));
        for &tab in tabs {
            if ui
                .selectable_label(app.settings_tab == tab, tab.label())
                .clicked()
            {
                app.settings_tab = tab;
            }
        }
        ui.add_space(12.0);
    }
    ui.label(
        RichText::new(format!(
            "Configurações salvas em {}",
            paths::settings_file().display()
        ))
        .small()
        .color(theme::MUTED),
    );
}

/// Who is signed in, and the logout button; true once logged out.
fn account_settings(app: &mut VesktopApp, ui: &mut egui::Ui) -> bool {
    if let Some(user) = &app.me {
        ui.label(format!("Conectado como {}", user.display_name()));
    }
    if ui
        .button("Sair (remover token deste dispositivo)")
        .clicked()
    {
        app.logout();
        return true;
    }
    false
}

fn appearance_settings(app: &mut VesktopApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label("Tema:");
        if ui
            .radio(app.settings.theme == Theme::Dark, "Escuro")
            .clicked()
        {
            app.settings.theme = Theme::Dark;
            app.settings.save();
        }
        if ui
            .radio(app.settings.theme == Theme::Light, "Claro")
            .clicked()
        {
            app.settings.theme = Theme::Light;
            app.settings.save();
        }
    });
    ui.horizontal(|ui| {
        ui.label("Zoom:");
        let response = ui.add(egui::Slider::new(&mut app.settings.zoom, 0.5..=2.0).step_by(0.05));
        if response.changed() {
            app.settings.save();
        }
    });
}

fn behavior_settings(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let mut tray = app.settings.tray;
    ui.add_enabled(
        false,
        egui::Checkbox::new(&mut tray, "Minimizar para a bandeja (em breve)"),
    );
    let mut updates = app.settings.check_for_updates;
    if ui
        .add(egui::Checkbox::new(
            &mut updates,
            "Procurar atualizações automaticamente (em breve)",
        ))
        .changed()
    {
        app.settings.check_for_updates = updates;
        app.settings.save();
    }
}

/// Device pickers, input sensitivity and noise suppression. Device changes
/// restart the audio streams live when in a call (docs/VOICE.md, phase 4).
fn voice_settings(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Saída:");
        changed |= device_combo(app, ui, true);
    });
    ui.horizontal(|ui| {
        ui.label("Entrada:");
        changed |= device_combo(app, ui, false);
    });
    changed |= volume_control(app, ui, true);
    changed |= volume_control(app, ui, false);
    changed |= input_controls(app, ui);
    if changed {
        apply_voice_change(app);
    }
}

/// The body of the user panel's chevron popups (docs/UI.md §5): device
/// submenu, the input controls under the mic, and the settings link.
pub fn voice_devices_menu(app: &mut VesktopApp, ui: &mut egui::Ui, output: bool) {
    let devices = crate::backend::audio::list_devices(output);
    let current = if output {
        &app.settings.output_device
    } else {
        &app.settings.input_device
    };
    let title = if output {
        "Dispositivo de saída"
    } else {
        "Dispositivo de entrada"
    };
    let mut changed = false;
    ui.label(RichText::new(title).small().strong().color(theme::MUTED));
    ui.menu_button(device_label(&devices, current), |ui| {
        changed |= device_choices(app, ui, &devices, output);
    });
    ui.separator();
    changed |= volume_control(app, ui, output);
    if !output {
        ui.separator();
        changed |= input_controls(app, ui);
    }
    if changed {
        apply_voice_change(app);
    }
    ui.separator();
    if ui.button("Configurações de voz").clicked() {
        open_at(app, SettingsTab::Voice);
        ui.close();
    }
}

fn device_combo(app: &mut VesktopApp, ui: &mut egui::Ui, output: bool) -> bool {
    let devices = crate::backend::audio::list_devices(output);
    let current = if output {
        &app.settings.output_device
    } else {
        &app.settings.input_device
    };
    let mut changed = false;
    egui::ComboBox::from_id_salt(if output { "voz_saida" } else { "voz_entrada" })
        .selected_text(device_label(&devices, current))
        .show_ui(ui, |ui| {
            changed = device_choices(app, ui, &devices, output);
        });
    changed
}

/// Pickers show the desktop's friendly names; the settings store the sound
/// server's device name.
fn device_label(devices: &[(String, String)], wanted: &Option<String>) -> String {
    match wanted {
        None => "Padrão do sistema".to_string(),
        Some(name) => devices
            .iter()
            .find(|(pulse_name, _)| pulse_name == name)
            .map(|(_, description)| description.clone())
            .unwrap_or_else(|| name.clone()),
    }
}

/// "Padrão do sistema" plus one row per device; true when the pick changed.
fn device_choices(
    app: &mut VesktopApp,
    ui: &mut egui::Ui,
    devices: &[(String, String)],
    output: bool,
) -> bool {
    let current = if output {
        &mut app.settings.output_device
    } else {
        &mut app.settings.input_device
    };
    let mut changed = false;
    if ui
        .selectable_label(current.is_none(), "Padrão do sistema")
        .clicked()
    {
        *current = None;
        changed = true;
    }
    for (name, description) in devices {
        if ui
            .selectable_label(current.as_deref() == Some(name), description)
            .clicked()
        {
            *current = Some(name.clone());
            changed = true;
        }
    }
    changed
}

fn volume_control(app: &mut VesktopApp, ui: &mut egui::Ui, output: bool) -> bool {
    let (volume, label) = if output {
        (&mut app.settings.output_volume, "Volume de saída")
    } else {
        (&mut app.settings.input_volume, "Volume de entrada")
    };
    ui.add(egui::Slider::new(volume, 0..=200).text(label).suffix("%"))
        .changed()
}

/// Sensitivity slider and noise suppression; true when either changed.
fn input_controls(app: &mut VesktopApp, ui: &mut egui::Ui) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Sensibilidade do microfone:");
        changed |= ui
            .add(
                egui::Slider::new(&mut app.settings.input_sensitivity, 0..=100)
                    .text("0 = sempre aberto"),
            )
            .changed();
    });
    changed |= ui
        .checkbox(
            &mut app.settings.noise_suppression,
            "Supressão de ruído (RNNoise)",
        )
        .changed();
    changed
}

fn apply_voice_change(app: &mut VesktopApp) {
    app.settings.save();
    app.push_voice_audio_config();
}

/// FockyTV as the share backend; off falls back to Discord's Go Live.
fn screen_share_settings(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let mut changed = ui
        .checkbox(
            &mut app.settings.fockytv_share,
            "Usar backend de compartilhamento do FockyTV",
        )
        .on_hover_text("Desligado: Go Live do Discord (experimental)")
        .changed();
    ui.add_enabled_ui(app.settings.fockytv_share, |ui| {
        ui.horizontal(|ui| {
            ui.label("Servidor:");
            changed |= ui
                .text_edit_singleline(&mut app.settings.fockytv_url)
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("Taxa de quadros:");
            for fps in [60, 30, 15] {
                if ui
                    .radio(app.settings.fockytv_fps == fps, format!("{fps} fps"))
                    .clicked()
                {
                    app.settings.fockytv_fps = fps;
                    changed = true;
                }
            }
        })
        .response
        .on_hover_text("Vale para a próxima transmissão");
        ui.horizontal(|ui| {
            ui.label("Nick:");
            let username = app
                .me
                .as_ref()
                .map(|me| me.username.clone())
                .unwrap_or_default();
            let mut nick = app.settings.fockytv_nick.clone().unwrap_or_default();
            if ui
                .add(egui::TextEdit::singleline(&mut nick).hint_text(username))
                .on_hover_text("A chave da transmissão no FockyTV; vazio = seu usuário do Discord")
                .changed()
            {
                app.settings.fockytv_nick = Some(nick).filter(|nick| !nick.trim().is_empty());
                changed = true;
            }
        });
    });
    if changed {
        app.settings.save();
    }
}
