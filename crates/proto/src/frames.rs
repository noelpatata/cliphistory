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

/// Single JSON document piped to a frontend's stdin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowRequest {
    pub entries: Vec<HistoryItem>,
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
}
