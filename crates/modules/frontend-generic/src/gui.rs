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

    let font_scale = req.view.font_size as f32 / 16.0;
    let base_width = if req.view.window_width > 0 {
        req.view.window_width as f32
    } else {
        cliphistory_proto::DEFAULT_WINDOW_WIDTH as f32
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("cliphistory")
            .with_inner_size([
                (base_width * font_scale).round(),
                (520.0 * font_scale).round(),
            ]),
        // Disabling vsync: on Wayland, vsync waits for a compositor frame
        // callback that never fires when the surface is hidden (workspace
        // switch), blocking the main thread and triggering the
        // compositor's "not responding" detection.
        vsync: false,
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
                keys::resolve(&req.view.keys),
                result_for_app,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("starting embedded GUI: {e}"))?;

    log::info!(
        "picker window closed; response={}",
        serde_json::to_string(
            result
                .lock()
                .expect("picker result lock")
                .as_ref()
                .unwrap_or(&ShowResponse::Dismissed)
        )
        .unwrap_or_default()
    );

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

    #[test]
    fn build_rows_empty() {
        let rows = build_rows(&[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn pinned_first_order_groups_pinned_before_unpinned() {
        use model::{apply_permutation, pinned_first_order};
        let rows = model::build_rows(&[
            item(1, "a", false),
            item(2, "b", true),
            item(3, "c", true),
            item(4, "d", false),
        ]);
        let order = pinned_first_order(&rows);
        assert_eq!(order.len(), 4);
        // Indices 1 and 2 are pinned, must come first.
        assert!(order[0] == 1 || order[0] == 2);
        assert!(order[1] == 1 || order[1] == 2);
        assert_ne!(order[0], order[1]);
        // Indices 0 and 3 are unpinned, must come last.
        assert!(order[2] == 0 || order[2] == 3);
        assert!(order[3] == 0 || order[3] == 3);
    }

    #[test]
    fn pinned_first_order_respects_pinned_at() {
        use model::{apply_permutation, pinned_first_order};
        let mut a = item(1, "old", true);
        a.pinned_at = Some(10);
        let mut b = item(2, "new", true);
        b.pinned_at = Some(5);
        let rows = model::build_rows(&[a, b]);
        let order = pinned_first_order(&rows);
        // Lower pinned_at first: index 1 (pinned_at=5) before index 0 (pinned_at=10).
        assert_eq!(order[0], 1);
        assert_eq!(order[1], 0);
    }

    #[test]
    fn apply_permutation_reorders() {
        use model::apply_permutation;
        let mut v = vec!["a", "b", "c"];
        apply_permutation(&[2, 0, 1], &mut v);
        assert_eq!(v, vec!["c", "a", "b"]);
    }

    #[test]
    fn apply_permutation_identity() {
        use model::apply_permutation;
        let mut v = vec![10, 20, 30];
        apply_permutation(&[0, 1, 2], &mut v);
        assert_eq!(v, vec![10, 20, 30]);
    }

    #[test]
    fn apply_permutation_empty() {
        use model::apply_permutation;
        let mut v: Vec<i32> = vec![];
        apply_permutation(&[], &mut v);
        assert!(v.is_empty());
    }

    #[test]
    fn fingerprint_empty_is_zero_like() {
        assert_eq!(model::fingerprint(&[]), model::fingerprint(&[]));
    }

    #[test]
    fn fingerprint_differs_on_id_change() {
        let a = vec![item(1, "x", false)];
        let b = vec![item(2, "x", false)];
        assert_ne!(model::fingerprint(&a), model::fingerprint(&b));
    }
}
