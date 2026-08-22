//! cliphistory wire protocol.
//!
//! Everything that crosses a process boundary between the cliphistory core and
//! one of its modules (readers, frontends) is defined here: content types,
//! NDJSON envelopes, history items and module manifests. The core and every
//! module must agree on [`PROTOCOL_VERSION`].

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Version of the module wire protocol.
///
/// * Core refuses to spawn modules reporting a different major value.
/// * Release manifests carry this value so incompatible artifacts are never
///   downloaded in the first place.
pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Module kinds / capabilities
// ---------------------------------------------------------------------------

pub const KIND_READER: &str = "reader";
pub const KIND_FRONTEND: &str = "frontend";

pub const CAP_READ: &str = "read";
pub const CAP_WRITE: &str = "write";

/// Maximum number of characters kept in an entry preview.
pub const PREVIEW_MAX_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------

/// A single clipboard payload. Images travel base64-encoded over stdio.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    Image {
        mime: String,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
        width: Option<u32>,
        height: Option<u32>,
    },
}

impl Content {
    pub fn kind(&self) -> &'static str {
        match self {
            Content::Text { .. } => "text",
            Content::Image { .. } => "image",
        }
    }

    pub fn mime(&self) -> &str {
        match self {
            Content::Text { .. } => "text/plain;charset=utf-8",
            Content::Image { mime, .. } => mime,
        }
    }

    pub fn bytes(&self) -> Cow<'_, [u8]> {
        match self {
            Content::Text { text } => Cow::Borrowed(text.as_bytes()),
            Content::Image { data, .. } => Cow::Borrowed(data),
        }
    }

    pub fn size(&self) -> usize {
        self.bytes().len()
    }

    /// One-line human readable preview used by storage, frontends and logs.
    pub fn preview(&self) -> String {
        match self {
            Content::Text { text } => {
                truncate_chars(&text.replace(['\n', '\r', '\t'], "\\n"), PREVIEW_MAX_CHARS)
            }
            Content::Image {
                mime,
                data,
                width,
                height,
            } => {
                let mut s = format!("[image {} {}]", short_mime(mime), human_size(data.len()));
                if let (Some(w), Some(h)) = (width, height) {
                    s.push_str(&format!(" {}x{}", w, h));
                }
                s
            }
        }
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

fn short_mime(mime: &str) -> String {
    let base = mime.split(';').next().unwrap_or(mime);
    base.trim().to_string()
}

pub fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1}MB", b / MB)
    } else if b >= KB {
        format!("{:.1}KB", b / KB)
    } else {
        format!("{bytes}B")
    }
}

mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// History items (storage -> frontends)
// ---------------------------------------------------------------------------

/// Metadata-only view of a stored entry; full bytes never leave the daemon
/// until something is selected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryItem {
    pub id: i64,
    pub kind: String,
    pub mime: String,
    pub preview: String,
    pub size_bytes: u64,
    pub created_at: u64,
    pub use_count: u64,
    pub pinned: bool,
}

// ---------------------------------------------------------------------------
// Reader <-> host envelopes (NDJSON over stdio)
// ---------------------------------------------------------------------------

/// Frames sent by a reader module to the core on stdout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReaderToHost {
    /// First frame after startup; confirms protocol compatibility.
    Ready { protocol_version: u32 },
    /// Clipboard changed.
    Event { content: Content },
    /// Reply to [`HostToReader::Ping`].
    Pong,
    /// Non-fatal error report.
    Error { message: String },
}

/// Frames sent by the core to a reader module on stdin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostToReader {
    Ping,
    Stop,
    /// Ask the reader to take ownership of the clipboard with this payload.
    SetClipboard {
        content: Content,
    },
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
// Module manifest
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleKind {
    Reader,
    Frontend,
}

impl ModuleKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ModuleKind::Reader => KIND_READER,
            ModuleKind::Frontend => KIND_FRONTEND,
        }
    }
}

/// Self-description printed by every module binary via `--manifest`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleManifest {
    /// Stable identifier, e.g. `reader-wayland`, `frontend-rofi`.
    pub id: String,
    pub kind: ModuleKind,
    pub version: String,
    pub protocol_version: u32,
    /// Subset of `read` / `write`.
    pub capabilities: Vec<String>,
    /// External executables this module needs on PATH at runtime.
    pub requires: Vec<String>,
    pub description: String,
}

impl ModuleManifest {
    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }

    pub fn missing_tools<F>(&self, probe: F) -> Vec<String>
    where
        F: Fn(&str) -> bool,
    {
        self.requires
            .iter()
            .filter(|t| !probe(t))
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_text_roundtrip() {
        let c = Content::Text {
            text: "héllo\nworld".into(),
        };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Content>(&json).unwrap(), c);
    }

    #[test]
    fn content_image_base64_roundtrip() {
        let c = Content::Image {
            mime: "image/png".into(),
            data: vec![0, 1, 2, 250, 251],
            width: Some(3),
            height: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("data"));
        assert!(!json.contains("AQH")); // raw bytes must not leak as utf8
        assert_eq!(serde_json::from_str::<Content>(&json).unwrap(), c);
    }

    #[test]
    fn reader_envelopes_roundtrip() {
        let frames = [
            ReaderToHost::Ready {
                protocol_version: PROTOCOL_VERSION,
            },
            ReaderToHost::Event {
                content: Content::Text { text: "x".into() },
            },
            ReaderToHost::Pong,
            ReaderToHost::Error {
                message: "boom".into(),
            },
        ];
        for f in frames {
            let line = serde_json::to_string(&f).unwrap();
            assert_eq!(serde_json::from_str::<ReaderToHost>(&line).unwrap(), f);
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
    fn previews_are_flattened_and_truncated() {
        let long: String = "ab".repeat(500);
        let p = Content::Text { text: long }.preview();
        assert!(p.chars().count() <= PREVIEW_MAX_CHARS + 1);

        let img = Content::Image {
            mime: "image/png".into(),
            data: vec![0u8; 4096],
            width: Some(1920),
            height: Some(1080),
        };
        assert_eq!(img.preview(), "[image image/png 4.0KB] 1920x1080");
    }

    #[test]
    fn manifest_missing_tools() {
        let m = ModuleManifest {
            id: "reader-x11".into(),
            kind: ModuleKind::Reader,
            version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec![CAP_READ.into(), CAP_WRITE.into()],
            requires: vec!["xclip".into()],
            description: String::new(),
        };
        assert_eq!(m.missing_tools(|_| true), Vec::<String>::new());
        assert_eq!(m.missing_tools(|_| false), vec!["xclip".to_string()]);
    }
}
