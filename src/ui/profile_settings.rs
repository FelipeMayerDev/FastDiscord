//! Settings → Perfil: avatar, display name, the selected guild's nick,
//! pronouns and bio, beside a profile card preview. One background task
//! saves whatever changed: PATCH /users/@me (avatar, display name),
//! /users/@me/profile (pronouns, bio) and /guilds/{id}/members/@me (nick).

use base64::Engine;
use egui::{RichText, TextureHandle, Vec2};

use crate::app::VesktopApp;
use crate::backend::UiEvent;
use crate::backend::api::{Api, ApiError};
use crate::backend::events::ProfileEvent;
use crate::model::{Profile, User};
use crate::{theme, util};

/// Avatars go up center-cropped to a square of at most this side, as PNG:
/// the official client's cropper also re-encodes, and it keeps the request
/// small whatever the source (read_attachment caps that at 10 MB, Discord's
/// avatar upload limit).
const AVATAR_SIDE: u32 = 512;

#[derive(Default)]
pub struct ProfileEditor {
    /// What Discord has, once loaded; the draft is compared against it.
    loaded: Option<Profile>,
    /// The guild whose nick `loaded` holds (the selected one).
    guild: Option<String>,
    draft: Profile,
    /// A picked, not yet saved avatar: data URI and preview.
    avatar: Option<(String, egui::ColorImage)>,
    avatar_texture: Option<TextureHandle>,
    loading: bool,
    saving: bool,
    saved: bool,
    error: Option<String>,
}

pub fn handle_event(app: &mut VesktopApp, event: ProfileEvent) {
    let editor = &mut app.profile;
    match event {
        ProfileEvent::Loaded(result) => {
            editor.loading = false;
            match result {
                Ok(profile) => {
                    editor.draft = profile.clone();
                    editor.loaded = Some(profile);
                    editor.avatar = None;
                    editor.avatar_texture = None;
                }
                Err(error) => editor.error = Some(error),
            }
        }
        ProfileEvent::AvatarPicked(result) => {
            app.file_picker_open = false;
            match result {
                Ok(Some(avatar)) => {
                    editor.avatar = Some(avatar);
                    editor.avatar_texture = None;
                    editor.error = None;
                    editor.saved = false;
                }
                Ok(None) => {}
                Err(error) => editor.error = Some(error),
            }
        }
        ProfileEvent::Saved { user, error } => {
            editor.saving = false;
            if let Some(user) = user {
                editor.avatar = None;
                editor.avatar_texture = None;
                if let Some(loaded) = &mut editor.loaded {
                    loaded.global_name = user.global_name.clone().unwrap_or_default();
                }
                app.user_cache.insert(user.id.clone(), user.clone());
                app.me = Some(user);
            }
            match error {
                None => {
                    editor.loaded = Some(editor.draft.clone());
                    editor.saved = true;
                }
                Some(error) => editor.error = Some(error),
            }
        }
    }
}

pub fn profile_settings(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let Some(me) = app.me.clone() else {
        ui.label("Conecte-se para editar o perfil.");
        return;
    };
    let guild = app.selected_guild.clone();
    let editor = &app.profile;
    if !editor.loading
        && (editor.guild != guild || (editor.loaded.is_none() && editor.error.is_none()))
    {
        load(app, &me.id, guild);
    }
    if app.profile.loaded.is_none() {
        if let Some(error) = app.profile.error.clone() {
            ui.label(
                RichText::new(format!("Não foi possível carregar o perfil: {error}"))
                    .color(theme::RED),
            );
            if ui.button("Tentar de novo").clicked() {
                app.profile.error = None;
            }
        } else {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label("Carregando perfil…");
            });
        }
        return;
    }
    if ui.available_width() >= 560.0 {
        ui.columns(2, |columns| {
            form(app, &mut columns[0], &me);
            card(app, &mut columns[1], &me);
        });
    } else {
        card(app, ui, &me);
        ui.add_space(8.0);
        form(app, ui, &me);
    }
}

fn load(app: &mut VesktopApp, user_id: &str, guild: Option<String>) {
    let Some(token) = app.settings.token.clone() else {
        return;
    };
    app.profile.loading = true;
    app.profile.loaded = None;
    app.profile.error = None;
    app.profile.guild = guild.clone();
    let user_id = user_id.to_owned();
    let events = app.event_tx.clone();
    app.handle.spawn(async move {
        let result = Api::new(token)
            .own_profile(&user_id, guild.as_deref())
            .await
            .map_err(|error| error.to_string());
        events.send(UiEvent::Profile(ProfileEvent::Loaded(result)));
    });
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(theme::MUTED));
}

