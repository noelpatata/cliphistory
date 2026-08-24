//! Menu line rendering and selection resolution.
//!
//! This module owns the *menu wire format*. Callers (frontend modules)
//! never touch escape sequences or dialect details.
//!
//! Two dialects:
//!
//! * **plain** — every entry renders as ``[NNN] <label>`` where `NNN` is a
//!   zero-padded picker index, deliberately styled as UI chrome so it is
//!   never mistaken for pasteable content.
//!
//! * **image-capable** (wofi ≤ 1.5 constraint) — an entry with a thumbnail
//!   MUST be a single `img:<path>` segment: a text label cannot coexist
//!   with it, so thumbnails render without an index. Entries without a
//!   thumbnail fall back to the legacy `text:<id> <label>` shape. Ids
//!   travel through a side-table: the launcher echoes the rendered line
//!   back and [`SelectionMap`] resolves it to the entry id.

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
    /// `render_images` must mirror the launcher's declared image support.
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
                format!("[{:03}] {}", position(map.0.len() + 1), flatten(&entry.preview))
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
        // Tolerant shapes: index stripped from plain lines, segment
        // prefixes stripped from image lines.
        let body = strip_index(line);
        if let Some(id) = self.0.get(body) {
            return Some(*id);
        }
        // Launcher kept only the label of an indexed line.
        for (key, id) in self.0.iter() {
            if !body.is_empty() && strip_index(key) == body {
                return Some(*id);
            }
        }
        let stripped_img = body.strip_prefix("img:").unwrap_or(body);
        if let Some(id) = self.0.get(stripped_img) {
            return Some(*id);
        }
        let stripped_text = stripped_img.strip_prefix("text:").unwrap_or(stripped_img);
        let token = stripped_text
            .split_once('\t')
            .map(|(id, _)| id)
            .unwrap_or_else(|| stripped_text.split_once(' ').map(|(id, _)| id).unwrap_or(stripped_text));
        token.trim().parse::<i64>().ok()
    }
}

fn position(n: usize) -> usize {
    n
}

fn strip_index(line: &str) -> &str {
    match line.strip_prefix('[').and_then(|rest| {
        let end = rest.find(']')?;
        rest[..end]
            .chars()
            .all(|c| c.is_ascii_digit())
            .then_some(end)
    }) {
        // line = "[" + digits + "] " + label → skip bracket+digits+bracket+space
        Some(end) => &line[end + 3..],
        None => line,
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
    fn plain_lines_carry_indexed_labels() {
        let (lines, map) = SelectionMap::build(
            &[item(21, "first", None), item(4, "two\nlines\there", None)],
            false,
        );
        assert_eq!(lines[0].0, "[001] first");
        assert_eq!(lines[1].0, "[002] two\\nlines  here");
        assert_eq!(map.resolve(&lines[1].0), Some(4));
    }

    #[test]
    fn image_thumbnails_stay_a_single_segment() {
        // Restored wofi ≤ 1.5 contract: no index may ride along.
        let (lines, map) =
            SelectionMap::build(&[item(21, "[image png]", Some("/thumbs/a.png"))], true);
        assert_eq!(lines[0].0, "img:/thumbs/a.png");
        assert_eq!(lines[0].0.split_whitespace().count(), 1);
        assert_eq!(map.resolve(&lines[0].0), Some(21));
    }

    #[test]
    fn image_mode_without_thumbnail_uses_legacy_text_shape() {
        let (lines, _) = SelectionMap::build(&[item(4, "plain", None)], true);
        assert_eq!(lines[0].0, "text:4 plain");
        assert_eq!(
            SelectionMap::build(&[item(4, "plain", None)], true)
                .1
                .resolve("text:4 plain"),
            Some(4)
        );
    }

    #[test]
    fn resolution_survives_mangled_echoes() {
        let (_, map) = SelectionMap::build(
            &[
                item(9, "with thumb", Some("/t.png")),
                item(5, "plain one", None),
                item(6, "plain one", None), // duplicate preview!
            ],
            true,
        );
        assert_eq!(map.resolve("img:/t.png\n"), Some(9));
        assert_eq!(map.resolve("text:5 plain one"), Some(5));
        // Duplicate previews keep distinct ids via their text: prefix.
        assert_eq!(map.resolve("text:6 plain one"), Some(6));
        assert_eq!(map.resolve(""), None);
        assert_eq!(map.resolve("unknown"), None);
    }

    #[test]
    fn plain_dialect_tolerates_dropped_index() {
        let (_, map) = SelectionMap::build(
            &[item(21, "first", None), item(4, "second", None)],
            false,
        );
        // Launcher stripped the [NNN] chrome and echoed only the label.
        assert_eq!(map.resolve("second"), Some(4));
    }

}
