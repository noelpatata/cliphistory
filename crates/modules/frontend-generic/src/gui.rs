//! Embedded GUI picker (egui/eframe).
//!
//! Draws its own window on X11 and Wayland — no external launcher, no
//! system toolkit. This module owns orchestration and interaction; visual
//! constants live in [`theme`], font setup in [`fonts`], row measurement
//! in [`layout`].
//!
//! Features: live filter-as-you-type, ↑/↓ + Enter keyboard navigation with
//! automatic vertical scrolling of the selection, multi-line entries sized
//! to their line count (or to their wrapped height when the daemon asks
//! for word wrap), thumbnails rendered at their native aspect ratio,
//! Nerd Font glyph fallback, click to select, per-entry pin/delete buttons
//! reachable with →/← (plus Ctrl+P pin and Delete shortcuts), a
//! double-confirmed clear-all (button or Ctrl+Delete) that keeps pinned
//! entries — all edits go through the daemon IPC while the window stays
//! open. A background poller keeps the list in sync while it is open:
//! newly copied entries appear on their own and pins float to the top.

mod fonts;
mod keys;
mod layout;
mod theme;

use anyhow::Result;
use cliphistory_proto::{HistoryItem, ShowRequest, ShowResponse};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the clear-all confirmation stays armed before disarming.
const CLEAR_CONFIRM_WINDOW: Duration = Duration::from_secs(3);
/// Width reserved in the filter bar for the clear-all button plus its
/// breathing room from the window edge.
const CLEAR_BUTTON_RESERVE: f32 = 120.0;
/// Width of one square icon button in a row's action cluster.
const ACTION_BUTTON_SIZE: f32 = 24.0;
/// How often the open picker asks the daemon for fresh history.
const HISTORY_POLL_INTERVAL: Duration = Duration::from_millis(400);
/// How many entries a refresh fetches (mirrors the daemon's own show cap).
const HISTORY_POLL_LIMIT: usize = 100;

/// What part of the selected row has keyboard focus. `→` walks
/// Row → Pin → Delete → Row; `←` walks back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowAction {
    Row,
    Pin,
    Delete,
}

/// One selectable history entry as the GUI needs it.
struct Row {
    index_token: String,
    /// Multi-line display preview (already formatted by the daemon).
    label: String,
    id: i64,
    /// Pinned entries survive a clear-all, so they must survive it locally
    /// too.
    pinned: bool,
}

/// Decoded thumbnail plus its intrinsic pixel size.
struct Thumb {
    handle: egui::TextureHandle,
    native: egui::Vec2,
}

/// A thumbnail decoded from disk, before GPU upload (needs a context).
struct DecodedThumb {
    image: egui::ColorImage,
    native: egui::Vec2,
}

