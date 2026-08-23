//! Menu line rendering and selection parsing.
//!
//! This module owns the *menu wire format*: how a [`HistoryItem`] becomes a
//! line a menu program understands, and how the menu's echoed selection
//! maps back to an id. Callers (frontend modules) never touch escape
//! sequences or sentinels.
//!
//! Two dialects exist:
//!
//! * **plain** — `<id>\t<label>` for menus without image support.
//! * **images** — wofi-style segments (`img:<path> text:<label>`) where the
//!   label carries the id wrapped in unit-separator sentinels:
//!   `text:#{id}#{label}`. The separator is invisible, cannot appear in
//!   ids, and survives arbitrary preview text.

use cliphistory_proto::HistoryItem;

/// Invisible ASCII unit separator delimiting the id inside image-mode labels.
pub const ID_SENTINEL: char = '\u{1f}';

/// One rendered menu entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuLine(pub String);

/// Build the wire line for one entry.
///
/// `render_images` must reflect the frontend's declared `images` feature;
/// when on, thumbnails are emitted as wofi image escapes so capable menus
/// render the cached preview next to the label.
pub fn display_line(item: &HistoryItem, render_images: bool) -> MenuLine {
    let flat = flatten(&item.preview);
    let line = match (render_images, &item.thumbnail) {
        (true, Some(path)) => format!(
            "img:{path} text:{ID_SENTINEL}{0}{ID_SENTINEL}{1}",
            item.id, flat
        ),
        (true, None) => format!("text:{ID_SENTINEL}{0}{ID_SENTINEL}{1}", item.id, flat),
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
/// Accepts every dialect this crate ever emitted: sentinel-wrapped,
/// legacy `<id>\t<label>`, and bare `<id>`.
pub fn parse_selection(raw: &str) -> Option<Selection> {
    let line = raw.trim_end_matches(['\n', '\r']);

    if let Some(rest) = line.strip_prefix("text:") {
        return parse_sentinel(rest);
    }
    if let Some((first, _)) = line.split_once(' ') {
        // `img:<path> text:#<id>#<label>` echoes both segments.
        if first.starts_with("img:") {
            if let Some(after) = line.split_once("text:").map(|(_, r)| r) {
                return parse_sentinel(after);
            }
        }
    }

    // Legacy tab format.
    if let Some((id, _)) = line.split_once('\t') {
        return id.parse::<i64>().ok().map(Selection::Id);
    }
    if let Some((id, _)) = line.split_once(' ') {
        return id.parse::<i64>().ok().map(Selection::Id);
    }
    line.parse::<i64>().ok().map(Selection::Id)
}

fn parse_sentinel(body: &str) -> Option<Selection> {
    let rest = body.strip_prefix(ID_SENTINEL)?;
    let end = rest.find(ID_SENTINEL)?;
    rest[..end].parse::<i64>().ok().map(Selection::Id)
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
        assert_eq!(
            parse_selection(&line),
            Some(Selection::Id(7)),
            "menu echoes the line back verbatim"
        );
    }

    #[test]
    fn image_dialect_emits_escapes_and_sentinel_ids() {
        let line = display_line(
            &item(3, "[image image/png 12.0KB]", Some("/thumbs/abc.png")),
            true,
        )
        .0;
        assert!(line.starts_with("img:/thumbs/abc.png text:\u{1f}3\u{1f}"));
        // wofi echoes the whole line; the sentinel survives any label noise.
        let noisy = format!("{line} <b>& junk");
        assert_eq!(parse_selection(&noisy), Some(Selection::Id(3)));
    }

    #[test]
    fn image_capability_without_thumb_still_carries_id() {
        let line = display_line(&item(4, "plain", None), true).0;
        assert_eq!(line, "text:\u{1f}4\u{1f}plain");
        assert_eq!(parse_selection(&line), Some(Selection::Id(4)));
    }

    #[test]
    fn previews_with_colons_do_not_confuse_parsing() {
        let line = display_line(&item(9, "https://example.com:8080/x", Some("/t.png")), true).0;
        assert_eq!(parse_selection(&line), Some(Selection::Id(9)));
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
