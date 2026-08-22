//! Image helpers for cliphistory.
//!
//! One responsibility: turning image bytes into facts (dimensions) and
//! derived artifacts (thumbnails). Knows nothing about clipboards, storage
//! or modules.

use anyhow::{Context, Result};
use image::imageops::FilterType;
use std::io::Cursor;

/// Decode enough of `bytes` to report `(width, height)`.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let img = image::load_from_memory(bytes).ok()?;
    Some((img.width(), img.height()))
}

/// Render a PNG thumbnail whose longest edge is at most `max_dim`,
/// preserving aspect ratio. Never upscales. Input formats follow the
/// crate's enabled decoders (png/jpeg/gif/webp).
pub fn thumbnail_png(bytes: &[u8], max_dim: u32) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes).context("decoding image")?;
    let thumbnail = if img.width() > max_dim || img.height() > max_dim {
        img.resize(max_dim, max_dim, FilterType::Lanczos3)
    } else {
        img // small enough already; re-encode for a uniform format
    };
    let mut out = Vec::new();
    thumbnail
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .context("encoding thumbnail png")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbImage::new(w, h);
        for (_, _, px) in img.enumerate_pixels_mut() {
            *px = Rgb([200, 10, 10]);
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn probes_dimensions() {
        assert_eq!(dimensions(&png_bytes(1665, 937)), Some((1665, 937)));
        assert_eq!(dimensions(b"not an image"), None);
    }

    #[test]
    fn thumbnail_preserves_aspect_and_caps() {
        let thumb = thumbnail_png(&png_bytes(1665, 937), 256).unwrap();
        let (w, h) = dimensions(&thumb).unwrap();
        assert_eq!((w, h), (256, 144)); // 1665x937 scaled by 256/1665

        let thumb = thumbnail_png(&png_bytes(100, 50), 256).unwrap();
        let (w, h) = dimensions(&thumb).unwrap();
        assert_eq!((w, h), (100, 50)); // never upscales
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(thumbnail_png(b"junk", 256).is_err());
    }
}
