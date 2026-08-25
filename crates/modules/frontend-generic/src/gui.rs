//! Embedded GUI picker (egui/eframe).
//!
//! Draws its own window on X11 and Wayland — no external launcher, no
//! system toolkit. Submodules by concern: [`model`] owns data types and
//! pure transforms, [`sync`] the background history fetching, [`app`]
//! picker state and actions, [`render`] all drawing, [`keys`] key bindings,
//! [`layout`] row measurement, [`theme`] visual constants, [`fonts`] font
//! loading.
//!
//! Features: live filter-as-you-type, ↑/↓ + Enter keyboard navigation with
//! automatic vertical scrolling, multi-line entries sized to their line
//! count (or wrapped height), thumbnails at native aspect ratio, Nerd Font
//! glyph fallback, click / arrow-key selection with unified focus visuals,
//! per-entry pin/delete buttons (→/← + Enter or mouse), Ctrl+P pin,
//! Delete, double-confirmed clear-all (Ctrl+Delete) that keeps pinned
//! entries — all edits go through daemon IPC while the window stays open.
//! A background sync keeps the list live: new copies appear on their own.

pub(crate) mod app;
pub(crate) mod fonts;
pub(crate) mod keys;
pub(crate) mod layout;
pub(crate) mod model;
pub(crate) mod render;
pub(crate) mod sync;
pub(crate) mod theme;

use anyhow::Result;
use cliphistory_proto::{ShowRequest, ShowResponse};
use std::sync::{Arc, Mutex};

use app::PickerSnapshot;
use model::{build_rows, DecodedThumb};

/// Show the picker window and block until the user chooses or closes it.
///
/// * clicking a row / pressing Enter → `Some(Selected)`
/// * Delete removes the focused entry; Ctrl+Delete clears every unpinned
///   entry; the window stays open for further browsing
/// * Esc / closing the window → `Some(Dismissed)`
///
/// `socket` is the daemon IPC endpoint (from `CLIPHISTORY_SOCKET`);
/// without it the destructive shortcuts are inert.
pub fn pick(req: &ShowRequest, socket: Option<std::path::PathBuf>) -> Result<ShowResponse> {
    if req.entries.is_empty() {
        return Ok(ShowResponse::Dismissed);
    }

    let rows = build_rows(&req.entries);

    let decoded_thumbs: Vec<Option<DecodedThumb>> = req
        .entries
        .iter()
        .map(|entry| entry.thumbnail.as_deref().and_then(decode_png))
        .collect();

    let snapshot = PickerSnapshot {
        rows,
        decoded_thumbs,
        wrap_labels: req.view.word_wrap,
        body_size: req.view.font_size.max(1) as f32,
    };

    let result = Arc::new(Mutex::new(None::<ShowResponse>));
    let result_for_app = result.clone();
    let font_family = req.view.font_family.clone();

    // Live updates: a background thread subscribes to the daemon's history
    // and streams snapshots over whenever they differ from the last one.
    let (update_tx, update_rx) = std::sync::mpsc::channel();
    if let Some(sync_socket) = socket.clone() {
        std::thread::Builder::new()
            .name("history-sync".into())
            .spawn(move || sync::sync_history(sync_socket, update_tx))
            .ok();
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("cliphistory")
            .with_inner_size([560.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "cliphistory",
        options,
        Box::new(move |cc| {
            fonts::install(&cc.egui_ctx, font_family.as_deref());
            theme::apply(&cc.egui_ctx, snapshot.body_size);
            Ok(Box::new(app::PickerApp::new(
                &cc.egui_ctx,
                snapshot,
                socket,
                update_rx,
                result_for_app,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("starting embedded GUI: {e}"))?;

    let taken = result.lock().expect("picker result lock").take();
    match taken {
        Some(resp) => Ok(resp),
        None => Ok(ShowResponse::Dismissed),
    }
}

/// Decode a cached thumbnail PNG at its original aspect ratio.
fn decode_png(path: &str) -> Option<DecodedThumb> {
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let native = egui::vec2(img.width() as f32, img.height() as f32);
    Some(DecodedThumb {
        image: egui::ColorImage::from_rgba_unmultiplied(
            [img.width() as usize, img.height() as usize],
            img.as_raw(),
        ),
        native,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cliphistory_proto::HistoryItem;

    fn item(id: i64, preview: &str, pinned: bool) -> HistoryItem {
        HistoryItem {
            id,
            kind: "text".into(),
            mime: "text/plain".into(),
            preview: preview.into(),
            size_bytes: preview.len() as u64,
            created_at: 0,
            use_count: 0,
            pinned,
            pinned_at: None,
            thumbnail: None,
        }
    }

    #[test]
    fn build_rows_numbers_and_carries_state() {
        let rows = build_rows(&[item(7, "a", false), item(3, "b", true)]);
        assert_eq!(rows[0].index_token, "[001]");
        assert_eq!(rows[1].index_token, "[002]");
        assert_eq!(rows[1].id, 3);
        assert!(rows[1].pinned);
    }

    #[test]
    fn fingerprint_changes_when_pin_flips() {
        let a = vec![item(1, "x", false), item(2, "y", true)];
        assert_eq!(model::fingerprint(&a), model::fingerprint(&a.clone()));
        let b = vec![item(1, "x", true), item(2, "y", true)];
        assert_ne!(model::fingerprint(&a), model::fingerprint(&b));
    }
}
