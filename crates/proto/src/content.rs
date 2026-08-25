//! Clipboard payload types: the data that flows between apps, modules and
//! storage.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Maximum number of characters kept in an entry preview.
pub const PREVIEW_MAX_CHARS: usize = 200;

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
    fn content_size_text() {
        let c = Content::Text {
            text: "hello".into(),
        };
        assert_eq!(c.size(), 5);
    }

    #[test]
    fn content_size_empty_text() {
        let c = Content::Text { text: "".into() };
        assert_eq!(c.size(), 0);
    }

    #[test]
    fn content_size_image() {
        let c = Content::Image {
            mime: "image/png".into(),
            data: vec![0u8; 1024],
            width: None,
            height: None,
        };
        assert_eq!(c.size(), 1024);
    }

    #[test]
    fn content_kind_text() {
        assert_eq!(Content::Text { text: "x".into() }.kind(), "text");
    }

    #[test]
    fn content_kind_image() {
        assert_eq!(
            Content::Image {
                mime: "image/png".into(),
                data: vec![],
                width: None,
                height: None,
            }
            .kind(),
            "image"
        );
    }

    #[test]
    fn content_mime_text() {
        assert_eq!(
            Content::Text { text: "x".into() }.mime(),
            "text/plain;charset=utf-8"
        );
    }

    #[test]
    fn content_mime_image() {
        assert_eq!(
            Content::Image {
                mime: "image/gif".into(),
                data: vec![],
                width: None,
                height: None,
            }
            .mime(),
            "image/gif"
        );
    }

    #[test]
    fn content_bytes_text() {
        let c = Content::Text { text: "hi".into() };
        assert_eq!(c.bytes().as_ref(), b"hi");
    }

    #[test]
    fn content_bytes_image() {
        let c = Content::Image {
            mime: "image/png".into(),
            data: vec![1, 2, 3],
            width: None,
            height: None,
        };
        assert_eq!(c.bytes().as_ref(), &[1, 2, 3]);
    }
}
