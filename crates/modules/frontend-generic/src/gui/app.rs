//! Picker application state: selection, edits, and daemon interactions.
//!
//! Owns the mutable state of the picker window. Rendering lives in
//! [`super::render`]; this module handles state transitions and
//! fire-and-forget IPC so the UI never blocks on the daemon.

use super::model::{apply_permutation, pinned_first_order, Row, RowAction};
use cliphistory_proto::{HistoryItem, ShowResponse};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the clear-all confirmation stays armed before disarming.
pub(crate) const CLEAR_CONFIRM_WINDOW: Duration = Duration::from_secs(3);

/// Everything the picker renders, derived from one show request. Bundled
/// so `PickerApp::new` takes data + dependencies, not eight loose args.
pub(crate) struct PickerSnapshot {
    pub rows: Vec<Row>,
    /// Thumbnails decoded from disk, before GPU upload.
    pub decoded_thumbs: Vec<Option<super::model::DecodedThumb>>,
    pub wrap_labels: bool,
    pub body_size: f32,
}

pub(crate) struct PickerApp {
    pub rows: Vec<Row>,
    pub thumbs: Vec<Option<super::model::Thumb>>,
    /// Daemon hint: wrap long labels at the window edge instead of
    /// extending them behind the horizontal scrollbar.
    pub wrap_labels: bool,
    /// Base text size (and its monospace derivative) from the daemon, used
    /// to measure index tokens and wrapped label heights.
    pub body_size: f32,
    pub mono_size: f32,
    /// Daemon IPC endpoint for live edits; `None` disables them.
    pub socket: Option<std::path::PathBuf>,
    /// Fresh history snapshots from the sync thread; drained every frame.
    pub updates: Receiver<Vec<HistoryItem>>,
    /// When the clear-all confirmation was armed, if it is armed.
    pub clear_armed_at: Option<Instant>,
    /// Which part of the selected row keyboard focus is on.
    pub focused_action: RowAction,
    /// Which input device last moved the selection.
    pub focus_source: super::model::FocusSource,
    /// Cursor position on the previous frame, to detect real mouse motion.
    pub last_cursor: Option<egui::Pos2>,
    /// False when the loaded fonts cannot draw icon characters; resolved
    /// lazily on first frame (`Context::fonts` panics before first run).
    pub icons_ok: bool,
    pub fonts_probed: bool,
    pub filter: String,
    pub selected: usize,
    /// Selection the auto-scroll last centered on (`usize::MAX` initially).
    pub scrolled_for: usize,
    pub result: Arc<Mutex<Option<ShowResponse>>>,
    pub ctx: egui::Context,
}

impl PickerApp {
    pub(crate) fn new(
        ctx: &egui::Context,
        snapshot: PickerSnapshot,
        socket: Option<std::path::PathBuf>,
        updates: Receiver<Vec<HistoryItem>>,
        result: Arc<Mutex<Option<ShowResponse>>>,
    ) -> Self {
        let PickerSnapshot {
            rows,
            decoded_thumbs: decoded,
            wrap_labels,
            body_size,
        } = snapshot;
        let thumbs = decoded
            .into_iter()
            .enumerate()
            .map(|(i, slot)| {
                slot.map(|d| super::model::Thumb {
                    handle: ctx.load_texture(
                        format!("thumb-{i}"),
                        d.image,
                        egui::TextureOptions::LINEAR,
                    ),
                    native: d.native,
                })
            })
            .collect();
        Self {
            rows,
            thumbs,
            wrap_labels,
            body_size,
            mono_size: crate::gui::theme::mono_size(body_size),
            socket,
            updates,
            clear_armed_at: None,
            focused_action: RowAction::Row,
            focus_source: super::model::FocusSource::Keyboard,
            last_cursor: None,
            icons_ok: true,
            fonts_probed: false,
            filter: String::new(),
            selected: 0,
            scrolled_for: usize::MAX,
            result,
            ctx: ctx.clone(),
        }
    }

    pub(crate) fn visible(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        (0..self.rows.len())
            .filter(|&i| {
                f.is_empty()
                    || self.rows[i].label.to_lowercase().contains(&f)
                    || format!("{:03}", i + 1).contains(&f)
            })
            .collect()
    }

    /// Take the freshest history snapshot from the sync thread, if any
    /// arrived since the last frame, and rebuild the list from it.
    pub(crate) fn drain_updates(&mut self) {
        let mut latest = None;
        while let Ok(items) = self.updates.try_recv() {
            latest = Some(items);
        }
        if let Some(items) = latest {
            self.apply_items(items);
        }
    }

    /// Replace rows and thumbnails with a fresh snapshot, keeping the
    /// selection on the same entry when it still exists.
    fn apply_items(&mut self, items: Vec<HistoryItem>) {
        let selected_id = self.rows.get(self.selected).map(|row| row.id);
        let thumbs: Vec<Option<super::model::Thumb>> = items
            .iter()
            .map(|entry| {
                entry
                    .thumbnail
                    .as_deref()
                    .and_then(super::decode_png)
                    .map(|d| super::model::Thumb {
                        handle: self.ctx.load_texture(
                            format!("thumb-{}", entry.id),
                            d.image,
                            egui::TextureOptions::LINEAR,
                        ),
                        native: d.native,
                    })
            })
            .collect();
        self.rows = super::model::build_rows(&items);
        self.thumbs = thumbs;
        self.selected = selected_id
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .unwrap_or(0);
        // Follow the (possibly new) focused row after a structural change.
        self.scrolled_for = usize::MAX;
    }

