//! Unicode emoji as Twemoji images, like Discord: epaint can't draw color
//! fonts, and the monochrome fallback misses ZWJ sequences, flags, keycaps
//! and skin tones. Text runs stay labels; the font's glyph shows while an
//! image loads or if it fails.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, FontSelection, Image, Label, Response, RichText, Ui, Vec2};

use crate::app::VesktopApp;

/// jdecked's fork, the one Discord ships; pinned so file names never shift.
const TWEMOJI: &str = "https://cdn.jsdelivr.net/gh/jdecked/twemoji@17.0.3/assets/72x72";

/// Longest emoji sequence, in chars (a kiss with two skin tones is 10).
const MAX_CHARS: usize = 10;

#[derive(Debug, PartialEq)]
pub enum Piece<'a> {
    Text(&'a str),
    Emoji(&'static emojis::Emoji),
}

/// `text` cut into plain runs and emoji, longest sequence first.
pub fn split(text: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let (mut start, mut i) = (0, 0);
    while let Some(first) = text[i..].chars().next() {
        match emoji_at(&text[i..]) {
            Some((len, emoji)) => {
                if start < i {
                    pieces.push(Piece::Text(&text[start..i]));
                }
                pieces.push(Piece::Emoji(emoji));
                i += len;
                start = i;
            }
            None => i += first.len_utf8(),
        }
    }
    if start < text.len() {
        pieces.push(Piece::Text(&text[start..]));
    }
    pieces
}

fn emoji_at(rest: &str) -> Option<(usize, &'static emojis::Emoji)> {
    let first = rest.chars().next()?;
    // Keycap bases, ©, ® and everything from ‼ up; the rest is plain text.
    if !matches!(first, '#' | '*' | '0'..='9' | '©' | '®' | '\u{203c}'..) {
        return None;
    }
    let ends: Vec<usize> = rest
        .char_indices()
        .map(|(index, c)| index + c.len_utf8())
        .take(MAX_CHARS)
        .collect();
    let (end, emoji) = ends
        .into_iter()
        .rev()
        .find_map(|end| emojis::get(&rest[..end]).map(|emoji| (end, emoji)))?;
    // A bare text-style symbol (©, ™, ↔…) stays text unless VS16 asks for
    // the emoji, as in Discord.
    (end > first.len_utf8() || first > '\u{218f}').then_some((end, emoji))
}

/// Twemoji's file name: lowercase hex code points joined by `-`, VS16
/// dropped unless the sequence has a ZWJ (👁‍🗨 drops it anyway).
pub fn twemoji_url(emoji: &emojis::Emoji) -> String {
    let text = emoji.as_str();
    let keep_vs16 = text.contains('\u{200d}') && !text.starts_with('\u{1f441}');
    let name: Vec<String> = text
        .chars()
        .filter(|&c| keep_vs16 || c != '\u{fe0f}')
        .map(|c| format!("{:x}", c as u32))
        .collect();
    format!("{TWEMOJI}/{}.png", name.join("-"))
}

/// A server's custom emoji at 48 px; animated ones as GIF, which the image
/// cache plays.
pub fn custom_url(id: &str, animated: bool) -> String {
    let ext = if animated { "gif" } else { "webp" };
    format!("https://cdn.discordapp.com/emojis/{id}.{ext}?size=48")
}

/// A `<:name:id>` / `<a:name:id>` token's `:name:` and image URL.
pub fn custom(token: &str) -> Option<(String, String)> {
    let inner = token.strip_prefix('<')?.strip_suffix('>')?;
    let (head, id) = inner.rsplit_once(':')?;
    let (kind, name) = head.split_once(':')?;
    Some((format!(":{name}:"), custom_url(id, kind == "a")))
}

/// The emoji at `url`, `size` points square; `alt` in `format` while it
/// loads or if it fails.
pub fn image(
    app: &mut VesktopApp,
    ui: &mut Ui,
    url: &str,
    alt: &str,
    format: &TextFormat,
    size: f32,
) -> Response {
    match app.images.get(ui.ctx(), &app.handle, url) {
        Some(texture) => ui.add(Image::from_texture(&texture).fit_to_exact_size(Vec2::splat(size))),
        None => ui.add(
            Label::new(LayoutJob::single_section(alt.to_string(), format.clone()))
                .selectable(false),
        ),
    }
}

/// A one-line label whose emoji are Twemoji images at the font's row
/// height; elided to the available width when `truncate`. Text without
/// emoji is the very same `Label` as before.
pub fn label(app: &mut VesktopApp, ui: &mut Ui, text: RichText, truncate: bool) -> Response {
    let label = |label: Label| {
        let label = label.selectable(false);
        if truncate { label.truncate() } else { label }
    };
    let source = text.text().to_string();
    let pieces = split(&source);
    if !pieces.iter().any(|piece| matches!(piece, Piece::Emoji(_))) {
        return ui.add(label(Label::new(text)));
    }
    let job = egui::WidgetText::from(text).into_layout_job(
        ui.style(),
        FontSelection::Default,
        Align::Center,
    );
    let format = job.sections[0].format.clone();
    let size = ui.fonts_mut(|fonts| fonts.row_height(&format.font_id));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for piece in pieces {
            if truncate && ui.available_width() < size {
                break;
            }
            match piece {
                Piece::Text(text) => {
                    ui.add(label(Label::new(LayoutJob::single_section(
                        text.to_string(),
                        format.clone(),
                    ))));
                }
                Piece::Emoji(emoji) => {
                    image(app, ui, &twemoji_url(emoji), emoji.as_str(), &format, size);
                }
            }
        }
    })
    .response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<String> {
        split(text)
            .into_iter()
            .map(|piece| match piece {
                Piece::Text(text) => text.to_string(),
                Piece::Emoji(emoji) => twemoji_url(emoji)[TWEMOJI.len() + 1..]
                    .trim_end_matches(".png")
                    .to_string(),
            })
            .collect()
    }

    #[test]
    fn splits_sequences_and_names_twemoji_files() {
        assert_eq!(names("📢・avisos"), ["1f4e2", "・avisos"]);
        // VS16 dropped outside ZWJ sequences, unqualified forms resolved.
        assert_eq!(names("❤️ e ❤"), ["2764", " e ", "2764"]);
        assert_eq!(
            names("🏳️‍🌈👍🏽🇧🇷1️⃣"),
            [
                "1f3f3-fe0f-200d-1f308",
                "1f44d-1f3fd",
                "1f1e7-1f1f7",
                "31-20e3"
            ]
        );
        assert_eq!(
            names("🧔‍♂️ 👁️‍🗨️"),
            ["1f9d4-200d-2642-fe0f", " ", "1f441-200d-1f5e8"]
        );
        // Plain text, bare digits and text-style symbols stay text.
        assert_eq!(names("olá #1 © ™ ©️"), ["olá #1 © ™ ", "a9"]);
        assert!(split("").is_empty());
        assert_eq!(
            custom("<a:party:42>"),
            Some((
                ":party:".into(),
                "https://cdn.discordapp.com/emojis/42.gif?size=48".into()
            ))
        );
    }
}
