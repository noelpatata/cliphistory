//! Visual constants and style installation for the picker window.
//!
//! One place to tune sizes, spacing and colours; [`apply`] is called once at
//! startup so every widget picks the values up from `egui`'s style.

use egui::{FontId, TextStyle};

/// Gap between the body face and the smaller derived faces (monospace,
/// small): 16 px body renders a 14 px monospace.
const MONO_DELTA: f32 = 2.0;

/// Size of the derived monospace/small faces for a given body size.
pub fn mono_size(body_size: f32) -> f32 {
    (body_size - MONO_DELTA).max(1.0)
}
/// Breathing room inside each row.
pub const ROW_PADDING: f32 = 10.0;
/// Corner radius of row highlight rectangles.
pub const ROW_CORNER_RADIUS: f32 = 4.0;
/// Displayed thumbnail height; width follows the image's own aspect.
pub const THUMB_HEIGHT: f32 = 64.0;
/// Gap between the index token, thumbnail and label.
pub const COLUMN_GAP: f32 = 12.0;

/// Row background when highlighted as the keyboard selection.
const SELECTION: egui::Color32 = egui::Color32::from_rgb(64, 104, 168);
/// Dimmed chrome for the `[NNN]` index tokens.
const INDEX_COLOR: egui::Color32 = egui::Color32::from_rgb(130, 145, 170);
/// Keyboard-focus accent for per-row action buttons.
const FOCUS_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 170, 70);
/// Inverted fill for the pin button when its entry is pinned: light
/// background so the pin state pops against the blue selection stripe.
pub const PINNED_FILL: egui::Color32 = egui::Color32::from_rgb(215, 222, 234);
/// Dark glyph on top of [`PINNED_FILL`].
pub const PINNED_TEXT: egui::Color32 = egui::Color32::from_rgb(40, 55, 85);

/// Per-row visual state passed to paint helpers so colours stay
/// consistent across the row.
#[derive(Clone, Copy)]
pub struct RowPalette {
    pub fill: egui::Color32,
    /// Main text colour for label / glyph content.
    pub text: egui::Color32,
    /// Dimmed chrome (index tokens).
    pub chrome: egui::Color32,
}

/// Derive the palette for one row.
pub fn row_palette(selected: bool) -> RowPalette {
    if selected {
        RowPalette {
            fill: SELECTION,
            text: egui::Color32::WHITE,
            chrome: INDEX_COLOR,
        }
    } else {
        RowPalette {
            fill: egui::Color32::TRANSPARENT,
            text: egui::Color32::WHITE,
            chrome: INDEX_COLOR,
        }
    }
}

/// Install fonts-independent styling (sizes, spacing) into `ctx`.
///
/// `body_size` (from `frontend.font_size`) drives the main text faces;
/// monospace and small styles stay [`MONO_DELTA`] smaller.
pub fn apply(ctx: &egui::Context, body_size: f32) {
    let mono_size = mono_size(body_size);
    ctx.style_mut(|style| {
        style.text_styles = [
            (TextStyle::Body, FontId::proportional(body_size)),
            (TextStyle::Button, FontId::proportional(body_size)),
            (TextStyle::Monospace, FontId::monospace(mono_size)),
            (TextStyle::Small, FontId::proportional(mono_size)),
            (TextStyle::Heading, FontId::proportional(body_size + 6.0)),
        ]
        .into();
    });
}

/// Colour for index-token chrome.
pub fn index_color() -> egui::Color32 {
    INDEX_COLOR
}

/// Outline drawn on the keyboard-focused per-row action button. Bright
/// amber on purpose: it must stand out against the blue selection stripe.
pub fn focus_stroke() -> egui::Stroke {
    egui::Stroke::new(2.5_f32, FOCUS_COLOR)
}

/// Tint behind the keyboard-focused action button.
pub fn focus_fill() -> egui::Color32 {
    FOCUS_COLOR.gamma_multiply(0.25)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_size_is_two_px_smaller() {
        assert_eq!(mono_size(16.0), 14.0);
        assert_eq!(mono_size(18.0), 16.0);
    }

    #[test]
    fn mono_size_never_goes_below_one() {
        assert_eq!(mono_size(0.0), 1.0);
        assert_eq!(mono_size(1.5), 1.0);
    }

    #[test]
    fn row_palette_selected_has_fill() {
        let p = row_palette(true);
        assert_eq!(p.fill, SELECTION);
        assert_eq!(p.text, egui::Color32::WHITE);
    }

    #[test]
    fn row_palette_unselected_is_transparent() {
        let p = row_palette(false);
        assert_eq!(p.fill, egui::Color32::TRANSPARENT);
        assert_eq!(p.text, egui::Color32::WHITE);
    }

    #[test]
    fn index_color_matches_constant() {
        assert_eq!(index_color(), INDEX_COLOR);
    }

    #[test]
    fn focus_stroke_is_two_point_five() {
        let s = focus_stroke();
        assert_eq!(s.width, 2.5);
        assert_eq!(s.color, FOCUS_COLOR);
    }

    #[test]
    fn focus_fill_is_dimmed() {
        let f = focus_fill();
        assert_eq!(f, FOCUS_COLOR.gamma_multiply(0.25));
    }
}