fn form(app: &mut VesktopApp, ui: &mut egui::Ui, me: &User) {
    field_label(ui, "AVATAR");
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!app.file_picker_open, egui::Button::new("Trocar avatar"))
            .on_hover_text("PNG, JPEG, WebP ou GIF de até 10 MB; recortado no centro")
            .clicked()
        {
            pick_avatar(app);
        }
        if app.profile.avatar.is_some() && ui.button("Desfazer").clicked() {
            app.profile.avatar = None;
            app.profile.avatar_texture = None;
        }
    });
    let editor = &mut app.profile;
    field_label(ui, "NOME EXIBIDO");
    ui.add(
        egui::TextEdit::singleline(&mut editor.draft.global_name)
            .char_limit(32)
            .hint_text(&me.username)
            .desired_width(f32::INFINITY),
    );
    if let Some(guild_id) = &editor.guild {
        let name = app
            .guilds
            .iter()
            .find(|guild| &guild.id == guild_id)
            .map_or("este servidor", |guild| guild.name.as_str());
        field_label(ui, &format!("APELIDO EM {}", name.to_uppercase()));
        ui.add(
            egui::TextEdit::singleline(&mut editor.draft.nick)
                .char_limit(32)
                .hint_text("Vazio = nome exibido")
                .desired_width(f32::INFINITY),
        );
    }
    field_label(ui, "PRONOMES");
    ui.add(
        egui::TextEdit::singleline(&mut editor.draft.pronouns)
            .char_limit(40)
            .desired_width(f32::INFINITY),
    );
    field_label(ui, "SOBRE MIM");
    ui.add(
        egui::TextEdit::multiline(&mut editor.draft.bio)
            .char_limit(190)
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );
    ui.label(
        RichText::new(format!("{}/190", editor.draft.bio.chars().count()))
            .small()
            .color(theme::MUTED),
    );
    let dirty = editor.avatar.is_some() || editor.loaded.as_ref() != Some(&editor.draft);
    if dirty {
        editor.saved = false;
    }
    let mut save_clicked = false;
    ui.horizontal(|ui| {
        save_clicked = ui
            .add_enabled(
                dirty && !editor.saving,
                egui::Button::new("Salvar alterações"),
            )
            .clicked();
        if ui
            .add_enabled(dirty && !editor.saving, egui::Button::new("Descartar"))
            .clicked()
        {
            editor.draft = editor.loaded.clone().unwrap_or_default();
            editor.avatar = None;
            editor.avatar_texture = None;
            editor.error = None;
        }
        if editor.saving {
            ui.add(egui::Spinner::new());
        }
    });
    if let Some(error) = &editor.error {
        ui.label(RichText::new(error).color(theme::RED));
    } else if editor.saved {
        ui.label(RichText::new("Perfil salvo.").color(theme::GREEN));
    }
    if save_clicked {
        save(app);
    }
}

/// Discord's profile popout, roughly: banner, ring-cut avatar, names,
/// pronouns and "Sobre mim", all from the draft.
fn card(app: &mut VesktopApp, ui: &mut egui::Ui, me: &User) {
    let editor = &mut app.profile;
    if let (Some((_, image)), None) = (&editor.avatar, &editor.avatar_texture) {
        editor.avatar_texture = Some(ui.ctx().load_texture(
            "profile_avatar",
            image.clone(),
            egui::TextureOptions::LINEAR,
        ));
    }
    let texture = editor.avatar_texture.clone().or_else(|| {
        app.images
            .get(ui.ctx(), &app.handle, &util::user_avatar_url(me))
    });
    let draft = &app.profile.draft;
    let name = Some(draft.global_name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or(&me.username);
    egui::Frame::new()
        .fill(theme::SIDEBAR)
        .corner_radius(theme::RADIUS_MD)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            let (banner, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
            let radius = theme::RADIUS_MD as u8;
            ui.painter().rect_filled(
                banner,
                egui::CornerRadius {
                    nw: radius,
                    ne: radius,
                    sw: 0,
                    se: 0,
                },
                util::name_color(name),
            );
            let avatar = egui::Rect::from_min_size(
                banner.left_bottom() + egui::vec2(16.0, -40.0),
                Vec2::splat(80.0),
            );
            ui.painter()
                .circle_filled(avatar.center(), 46.0, theme::SIDEBAR);
            ui.scope_builder(egui::UiBuilder::new().max_rect(avatar), |ui| {
                crate::ui::round_avatar(ui, texture.as_ref(), 80.0, name, util::name_color(name))
            });
            egui::Frame::new().inner_margin(16).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(name)
                        .size(20.0)
                        .family(theme::bold())
                        .color(theme::TEXT),
                );
                let mut handle = format!("@{}", me.username);
                if !draft.pronouns.trim().is_empty() {
                    handle = format!("{handle} • {}", draft.pronouns.trim());
                }
                ui.label(RichText::new(handle).color(theme::MUTED));
                if !draft.nick.trim().is_empty() {
                    ui.label(
                        RichText::new(format!("Apelido no servidor: {}", draft.nick.trim()))
                            .small()
                            .color(theme::MUTED),
                    );
                }
                if !draft.bio.trim().is_empty() {
                    ui.add_space(8.0);
                    field_label(ui, "SOBRE MIM");
                    ui.label(RichText::new(draft.bio.trim()).color(theme::TEXT));
                }
            });
        });
}

