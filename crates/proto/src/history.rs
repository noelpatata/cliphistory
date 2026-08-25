//! Metadata-only view of a stored entry (storage -> frontends).

use serde::{Deserialize, Serialize};

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
    /// When the entry was last pinned (epoch seconds); `None` while
    /// unpinned. Drives pin-date ordering in frontends.
    #[serde(default)]
    pub pinned_at: Option<u64>,
    /// Absolute path to a cached downscaled PNG preview (images only).
    #[serde(default)]
    pub thumbnail: Option<String>,
}
