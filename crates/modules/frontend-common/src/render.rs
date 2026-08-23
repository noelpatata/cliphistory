//! Menu line rendering and selection resolution.
//!
//! This module owns the *menu wire format*. Callers (frontend modules)
//! never touch escape sequences or dialect details.
//!
//! wofi (≤1.5.x) parses space-separated `mode:data` segments where every
//! segment prefix must itself be a known mode; consequently an image entry
//! is a SINGLE `img:<path>` segment — a text label cannot coexist with it.
//! Ids therefore travel through a side-table: the menu echoes the rendered
//! line back verbatim, and [`SelectionMap`] resolves it to the entry id.

use cliphistory_proto::HistoryItem;
use std::collections::HashMap;

/// One rendered menu entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuLine(pub String);

/// Maps echoed menu lines back to entry ids.
#[derive(Default)]
pub struct SelectionMap(pub HashMap<String, i64>);

impl SelectionMap {
    /// Build lines for every entry and register their selections.
    ///
    /// `render_images` must mirror the frontend's declared `images`
    /// feature: image-capable menus show cached thumbnails alone;
    /// otherwise entries render as legacy `<id>\t<label>` lines.
    pub fn build(entries: &[HistoryItem], render_images: bool) -> (Vec<MenuLine>, Self) {
        let mut map = SelectionMap(HashMap::with_capacity(entries.len()));
        let mut lines = Vec::with_capacity(entries.len());
        for entry in entries {
            let line = if render_images {
                match &entry.thumbnail {
                    Some(path) => format!("img:{path}"),
                    None => format!("text:{} {}", entry.id, flatten(&entry.preview)),
                }
            } else {
                format!("{}\t{}", entry.id, flatten(&entry.preview))
            };
            map.0.entry(line.clone()).or_insert(entry.id);
            lines.push(MenuLine(line));
        }
        (lines, map)
    }

    /// Resolve the menu's echoed selection.
    pub fn resolve(&self, raw: &str) -> Option<i64> {
        let line = raw.trim_end_matches(['\n', '\r']);
        if let Some(id) = self.0.get(line) {
            return Some(*id);
        }
        // Fallbacks for menus that trim/echo partially mangled lines.
        let stripped_img = line.strip_prefix("img:").unwrap_or(line);
        if let Some(id) = self.0.get(stripped_img) {
            return Some(*id);
        }
        let body = stripped_img.strip_prefix("text:").unwrap_or(stripped_img);
        let body = body.strip_prefix("text:").unwrap_or(body);
        let token = body
            .split_once('\t')
            .map(|(id, _)| id)
            .unwrap_or_else(|| body.split_once(' ').map(|(id, _)| id).unwrap_or(body));
        token.trim().parse::<i64>().ok()
    }
}

fn flatten(preview: &str) -> String {
    preview
        .replace('\n', "\\n")
        .replace('\r', "")
        .replace('\t', "  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: i64, preview: &str, thumb: Option<&str>) -> HistoryItem {
        HistoryItem {
            id,
            kind: if thumb.is_some() { "image" } else { "text" }.into(),
            mime: "application/octet-stream".into(),
            preview: preview.into(),
            size_bytes: 1,
            created_at: 0,
            use_count: 0,
            pinned: false,
            thumbnail: thumb.map(str::to_string),
        }
    }

    #[test]
    fn image_entries_are_single_img_segments() {
        let (lines, map) =
            SelectionMap::build(&[item(21, "[image png]", Some("/thumbs/a.png"))], true);
        assert_eq!(lines[0].0, "img:/thumbs/a.png");
        // No spaces beyond the segment separator: nothing can leak into the
        // file path wofi will load.
        assert_eq!(lines[0].0.split_whitespace().count(), 1);
        assert!(lines[0].0.chars().all(|c| !c.is_control()));
        assert_eq!(map.resolve(&lines[0].0), Some(21));
    }

    #[test]
    fn text_entries_in_image_mode_carry_visible_ids() {
        let (lines, map) = SelectionMap::build(&[item(4, "plain", None)], true);
        assert_eq!(lines[0].0, "text:4 plain");
        assert_eq!(map.resolve(&lines[0].0), Some(4));
    }

    #[test]
    fn plain_dialect_keeps_legacy_tab_format() {
        let (lines, map) = SelectionMap::build(&[item(7, "two\nlines\there", None)], false);
        assert_eq!(lines[0].0, "7\ttwo\\nlines  here");
        assert_eq!(map.resolve(&lines[0].0), Some(7));
    }

    #[test]
    fn resolution_survives_trailing_whitespace_and_legacy_shapes() {
        let (_, map) =
            SelectionMap::build(&[item(9, "x", Some("/t.png")), item(5, "y", None)], true);
        // wofi echoes the whole rendered line, trailing newline included.
        assert_eq!(map.resolve("img:/t.png\n"), Some(9));
        assert_eq!(map.resolve("text:5 y"), Some(5));
        assert_eq!(map.resolve("5\ty"), Some(5));
        assert_eq!(map.resolve(""), None);
    }

    #[test]
    fn image_lines_are_unique_per_thumbnail() {
        // Content-hash thumbnails guarantee one line per image; two entries
        // with the SAME thumbnail cannot exist (dedup upstream).
        let (lines, map) = SelectionMap::build(
            &[item(1, "a", Some("/h1.png")), item(2, "b", Some("/h2.png"))],
            true,
        );
        assert_ne!(lines[0].0, lines[1].0);
        assert_eq!(map.resolve(&lines[0].0), Some(1));
        assert_eq!(map.resolve(&lines[1].0), Some(2));
    }
}
