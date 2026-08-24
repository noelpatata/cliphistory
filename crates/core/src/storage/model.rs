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

impl InsertOutcome {
    /// The entry id when something was stored or promoted.
    pub fn id(&self) -> Option<i64> {
        match self {
            InsertOutcome::Inserted(id) | InsertOutcome::Duplicate(id) => Some(*id),
            InsertOutcome::TooLarge { .. } => None,
        }
    }
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

/// Prefix of a stored payload, for frontends that render more than the
/// flattened one-line preview without loading full blobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentHead {
    /// Entry kind as stored (`text` / `image`).
    pub kind: String,
    /// First bytes of the payload, lossily decoded.
    pub text: String,
}
