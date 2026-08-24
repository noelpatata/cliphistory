//! Single source of truth for every cliphistory constant.
//!
//! Values that alter *behaviour* at runtime belong in [`crate::config`];
//! everything immutable belongs here.

/// Protocol version this build of the core speaks.
#[allow(unused_imports)]
pub use cliphistory_proto::PROTOCOL_VERSION;

// ---------------------------------------------------------------------------
// Identity & filesystem layout
// ---------------------------------------------------------------------------

pub const APP_NAME: &str = "cliphistory";

pub const CONFIG_SUBDIR: &str = "cliphistory";
pub const DATA_SUBDIR: &str = "cliphistory";
pub const RUNTIME_SUBDIR: &str = "cliphistory";
pub const CONFIG_FILENAME: &str = "config.toml";
pub const DB_FILENAME: &str = "history.db";
pub const THUMBS_DIRNAME: &str = "thumbs";
pub const SOCKET_FILENAME: &str = "cliphistory.sock";
pub const MODULES_DIRNAME: &str = "modules";
pub const RELEASE_MANIFEST_CACHE_FILENAME: &str = "release-manifest.json";
pub const CURRENT_LINK_NAME: &str = "current";

pub const MODULE_BINARY_PERMS: u32 = 0o755;

// ---------------------------------------------------------------------------
// IPC
// ---------------------------------------------------------------------------

/// Images travel base64-encoded inside single NDJSON lines; keep generous headroom.
pub const MAX_IPC_LINE_BYTES: usize = 33_554_432;
pub const DEFAULT_HISTORY_LIMIT: usize = 20;
pub const SHOW_ENTRIES_LIMIT: usize = 100;

// ---------------------------------------------------------------------------
// Storage defaults (config may override)
// ---------------------------------------------------------------------------

pub const DEFAULT_MAX_ENTRIES: i64 = 500;
pub const DEFAULT_MAX_ITEM_SIZE_BYTES: i64 = 5 * 1024 * 1024;
/// 0 disables age-based pruning.
pub const DEFAULT_MAX_AGE_DAYS: i64 = 0;
/// Longest edge of generated image previews; 0 disables thumbnails.
pub const DEFAULT_THUMBNAIL_SIZE: u32 = 256;
/// Hard ceiling on summed entry payloads; pruning evicts oldest unpinned
/// entries until the total fits. This is what actually bounds disk usage —
/// an entry-count cap alone cannot (N x max_item_size grows unbounded).
pub const DEFAULT_MAX_TOTAL_BYTES: i64 = 256 * 1024 * 1024; // 256 MiB

// ---------------------------------------------------------------------------
// Auto-paste (runs after a selection is written back to the clipboard)
// ---------------------------------------------------------------------------

/// Grace period so the clipboard module owns the selection before the
/// target application reads it (config `general.paste_delay_ms`).
pub const PASTE_DELAY_MS: u64 = 150;

// ---------------------------------------------------------------------------
// Paster module discovery priorities
// ---------------------------------------------------------------------------

pub const PASTER_CANDIDATES_WAYLAND: &[&str] = &["paster-uinput", "paster-wayland"];
pub const PASTER_CANDIDATES_X11: &[&str] = &["paster-uinput"];

pub const SECS_PER_DAY: u64 = 86_400;

// ---------------------------------------------------------------------------
// Networking / module downloads
// ---------------------------------------------------------------------------

pub const USER_AGENT: &str = concat!("cliphistory/", env!("CARGO_PKG_VERSION"));
pub const CONNECT_TIMEOUT_SECS: u64 = 10;
pub const DOWNLOAD_TIMEOUT_SECS: u64 = 120;
pub const MAX_RELEASE_MANIFEST_BYTES: usize = 4 * 1024 * 1024;

pub const DEFAULT_SOURCE_URL: &str = "https://github.com/noelpatata/cliphistory";
pub const DEFAULT_CHANNEL: &str = "stable";
pub const CHANNEL_STABLE: &str = "stable";

// ---------------------------------------------------------------------------
// Discovery candidate priorities (first match wins, config can override)
// ---------------------------------------------------------------------------

pub const CLIPBOARD_CANDIDATES_WAYLAND: &[&str] = &["clipboard-wayland"];
pub const CLIPBOARD_CANDIDATES_X11: &[&str] = &["clipboard-x11"];
pub const FRONTEND_CANDIDATES: &[&str] = &["frontend-generic"];

// ---------------------------------------------------------------------------
// Engine tuning
// ---------------------------------------------------------------------------

/// Delay before respawning a crashed reader module.
/// (Reader-internal sampling intervals live with the modules themselves.)
pub const CLIPBOARD_RESPAWN_BACKOFF_MS: u64 = 2_000;
/// Give up on a reader that cannot be spawned / keeps dying.
pub const CLIPBOARD_MAX_SPAWN_ATTEMPTS: u32 = 3;
pub const CLIPBOARD_MAX_RUNTIME_RESTARTS: u32 = 5;

// ---------------------------------------------------------------------------
// Environment variables consulted during discovery
// ---------------------------------------------------------------------------

pub const ENV_SESSION_TYPE: &str = "XDG_SESSION_TYPE";
pub const ENV_WAYLAND_DISPLAY: &str = "WAYLAND_DISPLAY";
pub const ENV_DISPLAY: &str = "DISPLAY";

// ---------------------------------------------------------------------------
// os-release parsing
// ---------------------------------------------------------------------------

pub const OS_RELEASE_PATH: &str = "/etc/os-release";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_matches_proto() {
        assert_eq!(PROTOCOL_VERSION, cliphistory_proto::PROTOCOL_VERSION);
    }

    // Compile-time documentation of intent rather than a real invariant check.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn sane_defaults() {
        assert!(DEFAULT_MAX_ENTRIES > 0);
        assert!(DEFAULT_MAX_ITEM_SIZE_BYTES > 1024);
        assert!(!CLIPBOARD_CANDIDATES_WAYLAND.is_empty());
        assert!(!FRONTEND_CANDIDATES.is_empty());
    }
}
