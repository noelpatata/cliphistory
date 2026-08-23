//! Per-insert behaviour knobs and outcomes.

/// Outcome of [`crate::storage::Storage::insert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    /// New entry stored with this id.
    Inserted(i64),
    /// Already existed; promoted to top, id unchanged.
    Duplicate(i64),
    /// Larger than `max_item_size`; ignored.
    TooLarge { size: i64, limit: i64 },
}

/// Per-insert behaviour knobs.
#[derive(Debug, Clone, Copy)]
pub struct InsertOpts {
    pub max_item_size: i64,
    /// Longest edge allowed for cached previews; 0 disables thumbnails.
    pub thumbnail_size: u32,
}

impl InsertOpts {
    /// Size cap only — used by unit tests.
    pub fn sized(max_item_size: i64) -> Self {
        Self {
            max_item_size,
            thumbnail_size: 0,
        }
    }
}
