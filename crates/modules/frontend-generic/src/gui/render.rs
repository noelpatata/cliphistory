//! Rendering for the picker window.
//!
//! Owns the eframe event loop: keyboard handling, row painting, per-entry
//! action cluster, filter bar and clear-all confirmation. State transitions
//! live in [`super::app`].

use super::app::{PickerApp, CLEAR_CONFIRM_WINDOW};
use super::layout;
use super::model::{FocusSource, RowAction};
use crate::gui::{keys, theme};
use cliphistory_proto::ShowResponse;

/// Width reserved in the filter bar for the clear-all button, the gap
/// before it, and right-edge padding. Scales with font size.
fn clear_button_reserve(body_size: f32) -> f32 {
    let scale = body_size / 16.0;
    (120.0 + 12.0) * scale
}
/// Scaled width of one square icon button in a row's action cluster.
fn action_button_size(body_size: f32) -> f32 {
    24.0 * body_size / 16.0
}
/// Scaled corner radius of an action button.
fn action_button_radius(body_size: f32) -> f32 {
    6.0 * body_size / 16.0
}
/// Scaled glyph size inside action buttons.
fn action_glyph_size(body_size: f32) -> f32 {
    14.0 * body_size / 16.0
}
/// Characters used as icons; probed once against the loaded fonts.
const UI_ICONS: &[char] = &['🔍', '🗑', '📌', '❗'];

/// True when every icon character is renderable by the loaded font chain.
pub(crate) fn icons_render(ctx: &egui::Context) -> bool {
    ctx.fonts(|fonts| {
        let font = egui::FontId::proportional(14.0);
        UI_ICONS.iter().all(|&c| fonts.has_glyph(&font, c))
    })
}

