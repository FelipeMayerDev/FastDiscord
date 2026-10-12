//! The voice action row's soundboard picker (#12) and the DJ window (#15).

use std::collections::{HashMap, HashSet};

use egui::RichText;

use crate::app::{VesktopApp, VoiceConn};
use crate::backend::Command;
use crate::backend::audio::Sound;
use crate::backend::dj::{Action, DjClient};
use crate::backend::soundboard::SoundboardSound;
use crate::model::VoiceState;
use crate::theme;

#[derive(Default)]
pub struct VoiceExtras {
    pub soundboard_open: bool,
    /// Successfully loaded sounds per guild, including genuinely empty lists.
    pub sounds: HashMap<String, Vec<SoundboardSound>>,
    pub soundboard_loading: HashSet<String>,
    pub soundboard_errors: HashMap<String, String>,
    pub soundboard_error: Option<String>,
    pub dj: DjClient,
    pub dj_collapsed: bool,
    pub music_open: bool,
    pub music_input: String,
}

impl VoiceExtras {
    fn begin_soundboard_load(&mut self, guild_id: &str) -> bool {
        !self.sounds.contains_key(guild_id)
            && !self.soundboard_errors.contains_key(guild_id)
            && self.soundboard_loading.insert(guild_id.to_string())
    }
}

pub fn show(app: &mut VesktopApp, ctx: &egui::Context) {
    if !matches!(app.voice, VoiceConn::Connected { .. }) {
        return;
    }
    if app.extras.music_open {
        music_window(app, ctx);
    }
}

/// The soundboard popover, anchored above its button in the voice panel
/// like the chat's emoji and GIF pickers.
pub fn soundboard(app: &mut VesktopApp, button: &egui::Response) {
    let VoiceConn::Connected {
        guild_id,
        channel_id,
    } = app.voice.clone()
    else {
        return;
    };
    let (guild_id, channel_id) = (guild_id.as_str(), channel_id.as_str());
    if button.clicked() {
        app.extras.soundboard_open = !app.extras.soundboard_open;
    }
    if app.extras.soundboard_open && app.extras.begin_soundboard_load(guild_id) {
        app.send(Command::LoadSoundboard {
            guild_id: guild_id.to_string(),
        });
    }
    let mut open = app.extras.soundboard_open;
    let mut play = None;
    egui::Popup::from_response(button)
        .id(egui::Id::new("soundboard_picker"))
        .open_bool(&mut open)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::TOP_START)
        .gap(8.0)
        .show(|ui| {
            ui.set_width(320.0);
            ui.label(RichText::new("Soundboard").strong());
            if let Some(error) = &app.extras.soundboard_error {
                ui.label(RichText::new(error).small().color(theme::RED));
            }
            if let Some(error) = app.extras.soundboard_errors.get(guild_id) {
                ui.label(RichText::new(error).small().color(theme::RED));
                if ui.button("Tentar de novo").clicked() {
                    app.extras.soundboard_errors.remove(guild_id);
                    app.extras.soundboard_loading.insert(guild_id.to_string());
                    app.send(Command::LoadSoundboard {
                        guild_id: guild_id.to_string(),
                    });
                }
                return;
            }
            if app.extras.soundboard_loading.contains(guild_id) {
                ui.label(RichText::new("Carregando sons…").color(theme::MUTED));
                return;
            }
            let Some(sounds) = app.extras.sounds.get(guild_id) else {
                return;
            };
            if !sounds.iter().any(|sound| sound.available) {
                ui.label(
                    RichText::new("Nenhum som disponível neste servidor.").color(theme::MUTED),
                );
                return;
            }
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for sound in sounds.iter().filter(|sound| sound.available) {
                            let label = match &sound.emoji_name {
                                Some(emoji) => format!("{emoji} {}", sound.name),
                                None => sound.name.clone(),
                            };
                            if ui.button(label).clicked() {
                                play = Some(sound.clone());
                            }
                        }
                    });
                });
        });
    app.extras.soundboard_open = open;
    if let Some(sound) = play {
        app.extras.soundboard_error = None;
        app.send(Command::SendSoundboard {
            channel_id: channel_id.to_string(),
            sound_id: sound.sound_id,
            source_guild_id: sound.guild_id,
        });
    }
}

