//! Embedded GUI picker (egui/eframe).
//!
//! Draws its own window on X11 and Wayland — no external launcher, no
//! system toolkit. This is the primary picker; the dmenu/dialog/tty paths
//! are fallbacks for machines where a window cannot be opened.
//!
//! Features: live filter-as-you-type, ↑/↓ + Enter keyboard navigation,
//! image thumbnail previews (loaded from the cached PNGs), click to select.

use anyhow::Result;
use cliphistory_proto::{ShowRequest, ShowResponse};
use std::sync::{Arc, Mutex};

struct Row {
    index_token: String,
    label: String,
    id: i64,
    /// Decoded thumbnail, if the entry has one.
    thumb_rgba: Option<image::RgbaImage>,
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
        .map(|(pos, entry)| {
            let (index_token, label) =
                fc_render::render::indexed_pair(pos, &entry.preview);
            Row {
                index_token,
                label,
                id: entry.id,
                thumb_rgba: entry.thumbnail.as_deref().and_then(load_png),
            }
        })
        .collect();

    let result = Arc::new(Mutex::new(None::<ShowResponse>));
    let result_for_app = result.clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("cliphistory")
            .with_inner_size([560.0, 460.0]),
        ..Default::default()
    };

    eframe::run_native(
        "cliphistory",
        options,
        Box::new(move |cc| {
            Ok(Box::new(PickerApp::new(cc, rows, result_for_app)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("starting embedded GUI: {e}"))?;

    let taken = result.lock().expect("picker result lock").take();
    match taken {
        Some(resp) => Ok(resp),
        None => Ok(ShowResponse::Dismissed),
    }
}

fn load_png(path: &str) -> Option<image::RgbaImage> {
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?;

    // Normalize to a uniform square: scale preserving aspect ratio, then
    // letterbox onto a transparent 96×96 canvas (rendered at 48 px).
    // Every preview ends up identical in size, whatever its shape.
    const SIDE: u32 = 96;
    let scaled = img
        .resize_exact(SIDE, SIDE, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let (sw, sh) = (SIDE, SIDE);
    let mut canvas = image::RgbaImage::from_pixel(
        SIDE,
        SIDE,
        image::Rgba([0, 0, 0, 0]),
    );
    image::imageops::overlay(
        &mut canvas,
        &scaled,
        ((SIDE - sw) / 2) as i64,
        ((SIDE - sh) / 2) as i64,
    );
    Some(canvas)
}

use cliphistory_frontend_common as fc_render;

struct PickerApp {
    rows: Vec<Row>,
    textures: Vec<Option<egui::TextureHandle>>,
    filter: String,
    selected: usize,
    result: Arc<Mutex<Option<ShowResponse>>>,
    ctx: egui::Context,
}

impl PickerApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        rows: Vec<Row>,
        result: Arc<Mutex<Option<ShowResponse>>>,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        let textures = rows
            .iter()
            .map(|row| {
                row.thumb_rgba.as_ref().map(|rgba| {
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        size,
                        rgba.as_raw(),
                    );
                    // Downscale to a 48 px tall strip for uniform rows; the
                    // GPU/driver handles the resample.
                    ctx.load_texture(
                        format!("thumb-{}", row.id),
                        img,
                        egui::TextureOptions::LINEAR,
                    )
                })
            })
            .collect();
        Self {
            rows,
            textures,
            filter: String::new(),
            selected: 0,
            result,
            ctx,
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

            egui::ScrollArea::vertical().auto_shrink(false).show(
                ui,
                |ui| {
                    let mut picked = None;
                    for &i in &visible {
                        let row = &self.rows[i];
                        let selected = i == self.selected;
                        let text =
                            egui::RichText::new(format!("{} {}", row.index_token, row.label))
                                .size(14.0);

                        let response = if let Some(tex) = &self.textures[i] {
                            let img = egui::Image::new(egui::load::SizedTexture::new(tex.id(), egui::vec2(48.0, 48.0)));
                            let mut btn = egui::Button::image_and_text(img, text);
                            btn = btn.fill(if selected {
                                egui::Color32::from_rgb(64, 104, 168)
                            } else {
                                egui::Color32::TRANSPARENT
                            });
                            ui.add_sized([ui.available_width(), 52.0], btn)
                        } else {
                            let label =
                                egui::SelectableLabel::new(selected, text.clone());
                            ui.add_sized([ui.available_width(), 26.0], label)
                        };
                        if response.clicked() {
                            picked = Some(row.id);
                        }
                        if selected && response.hovered() {
                            ui.scroll_to_cursor(Some(egui::Align::Center));
                        }
                    }
                    if let Some(id) = picked {
                        self.finish(ShowResponse::Selected { id });
                    }
                },
            );
        });

        // Repaint continuously while a key is held so held-arrow scrolling
        // feels responsive even without widget activity.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