impl eframe::App for PickerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Font capabilities can only be probed once egui has run a frame.
        if !self.fonts_probed {
            self.icons_ok = icons_render(ctx);
            self.fonts_probed = true;
        }

        let up = keys::pressed(ctx, &self.bindings.move_up);
        let down = keys::pressed(ctx, &self.bindings.move_down);
        let confirm = keys::pressed(ctx, &self.bindings.confirm) && !self.rows.is_empty();
        let dismiss = keys::pressed(ctx, &self.bindings.dismiss);
        let delete = keys::pressed(ctx, &self.bindings.delete_entry);
        let clear_all = keys::pressed(ctx, &self.bindings.clear_all);
        let toggle_pin = keys::pressed(ctx, &self.bindings.toggle_pin);
        let action_next = keys::pressed(ctx, &self.bindings.action_next);
        let action_prev = keys::pressed(ctx, &self.bindings.action_prev);

        // Any key interaction claims the selection for the keyboard.
        let keyboard_used = up
            || down
            || confirm
            || delete
            || clear_all
            || toggle_pin
            || action_next
            || action_prev;

        // Track real cursor motion: a pointer merely *resting* over a row
        // must not steal the selection back after arrow keys move away.
        let cursor = ctx.input(|i| i.pointer.latest_pos());
        let cursor_moved = cursor != self.last_cursor && cursor.is_some();
        self.last_cursor = cursor;
        if keyboard_used {
            self.focus_source = FocusSource::Keyboard;
        }

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
                // Mutate + return: the rest of this frame must not render
                // stale indices. Window stays open.
                RowAction::Pin => {
                    self.toggle_pin_at(self.selected);
                    return;
                }
                RowAction::Delete => {
                    self.delete_selected();
                    return;
                }
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
            if self.focused_action != RowAction::Row {
                self.focused_action = RowAction::Row;
            } else {
                self.finish(ShowResponse::Dismissed);
                return;
            }
        }

        // Center on the selection only for keyboard navigation; hover
        // changes must not yank the scroll position around.
        let selection_changed =
            self.scrolled_for != self.selected && self.focus_source == FocusSource::Keyboard;

        egui::CentralPanel::default().show(ctx, |ui| {
            self.filter_bar(ui);

            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                let mut picked = None;
                let mut pin_req = None;
                let mut delete_req = None;
                let mut hover_req = None;
                for &i in &visible {
                    if i >= self.rows.len() {
                        continue;
                    }
                    let row_id = self.rows[i].id;
                    let selected = i == self.selected;
                    let row_h = self.row_height(ui, i);

                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::hover(),
                    );
                    let hitbox = ui.interact(
                        rect,
                        egui::Id::new(("cliphistory-row", i)),
                        egui::Sense::click(),
                    );
                    let palette = theme::row_palette(selected);
                    ui.painter()
                        .rect_filled(rect, theme::ROW_CORNER_RADIUS, palette.fill);

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
                                        .color(palette.chrome),
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
                                egui::Label::new(
                                    egui::RichText::new(&self.rows[i].label).color(palette.text),
                                )
                                .wrap_mode(if self.wrap_labels {
                                    egui::TextWrapMode::Wrap
                                } else {
                                    egui::TextWrapMode::Extend
                                })
                                .selectable(false),
                            );
                        },
                    );

                    // Per-entry actions anchored at the row's right edge.
                    let show_actions = selected;
                    let mut pin_clicked = false;
                    let mut delete_clicked = false;
                    let pinned = self.rows[i].pinned;
                    if show_actions || (pinned && self.icons_ok) {
                        let keyboard = self.focus_source == FocusSource::Keyboard;
                        let pin_focused =
                            selected && keyboard && self.focused_action == RowAction::Pin;
                        let delete_focused =
                            selected && keyboard && self.focused_action == RowAction::Delete;
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
                                    let del = action_button(
                                        actions,
                                        self.body_size,
                                        if self.icons_ok { "🗑" } else { "X" },
                                        false,
                                        delete_focused,
                                        Some(palette.text),
                                        false,
                                    );
                                    delete_clicked |= del.clicked();

                                    // Inverted when pinned: light fill +
                                    // dark glyph = visible pin indicator.
                                    let pin = action_button(
                                        actions,
                                        self.body_size,
                                        if self.icons_ok { "📌" } else { "P" },
                                        !pinned,
                                        pin_focused,
                                        None,
                                        pinned,
                                    );
                                    pin_clicked |= pin.clicked();
                                } else {
                                    actions.label(egui::RichText::new("📌").color(palette.chrome));
                                }
                            },
                        );
                    }

                    if hitbox.clicked() && !pin_clicked && !delete_clicked {
                        picked = Some(row_id);
                    }
                    if cursor_moved && hitbox.hovered() && i != self.selected {
                        hover_req = Some(i);
                    }
                    if pin_clicked {
                        pin_req = Some(i);
                    }
                    if delete_clicked {
                        delete_req = Some(i);
                    }
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
                if let Some(i) = pin_req {
                    self.toggle_pin_at(i);
                }
                if let Some(i) = delete_req {
                    self.delete_at(i);
                }
                // Cursor claims the selection last (latest input wins).
                if let Some(i) = hover_req {
                    self.selected = i;
                    self.focus_source = FocusSource::Mouse;
                    self.focused_action = RowAction::Row;
                    self.scrolled_for = i;
                }
            });
        });

        if selection_changed {
            self.scrolled_for = self.selected;
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

impl PickerApp {
    /// Search field pinned to the top; the caret is re-claimed every frame.
    pub(crate) fn filter_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.icons_ok {
                ui.label("🔍");
            }
            let filter_response = ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("filter…")
                    .desired_width(ui.available_width() - clear_button_reserve(self.body_size)),
            );
            // Reclaim focus only when nothing else has it (first frame or
            // after a widget was removed). Respects Tab navigation.
            let anything_focused = ui.memory(|m| m.focused().is_some());
            if !anything_focused && !filter_response.has_focus() {
                filter_response.request_focus();
            }
            self.clear_all_button(ui);
        });
        ui.separator();
    }

    /// The destructive clear-all control. First click arms it; clicking
    /// again within the confirm window fires.
    fn clear_all_button(&mut self, ui: &mut egui::Ui) {
        let armed = self
            .clear_armed_at
            .is_some_and(|at| at.elapsed() <= CLEAR_CONFIRM_WINDOW);
        let warn_text = if self.icons_ok {
            "❗ sure?"
        } else {
            "! sure?"
        };
        let warn = egui::RichText::new(warn_text).color(egui::Color32::from_rgb(255, 150, 80));
        let label = if armed {
            warn
        } else if self.icons_ok {
            egui::RichText::new("🗑 clear all")
        } else {
            egui::RichText::new("clear all")
        };
        if ui.button(label).clicked() {
            self.clear_all_requested();
        }
    }
}

/// A square per-row action button.
///
/// Cursor hover and arrow-key focus produce the **same** appearance: amber
/// tint plus ring. `inverted` (used by the pin button when its entry is
/// pinned) swaps to a light fill + dark glyph so the pin state is visible
/// at a glance.
fn action_button(
    ui: &mut egui::Ui,
    body_size: f32,
    glyph: &str,
    dimmed: bool,
    keyboard_focused: bool,
    fg_override: Option<egui::Color32>,
    inverted: bool,
) -> egui::Response {
    let size = action_button_size(body_size);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(size, size),
        egui::Sense::all(),
    );
    let active = keyboard_focused || resp.hovered();
    let bg = if active {
        theme::focus_fill()
    } else if inverted {
        theme::PINNED_FILL
    } else {
        ui.visuals().widgets.inactive.bg_fill
    };
    let stroke = if active {
        theme::focus_stroke()
    } else {
        egui::Stroke::NONE
    };
    let radius = action_button_radius(body_size);
    ui.painter().rect_filled(rect, radius, bg);
    ui.painter()
        .rect_stroke(rect, radius, stroke, egui::StrokeKind::Inside);
    // On the light inverted fill, always use a dark glyph for contrast.
    let glyph_color = if inverted && !active {
        theme::PINNED_TEXT
    } else if dimmed && !active {
        theme::index_color()
    } else {
        fg_override.unwrap_or_else(|| ui.visuals().text_color())
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(action_glyph_size(body_size)),
        glyph_color,
    );
    resp
}