/// Show the picker window and block until the user chooses or closes it.
///
/// * clicking a row / pressing Enter → `Some(Selected)`
/// * Delete removes the focused entry via the daemon; the window stays open
/// * Ctrl+Delete (or the 🗑 button, twice) clears every unpinned entry
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

    // Live updates: a background thread polls the daemon's history and
    // streams snapshots over whenever they differ from the last one.
    let (update_tx, update_rx) = std::sync::mpsc::channel();
    if let Some(poll_socket) = socket.clone() {
        std::thread::Builder::new()
            .name("history-poll".into())
            .spawn(move || poll_history(poll_socket, update_tx))
            .ok();
    }
    let socket_for_app = socket;
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
            Ok(Box::new(PickerApp::new(
                &cc.egui_ctx,
                snapshot,
                socket_for_app,
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

/// A square glyph button for a row's action cluster; `focused` draws the
/// keyboard-focus outline.
fn action_button(glyph: egui::RichText, focused: bool) -> egui::Button<'static> {
    let mut button =
        egui::Button::new(glyph).min_size(egui::vec2(ACTION_BUTTON_SIZE, ACTION_BUTTON_SIZE));
    if focused {
        button = button
            .stroke(theme::focus_stroke())
            .fill(theme::focus_fill());
    }
    button
}

/// Decode a cached thumbnail PNG at its original aspect ratio — no
/// rescaling, no letterboxing; display sizing happens at draw time and GPU
/// upload once a context exists.
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

/// Map daemon history items onto selectable rows.
fn build_rows(items: &[HistoryItem]) -> Vec<Row> {
    items
        .iter()
        .enumerate()
        .map(|(pos, entry)| Row {
            index_token: format!("[{:03}]", pos + 1),
            label: entry.preview.clone(),
            id: entry.id,
            pinned: entry.pinned,
        })
        .collect()
}

/// Cheap change detector between two history snapshots: ids, pin state and
/// previews fully determine what the list renders.
fn fingerprint(items: &[HistoryItem]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for item in items {
        item.id.hash(&mut hasher);
        item.pinned.hash(&mut hasher);
        item.preview.hash(&mut hasher);
    }
    hasher.finish()
}

/// Indices of `pinned` reordered stable-first by pin state, mirroring the
/// daemon's ordering rule (pinned before unpinned, otherwise unchanged).
fn pinned_first_order(pinned: &[bool]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..pinned.len()).collect();
    order.sort_by_key(|&i| !pinned[i]);
    order
}

/// Rearrange `items` in place so element `order[k]` ends up at position
/// `k`. `order` must be a permutation.
fn apply_permutation<T>(order: &[usize], items: &mut Vec<T>) {
    let mut old: Vec<Option<T>> = std::mem::take(items).into_iter().map(Some).collect();
    *items = order
        .iter()
        .map(|&i| old[i].take().expect("permutation visits every index once"))
        .collect();
}

/// Poll the daemon for history snapshots until the process ends. Only
/// snapshots that differ from the previous one are forwarded, so idle
/// periods cost one tiny IPC round trip per interval and nothing else.
/// Failures (daemon restarting under us) are retried quietly.
fn poll_history(socket: std::path::PathBuf, tx: std::sync::mpsc::Sender<Vec<HistoryItem>>) {
    let mut last = None::<u64>;
    loop {
        if let Ok(items) = crate::ipc::history(&socket, HISTORY_POLL_LIMIT) {
            let fp = fingerprint(&items);
            if last != Some(fp) && tx.send(items).is_err() {
                break; // UI gone; window closed.
            }
            last = Some(fp);
        }
        std::thread::sleep(HISTORY_POLL_INTERVAL);
    }
}

/// Everything the picker renders, derived from one show request. Bundled
/// so [`PickerApp::new`] takes data + dependencies, not eight loose args.
struct PickerSnapshot {
    rows: Vec<Row>,
    /// Thumbnails decoded from disk, before GPU upload.
    decoded_thumbs: Vec<Option<DecodedThumb>>,
    wrap_labels: bool,
    body_size: f32,
}

struct PickerApp {
    rows: Vec<Row>,
    thumbs: Vec<Option<Thumb>>,
    /// Daemon hint: wrap long labels at the window edge instead of
    /// extending them behind the horizontal scrollbar.
    wrap_labels: bool,
    /// Base text size (and its monospace derivative) from the daemon, used
    /// to measure index tokens and wrapped label heights.
    body_size: f32,
    mono_size: f32,
    /// Daemon IPC endpoint for live edits (delete); `None` disables them.
    socket: Option<std::path::PathBuf>,
    /// Fresh history snapshots from the poller thread; the UI drains this
    /// every frame and rebuilds when a snapshot differs.
    updates: Receiver<Vec<HistoryItem>>,
    /// When the clear-all confirmation was armed, if it is armed.
    clear_armed_at: Option<Instant>,
    /// Which part of the selected row keyboard focus is on.
    focused_action: RowAction,
    filter: String,
    selected: usize,
    /// Selection the auto-scroll last centered on (`usize::MAX` initially).
    scrolled_for: usize,
    result: Arc<Mutex<Option<ShowResponse>>>,
    ctx: egui::Context,
}

impl PickerApp {
    fn new(
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
                slot.map(|d| Thumb {
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
            mono_size: theme::mono_size(body_size),
            socket,
            updates,
            clear_armed_at: None,
            focused_action: RowAction::Row,
            filter: String::new(),
            selected: 0,
            scrolled_for: usize::MAX,
            result,
            ctx: ctx.clone(),
        }
    }

    fn visible(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        (0..self.rows.len())
            .filter(|&i| {
                f.is_empty()
                    || self.rows[i].label.to_lowercase().contains(&f)
                    || format!("{:03}", i + 1).contains(&f)
            })
            .collect()
    }

    /// Take the freshest history snapshot from the poller, if any arrived
    /// since the last frame, and rebuild the list from it.
    fn drain_updates(&mut self) {
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
        let thumbs: Vec<Option<Thumb>> = items
            .iter()
            .map(|entry| {
                entry
                    .thumbnail
                    .as_deref()
                    .and_then(decode_png)
                    .map(|d| Thumb {
                        handle: self.ctx.load_texture(
                            format!("thumb-{}", entry.id),
                            d.image,
                            egui::TextureOptions::LINEAR,
                        ),
                        native: d.native,
                    })
            })
            .collect();
        self.rows = build_rows(&items);
        self.thumbs = thumbs;
        self.selected = selected_id
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .unwrap_or(0);
        // Follow the (possibly new) focused row after a structural change.
        self.scrolled_for = usize::MAX;
    }

    fn finish(&mut self, resp: ShowResponse) {
        *self.result.lock().expect("picker result lock") = Some(resp);
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Search field pinned to the top; a launcher window lives and dies by
    /// it, so the caret is re-claimed every frame. The clear-all button
    /// lives at the right edge and doubles as the confirmation indicator.
    fn filter_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("🔍");
            let filter_response = ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("filter…")
                    .desired_width(ui.available_width() - CLEAR_BUTTON_RESERVE),
            );
            if !filter_response.has_focus() {
                filter_response.request_focus();
            }
            self.clear_all_button(ui);
        });
        ui.separator();
    }

    /// The destructive clear-all control. First click arms it (label turns
    /// into a warning), clicking again within the confirm window fires.
    fn clear_all_button(&mut self, ui: &mut egui::Ui) {
        let armed = self
            .clear_armed_at
            .is_some_and(|at| at.elapsed() <= CLEAR_CONFIRM_WINDOW);
        let label = if armed {
            egui::RichText::new("❗ sure?").color(egui::Color32::from_rgb(255, 150, 80))
        } else {
            egui::RichText::new("🗑 clear all")
        };
        if ui.button(label).clicked() {
            self.clear_all_requested();
        }
    }

    /// Drop the focused entry from the list and ask the daemon to delete
    /// it, keeping the window open.
    fn delete_selected(&mut self) {
        let index = self.selected;
        self.delete_at(index);
    }

    /// Delete the entry at `index`: remove it from the local list (rows
    /// and thumbs in lockstep), renumber, and fire a background IPC so the
    /// UI never blocks; failures surface on stderr only.
    fn delete_at(&mut self, index: usize) {
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
        // Force the auto-scroll to re-center on the new focused row.
        self.scrolled_for = usize::MAX;
        self.renumber();

        if let Some(socket) = self.socket.clone() {
            std::thread::Builder::new()
                .name("delete-entry".into())
                .spawn(move || {
                    if let Err(e) = crate::ipc::delete_entry(&socket, id) {
                        eprintln!("cliphistory: deleting entry {id} failed: {e:#}");
                    }
                })
                .ok();
        }
    }

    /// Flip the pin state of the entry at `index`, then persist it through
    /// the daemon in the background.
    ///
    /// The list reorders optimistically (pinned entries float to the top,
    /// mirroring the daemon's ordering) so the effect is instant; the
    /// poller reconciles any difference within one interval.
    fn toggle_pin_at(&mut self, index: usize) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        row.pinned = !row.pinned;
        let (id, pinned) = (row.id, row.pinned);

        // Optimistic local reorder: pinned entries float to the top while
        // keeping the relative order of everything else.
        let flags: Vec<bool> = self.rows.iter().map(|r| r.pinned).collect();
        let order = pinned_first_order(&flags);
        apply_permutation(&order, &mut self.rows);
        apply_permutation(&order, &mut self.thumbs);
        self.selected = self
            .rows
            .iter()
            .position(|row| row.id == id)
            .unwrap_or(self.selected);
        self.scrolled_for = usize::MAX;
        self.renumber();

        if let Some(socket) = self.socket.clone() {
            std::thread::Builder::new()
                .name("set-pinned".into())
                .spawn(move || {
                    if let Err(e) = crate::ipc::set_pinned(&socket, id, pinned) {
                        eprintln!("cliphistory: pinning entry {id} failed: {e:#}");
                    }
                })
                .ok();
        }
    }

    /// Clear-all is destructive, so it asks twice: first call arms it (the
    /// button lights up), the second within [`CLEAR_CONFIRM_WINDOW`] fires.
    /// Pinned entries survive, matching the daemon's `ClearAll`.
    fn clear_all_requested(&mut self) {
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

        if let Some(socket) = self.socket.clone() {
            std::thread::Builder::new()
                .name("clear-all".into())
                .spawn(move || {
                    if let Err(e) = crate::ipc::clear_history(&socket) {
                        eprintln!("cliphistory: clearing history failed: {e:#}");
                    }
                })
                .ok();
        }
    }

    /// Re-align index tokens with list positions after structural edits.
    fn renumber(&mut self) {
        for (pos, row) in self.rows.iter_mut().enumerate() {
            row.index_token = format!("[{:03}]", pos + 1);
        }
    }

    /// Text metrics handed to [`layout`] for row measurement.
    fn metrics(&self) -> layout::TextMetrics {
        layout::TextMetrics {
            body_size: self.body_size,
            mono_size: self.mono_size,
            wrap_labels: self.wrap_labels,
        }
    }

    /// Height of one row, as computed by [`layout`].
    fn row_height(&self, ui: &egui::Ui, index: usize) -> f32 {
        layout::row_height(
            ui,
            &self.rows[index].label,
            &self.rows[index].index_token,
            self.thumbs[index].as_ref().map(|t| t.native),
            self.metrics(),
        )
    }
}

