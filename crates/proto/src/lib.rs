//! cliphistory wire protocol.
//!
//! Everything that crosses a process boundary between the cliphistory core
//! and one of its modules (readers, frontends, pasters) is defined here:
//! content types, NDJSON envelopes, history items and module manifests. The
//! core and every module must agree on [`PROTOCOL_VERSION`].
//!
//! Organized by concern:
//! * [`content`]  — clipboard payloads and preview formatting
//! * [`history`]  — metadata-only history items
//! * [`frames`]   — the three NDJSON envelope pairs
//! * [`ipc`]      — daemon/client frames and socket transport helpers
//! * [`manifest`] — module kinds, capabilities, manifests

pub mod content;
pub mod frames;
pub mod history;
pub mod ipc;
pub mod manifest;

// ---------------------------------------------------------------------------
// Protocol version & capability vocabulary
// ---------------------------------------------------------------------------

/// Version of the module wire protocol.
///
/// * Core refuses to spawn modules reporting a different major value.
/// * Release manifests carry this value so incompatible artifacts are never
///   downloaded in the first place.
pub const PROTOCOL_VERSION: u32 = 2;

pub use manifest::{KIND_CLIPBOARD, KIND_FRONTEND, KIND_PASTER};

/// Capability strings used in module manifests.
pub const CAP_READ: &str = "read";
pub const CAP_WRITE: &str = "write";

/// Optional module features. Frontends may declare `"images"` when they can
/// render `HistoryItem.thumbnail` entries.
pub const FEATURE_IMAGES: &str = "images";

// ---------------------------------------------------------------------------
// Re-exports (stable root paths for every consumer)
// ---------------------------------------------------------------------------

pub use content::{human_size, Content};
pub use frames::{
    ClipboardToHost, HostToClipboard, HostToPaster, KeyBindings, PasterToHost, ShowRequest,
    ShowResponse, ViewOptions, DEFAULT_FONT_SIZE, DEFAULT_MAX_PREVIEW_LINES,
    DEFAULT_WINDOW_WIDTH,
};
pub use history::HistoryItem;
pub use ipc::{
    read_response, write_request, DaemonStatus, IpcRequest, IpcResponse, ModuleInfo,
    MAX_IPC_LINE_BYTES,
};
pub use manifest::{ModuleKind, ModuleManifest};

#[cfg(test)]
pub use content::PREVIEW_MAX_CHARS;
