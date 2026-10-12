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
    Profile,
    Appearance,
    Voice,
    ScreenShare,
    Behavior,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            Self::Account => "Minha conta",
            Self::Profile => "Perfil",
            Self::Appearance => "Aparência",
            Self::Voice => "Voz e áudio",
            Self::ScreenShare => "Tela e transmissão",
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
    let size = (ctx.content_rect().size() - egui::vec2(80.0, 80.0)).min(egui::vec2(840.0, 580.0));
    let compact = size.x < 680.0;
    let mut close = false;
    let mut logged_out = false;
    let response = egui::Modal::new(egui::Id::new("settings_modal"))
        .backdrop_color(egui::Color32::from_black_alpha(160))
        .frame(
            egui::Frame::popup(&ctx.global_style())
                .inner_margin(24)
                .corner_radius(16),
        )
        .show(ctx, |ui| {
            ui.set_width(size.x);
            ui.set_height(size.y);
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Configurações").size(24.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui
                        .button("Fechar")
                        .on_hover_text("Fechar configurações (Esc)")
                        .clicked();
                });
            });
            ui.separator();
            if compact {
                ui.horizontal_wrapped(|ui| {
                    for tab in [
                        SettingsTab::Account,
                        SettingsTab::Profile,
                        SettingsTab::Appearance,
                        SettingsTab::Voice,
                        SettingsTab::ScreenShare,
                        SettingsTab::Behavior,
                    ] {
                        if ui
                            .selectable_label(app.settings_tab == tab, tab.label())
                            .clicked()
                        {
                            app.settings_tab = tab;
                        }
                    }
                });
                ui.separator();
            }
            let body_height = (ui.available_height() - 32.0).max(100.0);
            ui.horizontal_top(|ui| {
                if !compact {
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .inner_margin(12)
                        .corner_radius(10)
                        .show(ui, |ui| {
                            ui.set_width(176.0);
                            ui.set_height(body_height - 24.0);
                            ui.with_layout(
                                egui::Layout::top_down_justified(egui::Align::Min),
                                |ui| nav(app, ui),
                            );
                        });
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), body_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.heading(app.settings_tab.label());
                        egui::ScrollArea::vertical()
                            .id_salt(("settings_page", app.settings_tab.label()))
                            .auto_shrink(false)
                            .max_height(ui.available_height())
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                match app.settings_tab {
                                    SettingsTab::Account => logged_out = account_settings(app, ui),
                                    SettingsTab::Profile => {
                                        crate::ui::profile_settings::profile_settings(app, ui)
                                    }
                                    SettingsTab::Appearance => appearance_settings(app, ui),
                                    SettingsTab::Voice => {
                                        ui.label(RichText::new("Dispositivos e volumes").strong());
                                        voice_settings(app, ui);
                                        ui.separator();
                                        ui.label(
                                            RichText::new("Processamento do microfone").strong(),
                                        );
                                        crate::ui::voice_processing::voice_processing_settings(
                                            app, ui,
                                        );
                                    }
                                    SettingsTab::ScreenShare => screen_share_settings(app, ui),
                                    SettingsTab::Behavior => behavior_settings(app, ui),
                                }
                            });
                    },
                );
            });
            ui.separator();
            ui.label(
                RichText::new("As alterações são salvas automaticamente.")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            )
            .on_hover_text(format!(
                "Arquivo de configurações: {}",
                paths::settings_file().display()
            ));
        });

    if (close || response.should_close() || logged_out) && app.settings_open {
        app.settings_open = false;
        app.settings.save();
    }
}