fn pick_avatar(app: &mut VesktopApp) {
    if app.file_picker_open {
        return;
    }
    app.file_picker_open = true;
    let events = app.event_tx.clone();
    app.handle.spawn(async move {
        let result = match crate::backend::file_picker::pick_file().await {
            Ok(Some(path)) => tokio::task::spawn_blocking(move || avatar_from_file(&path))
                .await
                .map_err(|error| error.to_string())
                .and_then(|avatar| avatar.map(Some)),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        events.send(UiEvent::Profile(ProfileEvent::AvatarPicked(result)));
    });
}

fn avatar_from_file(path: &std::path::Path) -> Result<(String, egui::ColorImage), String> {
    let (_, bytes) =
        crate::backend::api::read_attachment(path).map_err(|error| error.to_string())?;
    encode_avatar(&bytes)
}

/// Center square, at most [`AVATAR_SIDE`], as a PNG data URI plus its
/// preview. GIFs keep only their first frame (animated avatars are Nitro).
fn encode_avatar(bytes: &[u8]) -> Result<(String, egui::ColorImage), String> {
    let image = image::load_from_memory(bytes)
        .map_err(|_| "Escolha uma imagem PNG, JPEG, WebP ou GIF".to_string())?;
    let side = image.width().min(image.height());
    let mut square = image.crop_imm(
        (image.width() - side) / 2,
        (image.height() - side) / 2,
        side,
        side,
    );
    if side > AVATAR_SIDE {
        square = square.resize_exact(
            AVATAR_SIDE,
            AVATAR_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
    }
    let rgba = square.to_rgba8();
    let mut png = Vec::new();
    rgba.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    let uri = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    );
    let preview = egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    );
    Ok((uri, preview))
}

fn save(app: &mut VesktopApp) {
    let Some(token) = app.settings.token.clone() else {
        return;
    };
    let editor = &mut app.profile;
    let Some(loaded) = editor.loaded.clone() else {
        return;
    };
    for field in [
        &mut editor.draft.global_name,
        &mut editor.draft.nick,
        &mut editor.draft.pronouns,
        &mut editor.draft.bio,
    ] {
        *field = field.trim().to_owned();
    }
    let draft = editor.draft.clone();
    let avatar = editor.avatar.as_ref().map(|(uri, _)| uri.clone());
    let guild = editor.guild.clone();
    editor.saving = true;
    editor.saved = false;
    editor.error = None;
    let events = app.event_tx.clone();
    app.handle.spawn(async move {
        let api = Api::new(token);
        let mut user = None;
        let result = async {
            let mut body = serde_json::Map::new();
            if draft.global_name != loaded.global_name {
                // Empty clears it: Discord falls back to the username.
                let name = (!draft.global_name.is_empty()).then(|| draft.global_name.clone());
                body.insert("global_name".into(), name.into());
            }
            if let Some(avatar) = avatar {
                body.insert("avatar".into(), avatar.into());
            }
            if !body.is_empty() {
                user = Some(api.modify_me(&body.into()).await?);
            }
            if draft.pronouns != loaded.pronouns || draft.bio != loaded.bio {
                api.modify_profile(&draft.pronouns, &draft.bio).await?;
            }
            if let Some(guild_id) = guild.filter(|_| draft.nick != loaded.nick) {
                api.modify_nick(&guild_id, &draft.nick).await?;
            }
            Ok::<_, ApiError>(())
        }
        .await;
        events.send(UiEvent::Profile(ProfileEvent::Saved {
            user,
            error: result.err().map(|error| error.to_string()),
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_is_center_cropped_capped_and_encoded_as_png_data_uri() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(1200, 600, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (uri, preview) = encode_avatar(&png).unwrap();
        assert_eq!(preview.size, [512, 512]);
        let data = uri.strip_prefix("data:image/png;base64,").unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (512, 512));
        assert!(encode_avatar(b"not an image").is_err());
    }
}