impl eframe::App for PickerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keyboard: ↑/↓ move, Enter confirms (or runs the focused per-row
        // action), →/← walk row → pin → delete, Esc dismisses, Delete
        // drops the focused entry, Ctrl+P pins it, Ctrl+Delete clears all
        // unpinned (asked twice). Typing always lands in the filter box
        // (focus is re-requested every frame); all bindings live in
        // [`keys`].
        let up = keys::pressed(ctx, &keys::MOVE_UP);
        let down = keys::pressed(ctx, &keys::MOVE_DOWN);
        let confirm = keys::pressed(ctx, &keys::CONFIRM) && !self.rows.is_empty();
        let dismiss = keys::pressed(ctx, &keys::DISMISS);
        let delete = keys::pressed(ctx, &keys::DELETE_ENTRY);
        let clear_all = keys::pressed(ctx, &keys::CLEAR_ALL);
        let toggle_pin = keys::pressed(ctx, &keys::TOGGLE_PIN);
        let action_next = keys::pressed(ctx, &keys::ACTION_NEXT);
        let action_prev = keys::pressed(ctx, &keys::ACTION_PREV);

        // An armed confirmation expires on its own.
        if let Some(at) = self.clear_armed_at {
            if at.elapsed() > CLEAR_CONFIRM_WINDOW {
                self.clear_armed_at = None;
            }
        }

        // Pull in any fresh history snapshot before rendering.
        self.drain_updates();

        let visible = self.visible();
        if visible.is_empty() {
            // Nothing matches the filter — or the list ran dry after
            // deletions; keep the window drawn so the state is visible,
            // only closing remains meaningful.
            if confirm || dismiss {
                self.finish(ShowResponse::Dismissed);
                return;
            }
            egui::CentralPanel::default().show(ctx, |ui| {
                self.filter_bar(ui);
                ui.weak("(nothing to show)");
            });
            return;
        }
        if !visible.contains(&self.selected) {
            self.selected = visible[0];
        }
        if up || down {
            if let Some(pos) = visible.iter().position(|&i| i == self.selected) {
                let next = if up {
                    pos.saturating_sub(1)
                } else {
                    (pos + 1).min(visible.len() - 1)
                };
                self.selected = visible[next];
                self.focused_action = RowAction::Row;
            }
        }
        if action_next {
            self.focused_action = match self.focused_action {
                RowAction::Row => RowAction::Pin,
                RowAction::Pin => RowAction::Delete,
                RowAction::Delete => RowAction::Row,
            };
        }
        if action_prev {
            self.focused_action = match self.focused_action {
                RowAction::Row => RowAction::Delete,
                RowAction::Delete => RowAction::Pin,
                RowAction::Pin => RowAction::Row,
            };
        }
        if confirm {
            match self.focused_action {
                RowAction::Row => {
                    let id = self.rows[self.selected].id;
                    self.finish(ShowResponse::Selected { id });
                    return;
                }
                RowAction::Pin => self.toggle_pin_at(self.selected),
                RowAction::Delete => self.delete_selected(),
            }
        }
        if toggle_pin {
            self.toggle_pin_at(self.selected);
            return;
        }
        if delete {
            self.delete_selected();
            return;
        }
        if clear_all {
            self.clear_all_requested();
            return;
        }
        if dismiss {
            // Esc backs out of the per-row actions before closing.
            if self.focused_action != RowAction::Row {
                self.focused_action = RowAction::Row;
            } else {
                self.finish(ShowResponse::Dismissed);
                return;
            }
        }

        let selection_changed = self.scrolled_for != self.selected;

        egui::CentralPanel::default().show(ctx, |ui| {
            self.filter_bar(ui);

            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                let mut picked = None;
                let mut pin_req = None;
                let mut delete_req = None;
                for &i in &visible {
                    let row_id = self.rows[i].id;
                    let selected = i == self.selected;
                    let row_h = self.row_height(ui, i);

                    // Full-width stripe; content is laid out left-aligned
                    // inside it. Labels extend (not wrap) past the viewport
                    // when long — the ScrollArea's horizontal bar follows —
                    // unless the daemon asked for word wrap.
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::hover(),
                    );
                    let hitbox = ui.interact(
                        rect,
                        egui::Id::new(("cliphistory-row", i)),
                        egui::Sense::click(),
                    );
                    ui.painter().rect_filled(
                        rect,
                        theme::ROW_CORNER_RADIUS,
                        theme::row_fill(selected, hitbox.hovered()),
                    );

                    ui.allocate_new_ui(
                        egui::UiBuilder::new()
                            .max_rect(rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center))
                            .id_salt(("cliphistory-row-ui", i)),
                        |inner| {
                            inner.style_mut().spacing.item_spacing.x = theme::COLUMN_GAP;
                            inner.add(
                                egui::Label::new(
                                    egui::RichText::new(&self.rows[i].index_token)
                                        .text_style(egui::TextStyle::Monospace)
                                        .color(theme::index_color()),
                                )
                                .wrap_mode(egui::TextWrapMode::Extend),
                            );
                            if let Some(thumb) = &self.thumbs[i] {
                                inner.add(egui::Image::new(egui::load::SizedTexture::new(
                                    thumb.handle.id(),
                                    layout::thumb_display_size(thumb.native),
                                )));
                            }
                            inner.add(
                                egui::Label::new(egui::RichText::new(&self.rows[i].label))
                                    .wrap_mode(if self.wrap_labels {
                                        egui::TextWrapMode::Wrap
                                    } else {
                                        egui::TextWrapMode::Extend
                                    })
                                    .selectable(false),
                            );
                        },
                    );

                    // Per-entry actions anchored at the row's right edge:
                    // full buttons on the selected/hovered row, a passive
                    // pin marker otherwise. Focus ring follows →/← keys.
                    let show_actions = selected || hitbox.hovered();
                    let mut pin_clicked = false;
                    let mut delete_clicked = false;
                    let pinned = self.rows[i].pinned;
                    if show_actions || pinned {
                        let pin_focused = selected && self.focused_action == RowAction::Pin;
                        let delete_focused = selected && self.focused_action == RowAction::Delete;
                        ui.allocate_new_ui(
                            egui::UiBuilder::new()
                                .max_rect(rect)
                                .layout(egui::Layout::right_to_left(egui::Align::Center))
                                .id_salt(("cliphistory-row-actions", i)),
                            |actions| {
                                actions.style_mut().spacing.item_spacing.x =
                                    theme::COLUMN_GAP / 2.0;
                                actions.add_space(theme::ROW_PADDING);
                                if show_actions {
                                    let delete_btn =
                                        action_button(egui::RichText::new("🗑"), delete_focused);
                                    let del = actions.add(delete_btn);
                                    delete_clicked |= del.clicked();

                                    let pin_glyph = if pinned {
                                        egui::RichText::new("📌")
                                    } else {
                                        egui::RichText::new("📌").weak()
                                    };
                                    let pin = actions.add(action_button(pin_glyph, pin_focused));
                                    pin_clicked |= pin.clicked();
                                } else {
                                    actions.label(
                                        egui::RichText::new("📌").color(theme::index_color()),
                                    );
                                }
                            },
                        );
                    }

                    if hitbox.clicked() && !pin_clicked && !delete_clicked {
                        picked = Some(row_id);
                    }
                    // Defer mutations until the loop is done: deleting or
                    // reordering rows mid-iteration would shift indices.
                    if pin_clicked {
                        pin_req = Some(i);
                    }
                    if delete_clicked {
                        delete_req = Some(i);
                    }
                    // Follow keyboard selection: center it once per change,
                    // both axes, leaving mouse-wheel scrolling alone.
                    // Follow keyboard selection vertically only: clamp the
                    // rect to the viewport's horizontal span so the
                    // centering pass has no x-shift to apply, leaving any
                    // manual horizontal scrolling untouched.
                    if selected && selection_changed {
                        let viewport = ui.clip_rect();
                        let vertical_band = egui::Rect::from_min_max(
                            egui::pos2(viewport.left(), rect.top()),
                            egui::pos2(viewport.right(), rect.bottom()),
                        );
                        ui.scroll_to_rect(vertical_band, Some(egui::Align::Center));
                    }
                }
                if let Some(id) = picked {
                    self.finish(ShowResponse::Selected { id });
                }
                // Apply deferred row-button actions once iteration is done.
                if let Some(i) = pin_req {
                    self.toggle_pin_at(i);
                }
                if let Some(i) = delete_req {
                    self.delete_at(i);
                }
            });
        });

        if selection_changed {
            self.scrolled_for = self.selected;
        }

        // Repaint continuously while a key is held so held-arrow scrolling
        // feels responsive even without widget activity.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn fingerprint_ignores_order_only_when_content_matches() {
        let a = vec![item(1, "x", false), item(2, "y", true)];
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        // Same entries, different pin state → different snapshot.
        let b = vec![item(1, "x", true), item(2, "y", true)];
        assert_ne!(fingerprint(&a), fingerprint(&b));
        // New entry → different snapshot.
        let c = vec![item(9, "z", false), item(1, "x", false), item(2, "y", true)];
        assert_ne!(fingerprint(&a), fingerprint(&c));
    }

    #[test]
    fn pinned_first_order_is_stable() {
        // Only the pinned entry floats to the front; everything else keeps
        // its relative order.
        assert_eq!(pinned_first_order(&[false, true, false]), vec![1, 0, 2]);
        assert_eq!(pinned_first_order(&[false, false]), vec![0, 1]);
        assert_eq!(pinned_first_order(&[]), Vec::<usize>::new());
    }

    #[test]
    fn apply_permutation_reorders_every_element_once() {
        let mut items = vec!["a", "b", "c"];
        apply_permutation(&[2, 0, 1], &mut items);
        assert_eq!(items, vec!["c", "a", "b"]);
    }
}
