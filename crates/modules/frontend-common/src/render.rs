//! Menu line rendering and selection parsing.
//!
//! This module owns the *menu wire format*: how a [`HistoryItem`] becomes a
//! line a menu program understands, and how the menu's echoed selection
//! maps back to an id. Callers (frontend modules) never touch escape
//! sequences or dialect details.
//!
//! Two dialects exist:
//!
//! * **plain**  — `<id>\t<label>` for menus without image support.
//! * **images** — wofi-style segments. wofi parses space-separated
//!   `<mode>:<data>` segments, so every entry is emitted as
//!   `img:<thumb> text:<id> <label>` (or `text:<id> <label>` when there is
//!   no preview). The id is always the first token of the `text:` segment,
//!   which makes parsing trivial and keeps every character printable.

use cliphistory_proto::HistoryItem;

/// One rendered menu entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuLine(pub String);

/// Build the wire line for one entry.
///
/// `render_images` must reflect the frontend's declared `images` feature;
/// when on, entries with a cached preview are emitted as wofi image escapes
/// so capable menus render the thumbnail next to the label.
pub fn display_line(item: &HistoryItem, render_images: bool) -> MenuLine {
    let flat = flatten(&item.preview);
    let line = match (render_images, &item.thumbnail) {
        (true, Some(path)) => format!("img:{path} text:{} {flat}", item.id),
        (true, None) => format!("text:{} {flat}", item.id),
        (false, _) => format!("{}\t{flat}", item.id),
    };
    MenuLine(line)
}

fn flatten(preview: &str) -> String {
    preview
        .replace('\n', "\\n")
        .replace('\r', "")
        .replace('\t', "  ")
}

/// What a menu told us the user picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Id(i64),
    Dismissed,
}

/// Parse the raw line echoed by the menu program.
///
/// Accepts every dialect this crate ever emitted:
/// * image segments — `img:<path> text:<id> <label>`
/// * legacy — `<id>\t<label>` or bare `<id>`
pub fn parse_selection(raw: &str) -> Option<Selection> {
    let line = raw.trim_end_matches(['\n', '\r']);

    // Strip the image segment when present.
    let body = match line.strip_prefix("img:") {
        Some(rest) => rest.split_once(' ')?.1,
        None => line,
    };
    // Strip an explicit text-segment marker when present.
    let body = body.strip_prefix("text:").unwrap_or(body);

    // Id = first whitespace-delimited token.
    let id_token = body
        .split_once('\t')
        .map(|(id, _)| id)
        .unwrap_or_else(|| body.split_once(' ').map(|(id, _)| id).unwrap_or(body));
    id_token.trim().parse::<i64>().ok().map(Selection::Id)
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
    fn plain_dialect_keeps_legacy_tab_format() {
        let line = display_line(&item(7, "two\nlines\there", None), false).0;
        assert_eq!(line, "7\ttwo\\nlines  here");
        assert_eq!(parse_selection(&line), Some(Selection::Id(7)));
    }

    #[test]
    fn image_dialect_leads_with_img_segment() {
        let line = display_line(
            &item(3, "[image image/png 12.0KB]", Some("/thumbs/abc.png")),
            true,
        )
        .0;
        // The mode token must start at column 0 for wofi to recognize it,
        // and every character must be printable.
        assert!(line.starts_with("img:/thumbs/abc.png text:3 [image"));
        assert!(line.chars().all(|c| !c.is_control()));
        assert_eq!(parse_selection(&line), Some(Selection::Id(3)));
    }

    #[test]
    fn image_capability_without_thumb_still_carries_id() {
        let line = display_line(&item(4, "plain", None), true).0;
        assert_eq!(line, "text:4 plain");
        assert_eq!(parse_selection(&line), Some(Selection::Id(4)));
    }

    #[test]
    fn ids_survive_label_noise() {
        // Labels containing colons, tabs-as-spaces, and markup-ish noise
        // must never break id extraction.
        for label in [
            "https://example.com:8080/x",
            "<meta http-equiv=\"x\"> & stuff",
            "42 is the answer",
            "",
        ] {
            let line = display_line(&item(9, label, Some("/t.png")), true).0;
            assert_eq!(parse_selection(&line), Some(Selection::Id(9)), "{line}");
        }
    }

    #[test]
    fn legacy_formats_still_parse() {
        assert_eq!(parse_selection("42\thello world"), Some(Selection::Id(42)));
        assert_eq!(parse_selection("42 hello"), Some(Selection::Id(42)));
        assert_eq!(parse_selection("42"), Some(Selection::Id(42)));
        assert_eq!(parse_selection(""), None);
        assert_eq!(parse_selection("x\ty"), None);
    }
}
