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

/// Background colour for a row given its interaction state.
pub fn row_fill(selected: bool, hovered: bool) -> egui::Color32 {
    if selected {
        SELECTION
    } else if hovered {
        // Dimmed variant of the selection colour.
        SELECTION.linear_multiply(0.55)
    } else {
        egui::Color32::TRANSPARENT
    }
}

/// Colour for index-token chrome.
pub fn index_color() -> egui::Color32 {
    INDEX_COLOR
}
