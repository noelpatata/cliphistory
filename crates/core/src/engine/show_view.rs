//! Presentation formatting for outgoing [`ShowRequest`]s.
//!
//! The database stores a flattened one-line preview per entry; graphical
//! frontends want the real line structure back. This module rebuilds a
//! display preview for text entries from a bounded head of the stored
//! payload, honouring the configured line cap. Keeping it here — instead of
//! in storage or in each frontend — means one testable formatter and config
//! changes that apply to already-stored entries immediately.

use cliphistory_proto::{HistoryItem, ShowRequest, ViewOptions};

/// How many payload bytes are read to build a display preview.
///
/// Generous enough for realistic snippets; keeps even multi-megabyte
/// payloads cheap to preview.
pub(crate) const PREVIEW_HEAD_BYTES: usize = 4096;

/// Build a show request whose text entries carry multi-line display
/// previews.
///
/// `head_of` returns up to [`PREVIEW_HEAD_BYTES`] payload bytes for an id;
/// failures simply keep the stored flattened preview.
pub(crate) fn build_request(
    items: Vec<HistoryItem>,
    view: ViewOptions,
    head_of: &dyn Fn(i64) -> Option<String>,
) -> ShowRequest {
    let mut items = items;
    for item in &mut items {
        if item.kind != "text" {
            continue;
        }
        if let Some(head) = head_of(item.id) {
            item.preview = display_preview(&head, item.size_bytes, view.max_preview_lines);
        }
    }
    ShowRequest {
        entries: items,
        view,
    }
}

/// Render `head` as a display preview: real newlines preserved, capped at
/// `max_lines` (`0` = unlimited), with an honest marker when content was
/// left out.
pub(crate) fn display_preview(head: &str, total_bytes: u64, max_lines: usize) -> String {
    let normalized = head.replace('\r', "");
    let lines: Vec<&str> = normalized.lines().collect();
    let head_bytes = head.len() as u64;
    let truncated_source = head_bytes >= PREVIEW_HEAD_BYTES as u64 && total_bytes > head_bytes;

    if max_lines > 0 && lines.len() > max_lines {
        let hidden = lines.len() - max_lines;
        let mut kept: Vec<String> = lines[..max_lines]
            .iter()
            .map(|l| (*l).to_string())
            .collect();
        kept.push(format!("… (+{hidden} more lines)"));
        return kept.join("\n");
    }

    let mut out = normalized.trim_end_matches('\n').to_string();
    if truncated_source {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("… (truncated)");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants as c;

    fn item(id: i64, kind: &str, preview: &str, size: u64) -> HistoryItem {
        HistoryItem {
            id,
            kind: kind.into(),
            mime: "text/plain".into(),
            preview: preview.into(),
            size_bytes: size,
            created_at: 0,
            use_count: 0,
            pinned: false,
            pinned_at: None,
            thumbnail: None,
        }
    }

    #[test]
    fn caps_lines_with_marker() {
        let raw = "l1\nl2\nl3\nl4\nl5\nl6\nl7";
        assert_eq!(display_preview(raw, 7, 3), "l1\nl2\nl3\n… (+4 more lines)");
    }

    #[test]
    fn exact_fit_is_not_reported_as_overflow() {
        let raw = "l1\nl2\nl3";
        assert_eq!(display_preview(raw, 6, 3), "l1\nl2\nl3");
    }

    #[test]
    fn zero_means_unlimited() {
        let raw = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj";
        assert_eq!(display_preview(raw, 20, 0), raw);
    }

    #[test]
    fn single_line_passes_through() {
        assert_eq!(display_preview("just text", 9, 8), "just text");
    }

    #[test]
    fn carriage_returns_are_dropped_not_shown() {
        assert_eq!(display_preview("a\r\nb\r\n", 10, 0), "a\nb");
    }

    #[test]
    fn cut_head_gets_truncation_marker() {
        // Head filled the budget and the entry is bigger.
        let big_head = "x".repeat(PREVIEW_HEAD_BYTES);
        let out = display_preview(&big_head, PREVIEW_HEAD_BYTES as u64 + 100, 0);
        assert!(out.ends_with("… (truncated)"));

        // Small entries never get marked.
        let out = display_preview("tiny", 4, 0);
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn truncation_marker_respects_line_cap_too() {
        let big_head: String = (0..100).map(|i| format!("line{i}\n")).collect();
        let out = display_preview(&big_head, u64::MAX, 2);
        assert_eq!(out, "line0\nline1\n… (+98 more lines)");
    }

    #[test]
    fn only_text_entries_are_rewritten() {
        let items = vec![
            item(1, "text", "old", 5),
            item(2, "image", "[image png]", 5),
        ];
        let req = build_request(items, ViewOptions::default(), &|id| {
            Some(format!("payload of {id}\nsecond line"))
        });
        assert_eq!(req.entries[0].preview, "payload of 1\nsecond line");
        assert_eq!(req.entries[1].preview, "[image png]");
        assert_eq!(req.view.max_preview_lines, c::DEFAULT_MAX_PREVIEW_LINES);
    }

    #[test]
    fn failed_head_lookup_keeps_stored_preview() {
        let items = vec![item(1, "text", "stored flat", 5)];
        let req = build_request(items, ViewOptions::default(), &|_| None);
        assert_eq!(req.entries[0].preview, "stored flat");
    }

    #[test]
    fn line_cap_applies_at_request_build() {
        let items = vec![item(1, "text", "old", 100)];
        let view = ViewOptions {
            max_preview_lines: 1,
            ..Default::default()
        };
        let req = build_request(items, view, &|_| Some("a\nb\nc".into()));
        assert_eq!(req.entries[0].preview, "a\n… (+2 more lines)");
    }
}