/// Fixed heights keep the voice/user controls anchored at the bottom.
pub fn music_height(app: &VesktopApp) -> f32 {
    if !matches!(app.voice, VoiceConn::Connected { .. }) {
        return 0.0;
    }
    let view = app.extras.dj.view.lock().unwrap();
    if !view.connected || view.session.as_ref().is_none_or(|s| s.tracks.is_empty()) {
        return 0.0;
    }
    if app.extras.dj_collapsed { 32.0 } else { 150.0 }
}

pub fn music_panel(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let height = music_height(app);
    if height == 0.0 {
        return;
    }
    let width = ui.available_width();
    let collapsed = app.extras.dj_collapsed;
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_height(height);
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 5.0);
            let (session, clock, connected) = {
                let view = app.extras.dj.view.lock().unwrap();
                (view.session.clone(), view.clock.clone(), view.connected)
            };
            let Some(session) = session else {
                return;
            };
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if ui
                    .selectable_label(
                        false,
                        if app.extras.dj_collapsed {
                            "▸ DJ compartilhado"
                        } else {
                            "▾ DJ compartilhado"
                        },
                    )
                    .clicked()
                {
                    app.extras.dj_collapsed = !app.extras.dj_collapsed;
                }
                ui.label(
                    RichText::new(format!("{} no DJ", session.participants))
                        .small()
                        .color(theme::MUTED),
                );
            });
            if collapsed {
                return;
            }
            ui.add_enabled_ui(connected, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let title = session.current().map_or("DJ parado", |t| t.title.as_str());
                    ui.add(egui::Label::new(RichText::new(title).strong().size(12.0)).truncate())
                        .on_hover_text(title);
                });
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let (loading, error) = {
                        let state = app.extras.dj.music.state.lock().unwrap();
                        (state.loading, state.error.clone())
                    };
                    let position = clock.as_ref().map_or(0, |c| session.position(c.now()));
                    let duration = session.current().map_or(0, |t| t.duration_ms);
                    ui.label(
                        RichText::new(if error.is_some() {
                            "Falha no áudio; abra a playlist".into()
                        } else if session.preparing {
                            format!(
                                "Baixando: {}/{} prontos",
                                session.ready_count, session.participants
                            )
                        } else if loading {
                            "Preparando reprodução…".into()
                        } else {
                            format!("{} / {}", time_label(position), time_label(duration))
                        })
                        .small()
                        .color(if error.is_some() {
                            theme::RED
                        } else {
                            theme::MUTED
                        }),
                    )
                    .on_hover_text(error.unwrap_or_default());
                });
                ui.horizontal(|ui| {
                    use crate::ui::icons::{self, Icon};
                    ui.add_space(8.0);
                    let controls = [
                        (Icon::Previous, "Voltar", Action::Previous),
                        (
                            if session.paused || session.current().is_none() {
                                Icon::Play
                            } else {
                                Icon::Pause
                            },
                            if session.paused || session.current().is_none() {
                                "Reproduzir"
                            } else {
                                "Pausar"
                            },
                            if session.paused || session.current().is_none() {
                                Action::Play
                            } else {
                                Action::Pause
                            },
                        ),
                        (Icon::Next, "Próxima", Action::Next),
                        (Icon::Stop, "Parar para todos", Action::Stop),
                    ];
                    for (icon, label, action) in controls {
                        if icons::button(ui, icon, egui::vec2(30.0, 28.0), false, label).clicked() {
                            app.extras.dj.action(action);
                        }
                    }
                    if icons::button(
                        ui,
                        Icon::Playlist,
                        egui::vec2(30.0, 28.0),
                        app.extras.music_open,
                        "Ver playlist e adicionar músicas",
                    )
                    .clicked()
                    {
                        app.extras.music_open = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.spacing_mut().slider_width = (width - 120.0).max(40.0);
                if ui
                    .add(
                        egui::Slider::new(&mut app.settings.dj_volume, 0..=200)
                            .text("Volume")
                            .suffix("%"),
                    )
                    .on_hover_text("Volume do DJ só para você")
                    .changed()
                {
                    app.extras.dj.music.set_volume(app.settings.dj_volume);
                    app.settings.save();
                }
            });
            ui.add_space(4.0);
            ui.separator();
        },
    );
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(100));
}
fn time_label(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn music_window(app: &mut VesktopApp, ctx: &egui::Context) {
    let response = egui::Modal::new(egui::Id::new("dj_playlist"))
        .backdrop_color(egui::Color32::from_black_alpha(160))
        .frame(
            egui::Frame::popup(&ctx.global_style())
                .inner_margin(20)
                .corner_radius(12),
        )
        .show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 80.0).clamp(200.0, 500.0));
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            ui.horizontal(|ui| {
                ui.heading("Playlist do DJ");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Fechar").clicked() {
                        app.extras.music_open = false;
                    }
                });
            });
            ui.label(
                RichText::new("Compartilhada com os usuários do FastDiscord nesta sala")
                    .small()
                    .color(theme::MUTED),
            );
            let (session, connected, resolving, results, error) = {
                let view = app.extras.dj.view.lock().unwrap();
                (
                    view.session.clone(),
                    view.connected,
                    view.resolving,
                    view.search_results.clone(),
                    view.error.clone(),
                )
            };
            if let Some(error) = error {
                ui.label(RichText::new(error).color(theme::RED));
            }
            if let Some(error) = app.extras.dj.music.state.lock().unwrap().error.clone() {
                ui.label(RichText::new(error).color(theme::RED));
            }
            if !connected {
                ui.label("Conectando ao DJ…");
            }
            ui.separator();
            let field = ui.add(
                egui::TextEdit::singleline(&mut app.extras.music_input)
                    .hint_text("Link do YouTube ou nome da música")
                    .desired_width(f32::INFINITY)
                    .char_limit(2048),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let add = ui
                .add_enabled(
                    connected && !resolving && !app.extras.music_input.trim().is_empty(),
                    egui::Button::new(if resolving {
                        "Buscando música…"
                    } else if app.extras.music_input.contains("://") {
                        "Adicionar à playlist"
                    } else {
                        "Pesquisar músicas"
                    }),
                )
                .clicked();
            if (add || entered && connected && !resolving)
                && app
                    .extras
                    .dj
                    .enqueue(&app.extras.music_input, &app.handle, ctx)
            {
                app.extras.music_input.clear();
            }
            egui::ScrollArea::vertical()
                .id_salt("dj_tracks")
                .max_height((ctx.content_rect().height() - 300.0).clamp(80.0, 320.0))
                .show(ui, |ui| {
                    if !results.is_empty() {
                        ui.label(
                            RichText::new("Resultados da busca — selecione a música").strong(),
                        );
                        for track in &results {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    egui::vec2((ui.available_width() - 130.0).max(40.0), 24.0),
                                    egui::Label::new(&track.title).truncate(),
                                )
                                .on_hover_text(format!(
                                    "{}\n{}",
                                    track.title,
                                    track.url()
                                ));
                                ui.label(
                                    RichText::new(time_label(track.duration_ms))
                                        .small()
                                        .color(theme::MUTED),
                                );
                                if ui
                                    .add_enabled(
                                        connected && !resolving,
                                        egui::Button::new("Adicionar"),
                                    )
                                    .clicked()
                                {
                                    app.extras.dj.enqueue(&track.url(), &app.handle, ctx);
                                }
                            });
                        }
                        ui.separator();
                    }
                    ui.label(RichText::new("Playlist da sala").strong());
                    let Some(session) = &session else {
                        return;
                    };
                    if session.tracks.is_empty() {
                        ui.label(
                            RichText::new("Adicione músicas para iniciar o DJ.")
                                .color(theme::MUTED),
                        );
                    }
                    for (index, track) in session.tracks.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let active = session.index == Some(index);
                            ui.add_sized(
                                egui::vec2((ui.available_width() - 58.0).max(40.0), 22.0),
                                egui::Label::new(
                                    RichText::new(format!("{}. {}", index + 1, track.title))
                                        .color(if active { theme::GREEN } else { theme::TEXT }),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&track.title);
                            ui.label(
                                RichText::new(time_label(track.duration_ms))
                                    .small()
                                    .color(theme::MUTED),
                            );
                        });
                    }
                });
            ui.separator();
            ui.label(
                RichText::new("Todos podem controlar. O volume é individual.")
                    .small()
                    .color(theme::MUTED),
            );
        });
    if response.should_close() {
        app.extras.music_open = false;
    }
}

