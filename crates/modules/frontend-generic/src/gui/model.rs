//! Pure data types and transforms for the picker.
//!
//! No egui state, no I/O — just mapping daemon snapshots onto renderable
//! rows and computing orderings. Unit-testable without a display.

use cliphistory_proto::HistoryItem;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// What part of the selected row has keyboard focus. `→` walks
/// Row → Pin → Delete → Row; `←` walks back. Only meaningful while
/// [`FocusSource::Keyboard`] is in charge.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    Row,
    Pin,
    Delete,
}

/// Which input device owns the selection — the last one used wins. Arrow
/// keys claim it for the keyboard, moving the cursor over a row claims it
/// for the mouse. Exactly one row is ever highlighted, so keyboard and
/// pointer focus can never diverge.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusSource {
    Keyboard,
    Mouse,
}

/// One selectable history entry as the GUI needs it.
#[derive(Clone)]
pub struct Row {
    pub index_token: String,
    /// Multi-line display preview (already formatted by the daemon).
    pub label: String,
    pub id: i64,
    /// Pinned entries survive a clear-all, so they must survive it locally
    /// too.
    pub pinned: bool,
    /// When this entry was last pinned; drives pin-date ordering.
    pub pinned_at: Option<u64>,
}

/// Decoded thumbnail plus its intrinsic pixel size.
pub struct Thumb {
    pub handle: egui::TextureHandle,
    pub native: egui::Vec2,
}

/// A thumbnail decoded from disk, before GPU upload (needs a context).
pub struct DecodedThumb {
    pub image: egui::ColorImage,
    pub native: egui::Vec2,
}

/// Map daemon history items onto selectable rows.
pub fn build_rows(items: &[HistoryItem]) -> Vec<Row> {
    items
        .iter()
        .enumerate()
        .map(|(pos, entry)| Row {
            index_token: format!("[{:03}]", pos + 1),
            label: entry.preview.clone(),
            id: entry.id,
            pinned: entry.pinned,
            pinned_at: entry.pinned_at,
        })
        .collect()
}

/// Cheap change detector between two history snapshots: ids, pin state and
/// previews fully determine what the list renders.
pub fn fingerprint(items: &[HistoryItem]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for item in items {
        item.id.hash(&mut hasher);
        item.pinned.hash(&mut hasher);
        item.preview.hash(&mut hasher);
    }
    hasher.finish()
}

/// Indices of `rows` reordered so pinned entries come first (earliest pin
/// on top — mirroring the daemon's ordering rule), everything else keeps
/// its relative order.
pub fn pinned_first_order(rows: &[Row]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| {
        let row = &rows[i];
        match (row.pinned, row.pinned_at) {
            // Pinned: group 0, sorted by pin time ascending.
            (true, at) => (0, at.unwrap_or(0)),
            // Unpinned: group 1, stable within the group.
            (false, _) => (1, 0),
        }
    });
    order
}

/// Rearrange `items` in place so element `order[k]` ends up at position
/// `k`. `order` must be a permutation.
pub fn apply_permutation<T>(order: &[usize], items: &mut Vec<T>) {
    let mut old: Vec<Option<T>> = std::mem::take(items).into_iter().map(Some).collect();
    *items = order
        .iter()
        .map(|&i| old[i].take().expect("permutation visits every index once"))
        .collect();
}