    pub(crate) fn finish(&mut self, resp: ShowResponse) {
        *self.result.lock().expect("picker result lock") = Some(resp);
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Drop the focused entry from the list and ask the daemon to delete
    /// it, keeping the window open.
    pub(crate) fn delete_selected(&mut self) {
        let index = self.selected;
        self.delete_at(index);
    }

    /// Delete the entry at `index`: remove it from the local list (rows
    /// and thumbs in lockstep), renumber, and fire a background IPC so the
    /// UI never blocks; failures surface on stderr only.
    pub(crate) fn delete_at(&mut self, index: usize) {
        if index >= self.rows.len() {
            return;
        }
        let id = self.rows[index].id;
        self.rows.remove(index);
        self.thumbs.remove(index);
        self.selected = if self.rows.is_empty() {
            0
        } else {
            self.selected.min(self.rows.len() - 1)
        };
        // Structural change: action focus no longer points anywhere sane.
        self.focused_action = RowAction::Row;
        self.scrolled_for = usize::MAX;
        self.renumber();

        fire_and_forget(self.socket.clone(), "delete-entry", move |socket| {
            crate::ipc::delete_entry(socket, id)
        });
    }

    /// Flip the pin state of the entry at `index`, then persist it through
    /// the daemon in the background.
    ///
    /// The list reorders optimistically (pinned entries float to the top
    /// in pin-date order) so the effect is instant; the sync thread
    /// reconciles any difference within one interval.
    pub(crate) fn toggle_pin_at(&mut self, index: usize) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        row.pinned = !row.pinned;
        // Set pinned_at optimistically; daemon assigns the authoritative
        // timestamp.
        let now = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        );
        if row.pinned && row.pinned_at.is_none() {
            row.pinned_at = now;
        } else if !row.pinned {
            row.pinned_at = None;
        }
        let (id, pinned) = (row.id, row.pinned);

        let order = pinned_first_order(&self.rows);
        apply_permutation(&order, &mut self.rows);
        apply_permutation(&order, &mut self.thumbs);
        self.selected = self
            .rows
            .iter()
            .position(|row| row.id == id)
            .unwrap_or(self.selected);
        self.scrolled_for = usize::MAX;
        self.renumber();

        fire_and_forget(self.socket.clone(), "set-pinned", move |socket| {
            crate::ipc::set_pinned(socket, id, pinned)
        });
    }

    /// Clear-all is destructive, so it asks twice: first call arms it, the
    /// second within [`CLEAR_CONFIRM_WINDOW`] fires. Pinned entries
    /// survive, matching the daemon's `ClearAll`.
    pub(crate) fn clear_all_requested(&mut self) {
        match self.clear_armed_at {
            Some(at) if at.elapsed() <= CLEAR_CONFIRM_WINDOW => {
                self.clear_armed_at = None;
                self.clear_all_confirmed();
            }
            _ => self.clear_armed_at = Some(Instant::now()),
        }
    }

    fn clear_all_confirmed(&mut self) {
        // Pinned entries stay on the daemon; keep them in the list too.
        // rows and thumbs are index-aligned, so filter them in lockstep.
        let mut rows = Vec::new();
        let mut thumbs = Vec::new();
        for (row, thumb) in self.rows.drain(..).zip(self.thumbs.drain(..)) {
            if row.pinned {
                rows.push(row);
                thumbs.push(thumb);
            }
        }
        self.rows = rows;
        self.thumbs = thumbs;
        self.selected = 0;
        self.scrolled_for = usize::MAX;
        self.renumber();

        fire_and_forget(self.socket.clone(), "clear-all", |socket| {
            crate::ipc::clear_history(socket)
        });
    }

    /// Re-align index tokens with list positions after structural edits.
    fn renumber(&mut self) {
        for (pos, row) in self.rows.iter_mut().enumerate() {
            row.index_token = format!("[{:03}]", pos + 1);
        }
    }

    /// Text metrics handed to layout for row measurement.
    pub(crate) fn metrics(&self) -> crate::gui::layout::TextMetrics {
        crate::gui::layout::TextMetrics {
            body_size: self.body_size,
            mono_size: self.mono_size,
            wrap_labels: self.wrap_labels,
        }
    }

    /// Height of one row, as computed by layout.
    pub(crate) fn row_height(&self, ui: &egui::Ui, index: usize) -> f32 {
        crate::gui::layout::row_height(
            ui,
            &self.rows[index].label,
            &self.rows[index].index_token,
            self.thumbs[index].as_ref().map(|t| t.native),
            self.metrics(),
        )
    }
}

/// Fire-and-forget helper: spawn a background thread for an IPC call.
fn fire_and_forget(
    socket: Option<std::path::PathBuf>,
    name: &'static str,
    op: impl FnOnce(&std::path::Path) -> anyhow::Result<String> + Send + 'static,
) {
    if let Some(socket) = socket {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                if let Err(e) = op(&socket) {
                    eprintln!("cliphistory: {name} failed: {e:#}");
                }
            })
            .ok();
    }
}
