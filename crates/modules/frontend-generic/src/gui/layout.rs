//! Row geometry: how much space one entry needs before it is drawn.
//!
//! Pure measurement functions over egui state, kept separate from
//! [`super::gui`] so the picker orchestrates and this module computes.
//! Wrap-aware heights matter: rows are allocated up-front so the selection
//! stripe and click hitbox cover the whole entry.

use super::theme;
use egui::Ui;

/// Per-window text metrics the measurements depend on.
#[derive(Clone, Copy)]
pub(crate) struct TextMetrics {
    pub body_size: f32,
    pub mono_size: f32,
    /// Soft-wrap labels at the window edge instead of extending them.
    pub wrap_labels: bool,
}

/// Height of one row: driven by its text (line count, or the measured
/// wrapped height), never smaller than its thumbnail, padded for
/// breathing room.
pub(crate) fn row_height(
    ui: &Ui,
    label: &str,
    index_token: &str,
    thumb_native: Option<egui::Vec2>,
    m: TextMetrics,
) -> f32 {
    let line_h = ui.text_style_height(&egui::TextStyle::Body);
    let text_h = if m.wrap_labels {
        let max_w = label_max_width(ui, index_token, thumb_native, m);
        wrapped_label_height(ui, label, max_w, m.body_size).max(line_h)
    } else {
        label.lines().count().max(1) as f32 * line_h
    };
    let thumb_h = thumb_native.map_or(0.0, |_| theme::THUMB_HEIGHT);
    text_h.max(thumb_h) + theme::ROW_PADDING
}

/// Width left for the label once the index token, optional thumbnail and
/// the gaps between them have claimed their share of the row.
fn label_max_width(
    ui: &Ui,
    index_token: &str,
    thumb_native: Option<egui::Vec2>,
    m: TextMetrics,
) -> f32 {
    let mut w = ui.available_width() - index_token_width(ui, index_token, m.mono_size);
    if let Some(native) = thumb_native {
        w -= thumb_display_size(native).x;
    }
    let gaps = if thumb_native.is_some() { 2.0 } else { 1.0 };
    (w - theme::COLUMN_GAP * gaps).max(0.0)
}

/// Rendered height of `label` soft-wrapped at `max_w`.
fn wrapped_label_height(ui: &Ui, label: &str, max_w: f32, body_size: f32) -> f32 {
    let font = egui::FontId::proportional(body_size);
    let text = label.to_string();
    ui.ctx().fonts(|fonts| {
        let job = egui::text::LayoutJob::simple(text, font, egui::Color32::WHITE, max_w);
        fonts.layout_job(job).size().y
    })
}

/// Intrinsic width of an index token in the monospace face.
fn index_token_width(ui: &Ui, token: &str, mono_size: f32) -> f32 {
    ui.ctx().fonts(|fonts| {
        fonts
            .layout_no_wrap(
                token.to_string(),
                egui::FontId::monospace(mono_size),
                egui::Color32::WHITE,
            )
            .size()
            .x
    })
}

/// Display size for a thumbnail: own aspect ratio, capped height, never
/// upscaled.
pub(crate) fn thumb_display_size(native: egui::Vec2) -> egui::Vec2 {
    if native.y > theme::THUMB_HEIGHT {
        egui::vec2(
            native.x * (theme::THUMB_HEIGHT / native.y),
            theme::THUMB_HEIGHT,
        )
    } else {
        native
    }
}
