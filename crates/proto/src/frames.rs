//! NDJSON envelope pairs: reader <-> host, frontend <-> host, paster <-> host.

use crate::content::Content;
use crate::history::HistoryItem;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Reader <-> host
// ---------------------------------------------------------------------------

/// Frames sent by a reader module to the core on stdout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipboardToHost {
    /// First frame after startup; confirms protocol compatibility.
    Ready { protocol_version: u32 },
    /// Clipboard changed.
    Event { content: Content },
    /// Reply to [`HostToClipboard::Ping`].
    Pong,
    /// Non-fatal error report.
    Error { message: String },
}

/// Frames sent by the core to a reader module on stdin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostToClipboard {
    Ping,
    Stop,
    /// Ask the reader to take ownership of the clipboard with this payload.
    SetClipboard { content: Content },
}

// ---------------------------------------------------------------------------
// Frontend <-> host
// ---------------------------------------------------------------------------

/// Presentation hints the daemon attaches to a show request.
///
/// Frontends read what they support and ignore the rest; every field has a
/// default so requests from older daemons still parse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewOptions {
    /// Maximum lines rendered per text entry in graphical frontends.
    /// `0` disables the cap.
    #[serde(default = "default_max_preview_lines")]
    pub max_preview_lines: usize,
    /// Optional extra font for graphical frontends, either an installed
    /// font family name (e.g. `"JetBrainsMono Nerd Font"`) or a direct
    /// path to a `.ttf`/`.otf` file. When set it becomes the primary text
    /// face; when unset, frontends may auto-detect an installed Nerd Font
    /// purely as glyph fallback.
    #[serde(default)]
    pub font_family: Option<String>,
    /// Wrap long preview lines at the picker window's edge instead of
    /// extending them past the viewport behind a horizontal scrollbar.
    /// Only honoured by graphical frontends; `false` keeps the old
    /// extend-and-scroll behaviour.
    #[serde(default)]
    pub word_wrap: bool,
    /// Base body text size (px) for graphical frontends; derived styles
    /// (monospace, small) stay proportionally smaller. `0` falls back to
    /// [`DEFAULT_FONT_SIZE`].
    #[serde(default = "default_font_size")]
    pub font_size: u32,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            max_preview_lines: DEFAULT_MAX_PREVIEW_LINES,
            font_family: None,
            word_wrap: false,
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

/// Cap applied when a request carries no explicit view options.
pub const DEFAULT_MAX_PREVIEW_LINES: usize = 8;

/// Body text size used when a request carries no explicit `font_size`.
pub const DEFAULT_FONT_SIZE: u32 = 16;

fn default_max_preview_lines() -> usize {
    DEFAULT_MAX_PREVIEW_LINES
}

fn default_font_size() -> u32 {
    DEFAULT_FONT_SIZE
}

/// Single JSON document piped to a frontend's stdin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowRequest {
    pub entries: Vec<HistoryItem>,
    /// How the frontend should present the entries. Optional on the wire.
    #[serde(default)]
    pub view: ViewOptions,
}

/// Single JSON document printed by a frontend on stdout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ShowResponse {
    Selected { id: i64 },
    Delete { id: i64 },
    Clear,
    Dismissed,
}

// ---------------------------------------------------------------------------
// Paster <-> host
// ---------------------------------------------------------------------------

/// Frames sent by a paster module to the core on stdout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PasterToHost {
    Ready { protocol_version: u32 },
    /// Reply to [`HostToPaster::Ping`].
    Pong,
    Error { message: String },
}

/// Frames sent by the core to a paster module on stdin.
///
/// The paste chord itself is module business; the core stays
/// display-server agnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostToPaster {
    Ping,
    Stop,
    /// Replay the module's paste chord into the focused surface now that
    /// the clipboard module owns the selection.
    Paste,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::Content;
    use crate::PROTOCOL_VERSION;

    #[test]
    fn reader_envelopes_roundtrip() {
        let frames = [
            ClipboardToHost::Ready {
                protocol_version: PROTOCOL_VERSION,
            },
            ClipboardToHost::Event {
                content: Content::Text { text: "x".into() },
            },
            ClipboardToHost::Pong,
            ClipboardToHost::Error {
                message: "boom".into(),
            },
        ];
        for f in frames {
            let line = serde_json::to_string(&f).unwrap();
            assert_eq!(serde_json::from_str::<ClipboardToHost>(&line).unwrap(), f);
        }
    }

    #[test]
    fn show_response_tags() {
        let r = ShowResponse::Selected { id: 42 };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"outcome":"selected","id":42}"#);
        assert_eq!(serde_json::from_str::<ShowResponse>(&json).unwrap(), r);
    }

    #[test]
    fn show_request_defaults_view_options() {
        // Legacy daemons omit the view section entirely.
        let legacy = r#"{"entries":[]}"#;
        let req: ShowRequest = serde_json::from_str(legacy).unwrap();
        assert_eq!(req.view, ViewOptions::default());
        assert_eq!(req.view.max_preview_lines, DEFAULT_MAX_PREVIEW_LINES);
        assert_eq!(req.view.font_family, None);
        assert!(!req.view.word_wrap);
        assert_eq!(req.view.font_size, DEFAULT_FONT_SIZE);
    }

    #[test]
    fn show_request_roundtrips_view_options() {
        let req = ShowRequest {
            entries: vec![],
            view: ViewOptions {
                max_preview_lines: 3,
                font_family: Some("Symbols Nerd Font".into()),
                word_wrap: true,
                font_size: 18,
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<ShowRequest>(&json).unwrap(), req);
    }
}
