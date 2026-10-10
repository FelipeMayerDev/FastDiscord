//! Discord visual-refresh dark palette, resolved to hex from the official
//! web client's served CSS (the `:root` + `.theme-dark` token blocks in
//! discord.com/assets/952007-*.css, HSL indirections included), so the
//! native client matches it exactly. Applied through `egui::Visuals` so
//! every standard widget picks it up without custom frames.

use crate::settings::Theme;
use egui::Color32;

/// `--background-base-lowest` — server rail.
pub const RAIL: Color32 = Color32::from_rgb(0x2c, 0x2d, 0x32);
/// `--background-base-lower` — channel sidebar and DM list.
pub const SIDEBAR: Color32 = Color32::from_rgb(0x32, 0x33, 0x39);
/// `--background-base-low` — chat surface.
pub const CHAT: Color32 = Color32::from_rgb(0x36, 0x37, 0x3e);
/// `--input-background-default` (black 8% over the chat surface).
pub const INPUT: Color32 = Color32::from_rgb(0x32, 0x33, 0x39);
/// `--text-default` (neutral-4).
pub const TEXT: Color32 = Color32::from_rgb(0xf3, 0xf3, 0xf4);
/// `--text-muted` (neutral-23).
pub const MUTED: Color32 = Color32::from_rgb(0xab, 0xac, 0xb2);
/// `--brand-500`.
pub const BLURPLE: Color32 = Color32::from_rgb(0x58, 0x65, 0xf2);
/// `--green-360` — online presence and success.
pub const GREEN: Color32 = Color32::from_rgb(0x3d, 0x9e, 0x60);
/// `--green-new-50` — connected-voice accents.
pub const GREEN_VOICE: Color32 = Color32::from_rgb(0x00, 0x85, 0x45);
/// `--red-400` — destructive, muted mic, mention badge, new-messages divider.
pub const RED: Color32 = Color32::from_rgb(0xda, 0x3e, 0x44);
/// `--yellow-360` — idle presence and reconnecting states.
pub const YELLOW: Color32 = Color32::from_rgb(0xc9, 0x7e, 0x00);
pub const WHITE: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
/// White ~4% over the chat surface — row and widget hover.
pub const HOVER: Color32 = Color32::from_rgb(0x3e, 0x3f, 0x45);
/// White ~8% over the chat surface — selected rows.
pub const SELECTED: Color32 = Color32::from_rgb(0x43, 0x44, 0x4b);
/// Timeline dividers (date separators), a hairline over the chat surface.
pub const DIVIDER: Color32 = Color32::from_rgb(0x3f, 0x40, 0x46);
/// Translucent white overlay painted over a hovered message row.
pub const ROW_HOVER: Color32 = Color32::from_rgba_premultiplied(10, 10, 10, 10);
/// `--mention-background` / `--mention-foreground` — @user and #channel pills.
pub const MENTION_BG: Color32 = Color32::from_rgba_premultiplied(26, 30, 73, 77);
pub const MENTION_TEXT: Color32 = Color32::from_rgb(0xc9, 0xcd, 0xfb);
/// `--background-mentioned` and its left bar: a message that pings you.
pub const MENTIONED_BG: Color32 = Color32::from_rgba_premultiplied(19, 14, 4, 20);
pub const MENTIONED_BAR: Color32 = Color32::from_rgb(0xf0, 0xb2, 0x32);

/// Corner-radius tokens, from `--radius-sm/md/lg`.
pub const RADIUS_SM: f32 = 8.0;
pub const RADIUS_MD: f32 = 12.0;
pub const RADIUS_LG: f32 = 16.0;

/// The semibold family for names and headings: egui's `strong()` only
/// brightens the color, Discord's gg sans is 600 there.
pub fn bold() -> egui::FontFamily {
    egui::FontFamily::Name("bold".into())
}

/// Noto Sans (OFL, `assets/fonts`) in place of egui's thin default — the
/// closest free match for gg sans. egui's own fonts stay as fallbacks for
/// emoji and symbols.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        ("noto", &include_bytes!("../assets/fonts/NotoSans-Regular.ttf")[..]),
        ("noto-bold", &include_bytes!("../assets/fonts/NotoSans-Bold.ttf")[..]),
    ] {
        fonts.font_data.insert(
            name.into(),
            std::sync::Arc::new(egui::FontData::from_static(bytes)),
        );
    }
    let fallbacks = fonts.families[&egui::FontFamily::Proportional].clone();
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .insert(0, "noto".into());
    let mut semibold = vec!["noto-bold".to_string()];
    semibold.extend(fallbacks);
    fonts.families.insert(bold(), semibold);
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context, theme: Theme) {
    let mut visuals = match theme {
        Theme::Dark => egui::Visuals::dark(),
        Theme::Light => egui::Visuals::light(),
    };
    if theme == Theme::Dark {
        visuals.panel_fill = SIDEBAR;
        visuals.window_fill = CHAT;
        visuals.extreme_bg_color = INPUT;
        visuals.faint_bg_color = HOVER;
        visuals.widgets.hovered.bg_fill = HOVER;
        visuals.widgets.active.bg_fill = SELECTED;
        visuals.selection.bg_fill = BLURPLE;
        visuals.hyperlink_color = BLURPLE;
    }
    // Pointer cursor on everything clickable (docs/UI.md §5).
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    ctx.set_visuals(visuals);
}