fn nav(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let groups: [(&str, &[SettingsTab]); 2] = [
        (
            "CONFIGURAÇÕES DE USUÁRIO",
            &[SettingsTab::Account, SettingsTab::Profile],
        ),
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
        ui.label(
            RichText::new(title)
                .small()
                .strong()
                .color(ui.visuals().weak_text_color()),
        );
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
}

/// Who is signed in, and the logout button; true once logged out.
fn account_settings(app: &mut VesktopApp, ui: &mut egui::Ui) -> bool {
    if let Some(user) = &app.me {
        egui::Frame::group(ui.style())
            .inner_margin(16)
            .corner_radius(10)
            .show(ui, |ui| {
                ui.label(RichText::new(user.display_name()).size(20.0).strong());
                ui.label(
                    RichText::new(format!("@{}", user.username))
                        .color(ui.visuals().weak_text_color()),
                );
            });
    }
    ui.add_space(8.0);
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
    ui.horizontal_wrapped(|ui| {
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
    ui.horizontal_wrapped(|ui| {
        ui.label("Zoom:");
        let response = ui.add(egui::Slider::new(&mut app.settings.zoom, 0.5..=2.0).step_by(0.05));
        if response.changed() {
            app.settings.save();
        }
    });
    if ui
        .checkbox(
            &mut app.settings.reduce_motion,
            "Reduzir movimento da interface e GIFs",
        )
        .changed()
    {
        app.settings.save();
        ui.ctx().data_mut(|data| {
            data.insert_temp(egui::Id::new("reduce_motion"), app.settings.reduce_motion)
        });
    }
}

fn behavior_settings(app: &mut VesktopApp, ui: &mut egui::Ui) {
    if ui
        .add_enabled(
            cfg!(feature = "tray"),
            egui::Checkbox::new(
                &mut app.settings.tray,
                "Mostrar ícone na bandeja do sistema",
            ),
        )
        .changed()
    {
        app.settings.save();
        app.configure_tray(ui.ctx());
    }
    let available = app.tray_available();
    if ui
        .add_enabled(
            available,
            egui::Checkbox::new(
                &mut app.settings.minimize_to_tray,
                "Manter em execução na bandeja ao fechar a janela",
            ),
        )
        .changed()
    {
        app.settings.save();
    }
    if app.settings.tray && !available {
        ui.weak("Manter na bandeja está indisponível neste ambiente. Fechar a janela encerra o aplicativo.");
        ui.weak("Se acabou de ativar a bandeja no Linux, reinicie o aplicativo.");
    }
    let mut updates = app.settings.check_for_updates;
    if ui
        .add(egui::Checkbox::new(
            &mut updates,
            "Atualizar automaticamente (vale ao reiniciar)",
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
    ui.horizontal_wrapped(|ui| {
        ui.label("Saída:");
        changed |= device_combo(app, ui, true);
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Entrada:");
        changed |= device_combo(app, ui, false);
    });
    changed |= volume_control(app, ui, true);
    changed |= volume_control(app, ui, false);
    changed |= input_controls(app, ui);
    if changed {
        apply_voice_change(app);
    }
    ui.separator();
    ui.heading("DJ compartilhado");
    ui.label("Todos na sala devem usar o mesmo servidor e chave do DJ.");
    ui.label(
        RichText::new("A conexão será atualizada ao fechar as configurações.")
            .small()
            .color(theme::MUTED),
    );
    ui.label("Servidor (wss://):");
    let url = ui.add(
        egui::TextEdit::singleline(&mut app.settings.dj_url)
            .hint_text("wss://dj.seudominio.com")
            .desired_width(f32::INFINITY),
    );
    ui.label("Chave do servidor DJ (não é o token do Discord):");
    let key = ui.add(
        egui::TextEdit::singleline(&mut app.settings.dj_key)
            .password(true)
            .desired_width(f32::INFINITY),
    );
    if url.changed() || key.changed() {
        app.settings.save();
    }
    if !app.settings.dj_url.is_empty()
        && let Err(error) =
            crate::backend::dj::validate_endpoint(&app.settings.dj_url, &app.settings.dj_key)
    {
        ui.label(RichText::new(error).small().color(theme::RED));
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
    ui.label("Sensibilidade do microfone:");
    changed |= ui
        .add(
            egui::Slider::new(&mut app.settings.input_sensitivity, 0..=100)
                .text("0 = sempre aberto"),
        )
        .changed();
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
        ui.horizontal_wrapped(|ui| {
            ui.label("Servidor:");
            changed |= ui
                .text_edit_singleline(&mut app.settings.fockytv_url)
                .changed();
        });
        ui.horizontal_wrapped(|ui| {
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
        ui.horizontal_wrapped(|ui| {
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
        ui.label(
            RichText::new("Nick: até 32 letras sem acentos, números, _, - ou .; sem espaços.")
                .color(theme::MUTED),
        );
        if let Err(error) = crate::backend::fockytv::validate_stream_key(&app.fockytv_nick()) {
            ui.label(RichText::new(error).color(theme::RED));
        }
    });
    if changed {
        app.settings.save();
    }
}
