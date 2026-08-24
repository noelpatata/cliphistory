//! Embedded GUI picker (egui/eframe).
//!
//! Draws its own window on X11 and Wayland — no external launcher, no
//! system toolkit. This module owns orchestration and interaction; visual
//! constants live in [`theme`], font setup in [`fonts`].
//!
//! Features: live filter-as-you-type, ↑/↓ + Enter keyboard navigation with
//! automatic vertical scrolling of the selection, multi-line entries sized
//! to their line count (or to their wrapped height when the daemon asks
//! for word wrap), thumbnails rendered at their native aspect ratio,
//! Nerd Font glyph fallback, click to select.

mod fonts;
mod theme;

use anyhow::Result;
use cliphistory_proto::{ShowRequest, ShowResponse};
use std::sync::{Arc, Mutex};

/// One selectable history entry as the GUI needs it.
struct Row {
    index_token: String,
    /// Multi-line display preview (already formatted by the daemon).
    label: String,
    id: i64,
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
/// * Esc / closing the window → `Some(Dismissed)`
pub fn pick(req: &ShowRequest) -> Result<ShowResponse> {
    if req.entries.is_empty() {
        return Ok(ShowResponse::Dismissed);
    }

    let rows: Vec<Row> = req
        .entries
        .iter()
        .enumerate()
        .map(|(pos, entry)| Row {
            index_token: format!("[{:03}]", pos + 1),
            label: entry.preview.clone(),
            id: entry.id,
        })
        .collect();

    let decoded_thumbs: Vec<Option<DecodedThumb>> = req
        .entries
        .iter()
        .map(|entry| entry.thumbnail.as_deref().and_then(decode_png))
        .collect();

    let result = Arc::new(Mutex::new(None::<ShowResponse>));
    let result_for_app = result.clone();
    let font_family = req.view.font_family.clone();
    let word_wrap = req.view.word_wrap;
    let font_size = req.view.font_size.max(1) as f32;
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
            theme::apply(&cc.egui_ctx, font_size);
            Ok(Box::new(PickerApp::new(
                &cc.egui_ctx,
                rows,
                decoded_thumbs,
                word_wrap,
                font_size,
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
        rows: Vec<Row>,
        decoded: Vec<Option<DecodedThumb>>,
        wrap_labels: bool,
        body_size: f32,
        result: Arc<Mutex<Option<ShowResponse>>>,
    ) -> Self {
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
            mono_size: (body_size - 2.0).max(1.0),
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

    fn finish(&mut self, resp: ShowResponse) {
        *self.result.lock().expect("picker result lock") = Some(resp);
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Height of one row: driven by its text (line count, or the measured
    /// wrapped height when `wrap_labels` is set), never smaller than its
    /// thumbnail, padded for breathing room.
    fn row_height(&self, ui: &egui::Ui, index: usize) -> f32 {
        let line_h = ui.text_style_height(&egui::TextStyle::Body);
        let text_h = if self.wrap_labels {
            self.wrapped_label_height(ui, index).max(line_h)
        } else {
            self.rows[index].label.lines().count().max(1) as f32 * line_h
        };
        let thumb_h = self.thumbs[index]
            .as_ref()
            .map_or(0.0, |_| theme::THUMB_HEIGHT);
        text_h.max(thumb_h) + theme::ROW_PADDING
    }

    /// Width left for the label once the index token, optional thumbnail
    /// and the gaps between them have claimed their share of the row.
    fn label_max_width(&self, ui: &egui::Ui, index: usize) -> f32 {
        let mut w = ui.available_width()
            - index_token_width(ui, &self.rows[index].index_token, self.mono_size);
        if let Some(thumb) = &self.thumbs[index] {
            w -= thumb_display_size(thumb.native).x;
        }
        let gaps = if self.thumbs[index].is_some() {
            2.0
        } else {
            1.0
        };
        (w - theme::COLUMN_GAP * gaps).max(0.0)
    }

    /// Rendered height of the label soft-wrapped at its share of the
    /// window width.
    fn wrapped_label_height(&self, ui: &egui::Ui, index: usize) -> f32 {
        let max_w = self.label_max_width(ui, index);
        let font = egui::FontId::proportional(self.body_size);
        let text = self.rows[index].label.clone();
        ui.ctx().fonts(|fonts| {
            let job = egui::text::LayoutJob::simple(text, font, egui::Color32::WHITE, max_w);
            fonts.layout_job(job).size().y
        })
    }
}

/// Intrinsic width of an index token in the monospace face.
fn index_token_width(ui: &egui::Ui, token: &str, mono_size: f32) -> f32 {
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
fn thumb_display_size(native: egui::Vec2) -> egui::Vec2 {
    if native.y > theme::THUMB_HEIGHT {
        egui::vec2(
            native.x * (theme::THUMB_HEIGHT / native.y),
            theme::THUMB_HEIGHT,
        )
    } else {
        native
    }
}

impl eframe::App for PickerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keyboard: ↑/↓ move, Enter confirms, Esc dismisses. Typing always
        // lands in the filter box (focus is re-requested every frame).
        let up = ctx.input(|i| i.key_pressed(egui::Key::ArrowUp));
        let down = ctx.input(|i| i.key_pressed(egui::Key::ArrowDown));
        let confirm = ctx.input(|i| i.key_pressed(egui::Key::Enter))
            && !self.rows.is_empty();
        let dismiss = ctx.input(|i| i.key_pressed(egui::Key::Escape));

        let visible = self.visible();
        if visible.is_empty() {
            if confirm || dismiss {
                self.finish(ShowResponse::Dismissed);
            }
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
            }
        }
        if confirm {
            let id = self.rows[self.selected].id;
            self.finish(ShowResponse::Selected { id });
            return;
        }
        if dismiss {
            self.finish(ShowResponse::Dismissed);
            return;
        }

        let selection_changed = self.scrolled_for != self.selected;

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("🔍");
                let filter_response = ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("filter…")
                        .desired_width(f32::INFINITY),
                );
                // A launcher window lives and dies by its search field:
                // keep the caret there no matter what.
                if !filter_response.has_focus() {
                    filter_response.request_focus();
                }
            });
            ui.separator();

            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                let mut picked = None;
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
                                    thumb_display_size(thumb.native),
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

                    if hitbox.clicked() {
                        picked = Some(row_id);
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