/// The tone for another member's voice-state change, seen from our
/// channel: arriving, leaving, or starting/stopping a stream in it.
pub fn roster_sound(my_channel: &str, old: Option<&VoiceState>, new: &VoiceState) -> Option<Sound> {
    let here = |state: &VoiceState| state.channel_id.as_deref() == Some(my_channel);
    let was_here = old.is_some_and(here);
    let streamed = old.is_some_and(|old| was_here && old.self_stream);
    match (was_here, here(new)) {
        (false, true) => Some(Sound::Join),
        (true, false) => Some(Sound::Leave),
        (true, true) if !streamed && new.self_stream => Some(Sound::StreamStart),
        (true, true) if streamed && !new.self_stream => Some(Sound::StreamStop),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(channel: Option<&str>, self_stream: bool) -> VoiceState {
        VoiceState {
            channel_id: channel.map(str::to_string),
            self_stream,
            ..VoiceState::default()
        }
    }

    #[test]
    fn soundboard_load_is_deduplicated_and_empty_success_is_final() {
        let mut extras = VoiceExtras::default();
        assert!(extras.begin_soundboard_load("a"));
        assert!(!extras.begin_soundboard_load("a"));
        extras.soundboard_loading.remove("a");
        extras
            .soundboard_errors
            .insert("a".into(), "offline".into());
        assert!(!extras.begin_soundboard_load("a"));
        // Explicit retry permits a new request, without retrying every frame.
        extras.soundboard_errors.remove("a");
        assert!(extras.begin_soundboard_load("a"));
        extras.soundboard_loading.remove("a");
        extras.sounds.insert("a".into(), Vec::new());
        assert!(!extras.begin_soundboard_load("a"));
        assert!(extras.begin_soundboard_load("b"));
    }

    #[test]
    fn roster_changes_map_to_tones() {
        let here = state(Some("c"), false);
        let live = state(Some("c"), true);
        let away = state(Some("x"), false);
        let gone = state(None, false);
        assert!(matches!(roster_sound("c", None, &here), Some(Sound::Join)));
        assert!(matches!(
            roster_sound("c", Some(&away), &here),
            Some(Sound::Join)
        ));
        assert!(matches!(
            roster_sound("c", Some(&here), &gone),
            Some(Sound::Leave)
        ));
        assert!(matches!(
            roster_sound("c", Some(&here), &live),
            Some(Sound::StreamStart)
        ));
        assert!(matches!(
            roster_sound("c", Some(&live), &here),
            Some(Sound::StreamStop)
        ));
        assert!(roster_sound("c", Some(&here), &here).is_none());
        assert!(roster_sound("c", Some(&away), &gone).is_none());
    }
}
