//! The voice action row's soundboard picker (#12) and the DJ window (#15).

use std::collections::{HashMap, HashSet};

use egui::RichText;

use crate::app::{VesktopApp, VoiceConn};
use crate::backend::Command;
use crate::backend::audio::Sound;
use crate::backend::music::{self, Music};
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
    pub music: Music,
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
    let VoiceConn::Connected {
        guild_id,
        channel_id,
    } = app.voice.clone()
    else {
        return;
    };
    if app.extras.soundboard_open {
        soundboard(app, ctx, &guild_id, &channel_id);
    }
    if app.extras.music_open {
        music_window(app, ctx);
    }
}

fn soundboard(app: &mut VesktopApp, ctx: &egui::Context, guild_id: &str, channel_id: &str) {
    if app.extras.begin_soundboard_load(guild_id) {
        app.send(Command::LoadSoundboard {
            guild_id: guild_id.to_string(),
        });
    }
    let mut open = true;
    let mut play = None;
    egui::Window::new(RichText::new("Soundboard").strong())
        .open(&mut open)
        .collapsible(false)
        .default_width(320.0)
        .show(ctx, |ui| {
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

fn music_window(app: &mut VesktopApp, ctx: &egui::Context) {
    let mut open = true;
    let extras = &mut app.extras;
    egui::Window::new(RichText::new("DJ").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut extras.music_input)
                        .hint_text("Link ou busca")
                        .desired_width(240.0),
                );
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Tocar").clicked() || entered {
                    extras.music.enqueue(&extras.music_input);
                    extras.music_input.clear();
                }
            });
            let (now, paused, queue, error) = {
                let state = extras.music.state.lock().unwrap();
                (
                    state.now.clone(),
                    state.paused,
                    state
                        .queue
                        .iter()
                        .map(|t| music::label(t))
                        .collect::<Vec<_>>(),
                    state.error.clone(),
                )
            };
            if let Some(error) = error {
                ui.label(RichText::new(error).small().color(theme::RED));
            }
            ui.add_space(4.0);
            match &now {
                Some(title) => {
                    ui.label(RichText::new(format!("▶ {title}")).strong());
                    ui.horizontal(|ui| {
                        if ui
                            .button(if paused { "Continuar" } else { "Pausar" })
                            .clicked()
                        {
                            extras.music.toggle_pause();
                        }
                        if ui.button("Pular").clicked() {
                            extras.music.skip();
                        }
                        if ui.button("Parar").clicked() {
                            extras.music.stop();
                        }
                    });
                }
                None => {
                    ui.label(RichText::new("Nada tocando.").color(theme::MUTED));
                }
            }
            let mut volume = *extras.music.volume.lock().unwrap() * 100.0;
            if ui
                .add(
                    egui::Slider::new(&mut volume, 0.0..=100.0)
                        .text("Volume")
                        .suffix("%"),
                )
                .changed()
            {
                *extras.music.volume.lock().unwrap() = volume / 100.0;
            }
            if !queue.is_empty() {
                ui.separator();
                ui.label(RichText::new("Na fila").small().color(theme::MUTED));
                for (index, item) in queue.iter().enumerate() {
                    ui.label(format!("{}. {item}", index + 1));
                }
            }
        });
    app.extras.music_open = open;
    // The title and queue change from the DJ thread.
    if app.extras.music.state.lock().unwrap().now.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
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
