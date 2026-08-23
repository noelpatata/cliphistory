//! Thumbnail cache: `<content-hash>.png` previews next to the database.

use super::Storage;
use anyhow::Result;
use std::path::PathBuf;

impl Storage {
    /// Directory holding cached previews (`None` for in-memory DBs).
    pub fn thumbs_dir(&self) -> Option<PathBuf> {
        self.path.parent().map(|p| p.join(crate::constants::THUMBS_DIRNAME))
    }

    pub(crate) fn thumb_path(&self, hash: &str) -> Option<PathBuf> {
        self.thumbs_dir().map(|d| d.join(format!("{hash}.png")))
    }

    /// Generate the preview for `bytes` unless already cached. Failures are
    /// logged by the caller; they never lose the entry itself.
    pub(crate) fn ensure_thumbnail(&self, hash: &str, bytes: &[u8], max_dim: u32) -> Result<()> {
        use cliphistory_image_utils as iu;
        let Some(path) = self.thumb_path(hash) else {
            return Ok(());
        };
        if std::fs::metadata(&path)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
        {
            return Ok(());
        }
        let thumb = iu::thumbnail_png(bytes, max_dim)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Atomic-ish write so a concurrent frontend never reads half a PNG.
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, &thumb)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub(crate) fn remove_thumb(&self, hash: &str) {
        if let Some(path) = self.thumb_path(hash) {
            let _ = std::fs::remove_file(path);
        }
    }
}
