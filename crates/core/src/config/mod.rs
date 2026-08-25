//! TOML configuration: everything that may alter behaviour at runtime.

use crate::constants as c;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) mod template;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub storage: StorageConfig,
    pub discovery: DiscoveryConfig,
    pub modules: ModulesConfig,
    pub frontend: FrontendConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// `trace|debug|info|warn|error`. `RUST_LOG` overrides.
    pub log_level: String,
    /// Replay the paste shortcut into the focused window after a selection
    /// is written back. Uses the discovered paster module (native, no
    /// external tools). Disable to make cliphistory copy-only.
    pub auto_paste: bool,
    /// Milliseconds between clipboard ownership and paste injection.
    pub paste_delay_ms: u64,
    /// Expert override: run this shell command instead of the paster module
    /// when set (e.g. a custom wtype invocation).
    pub paste_command: Option<String>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
            auto_paste: true,
            paste_delay_ms: c::PASTE_DELAY_MS,
            paste_command: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// `~` is expanded. Empty/missing falls back to the XDG data dir.
    pub db_path: PathBuf,
    /// Entries kept (pinned entries are exempt from all pruning).
    pub max_entries: i64,
    /// Payloads larger than this many bytes are ignored.
    pub max_item_size: i64,
    /// Entries older than this are pruned; 0 disables age pruning.
    pub max_age_days: i64,
    /// Hard ceiling for summed entry payloads (bytes); pruning evicts the
    /// oldest unpinned entries until the total fits. 0 = unlimited.
    pub max_total_bytes: i64,
    /// Longest edge of cached image previews, in pixels. 0 disables
    /// thumbnail generation entirely.
    pub thumbnail_size: u32,
    /// Byte budget of the hot in-memory payload cache that fronts the
    /// database (speeds up repeated pastes). 0 disables caching.
    pub max_cache_bytes: i64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: PathBuf::new(),
            max_entries: c::DEFAULT_MAX_ENTRIES,
            max_item_size: c::DEFAULT_MAX_ITEM_SIZE_BYTES,
            max_age_days: c::DEFAULT_MAX_AGE_DAYS,
            max_total_bytes: c::DEFAULT_MAX_TOTAL_BYTES,
            thumbnail_size: c::DEFAULT_THUMBNAIL_SIZE,
            max_cache_bytes: c::DEFAULT_MAX_CACHE_BYTES,
        }
    }
}

impl StorageConfig {
    /// Resolved database path (`~` expanded, default applied).
    pub fn resolved_db_path(&self) -> PathBuf {
        if self.db_path.as_os_str().is_empty() {
            data_dir().join(c::DB_FILENAME)
        } else {
            expand_path(&self.db_path)
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoveryConfig {
    /// Force a specific module id, e.g. `clipboard-wayland`. Empty = auto.
    pub preferred_clipboard: Option<String>,
    /// Force a specific frontend id, e.g. `frontend-generic`. Empty = auto.
    pub preferred_frontend: Option<String>,
    /// Force a specific paster id, e.g. `paster-uinput`. Empty = auto.
    pub preferred_paster: Option<String>,
    /// Fail instead of falling back when requirements are unmet.
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModulesConfig {
    /// Repository whose GitHub Releases ship modules. `file://` and plain
    /// local paths are honoured for offline/dev installs.
    pub source_url: String,
    /// `stable` (latest release) or an explicit tag via `modules.pins`.
    pub channel: String,
    /// Refresh modules on daemon start when a newer release exists.
    pub auto_update: bool,
    /// Where modules are installed (`~` expanded).
    pub install_dir: PathBuf,
    /// Development override: directory containing freshly built module
    /// binaries; disables downloads entirely.
    pub local_dir: Option<PathBuf>,
    /// Pin individual modules to tags, e.g. `clipboard-wayland = "v0.2.1"`.
    pub pins: std::collections::BTreeMap<String, String>,
    /// Force a release-manifest target triple (e.g. musl on glibc systems).
    pub platform_override: Option<String>,
}

impl Default for ModulesConfig {
    fn default() -> Self {
        Self {
            source_url: c::DEFAULT_SOURCE_URL.into(),
            channel: c::DEFAULT_CHANNEL.into(),
            auto_update: false,
            install_dir: PathBuf::new(),
            local_dir: None,
            pins: Default::default(),
            platform_override: None,
        }
    }
}

impl ModulesConfig {
    pub fn resolved_install_dir(&self) -> PathBuf {
        match &self.local_dir {
            Some(d) => expand_path(d),
            None if self.install_dir.as_os_str().is_empty() => data_dir().join(c::MODULES_DIRNAME),
            None => expand_path(&self.install_dir),
        }
    }

    pub fn uses_local_dir(&self) -> bool {
        self.local_dir.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FrontendConfig {
    /// Extra CLI arguments appended to every frontend invocation.
    pub extra_args: Vec<String>,
    /// Lines rendered per text entry in the embedded graphical picker.
    /// Long snippets stay browsable without flooding the window.
    /// `0` disables the cap.
    pub max_preview_lines: usize,
    /// Extra font for the embedded picker: an installed font family name
    /// (e.g. `"JetBrainsMono Nerd Font"`) or a direct path to a
    /// `.ttf`/`.otf`. When set, the picker renders in that font; unset =
    /// default fonts plus an auto-detected Nerd Font as glyph fallback.
    pub font_family: Option<String>,
    /// Wrap long preview lines at the picker window's edge instead of
    /// extending them past the viewport behind a horizontal scrollbar.
    pub word_wrap: bool,
    /// Base text size of the embedded picker, in pixels. The monospace
    /// style (index tokens) stays two pixels smaller than this.
    pub font_size: u32,
    /// Configurable key bindings for the picker.
    #[serde(default)]
    pub keys: cliphistory_proto::KeyBindings,
}

impl Default for FrontendConfig {
    fn default() -> Self {
        Self {
            extra_args: Vec::new(),
            max_preview_lines: c::DEFAULT_MAX_PREVIEW_LINES,
            font_family: None,
            word_wrap: false,
            font_size: c::DEFAULT_FONT_SIZE,
            keys: Default::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join(c::CONFIG_SUBDIR)
}

pub fn config_path() -> PathBuf {
    config_dir().join(c::CONFIG_FILENAME)
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp").join(format!("{}-data", c::APP_NAME)))
        .join(c::DATA_SUBDIR)
}

/// Directory for runtime artifacts (socket). Prefers `$XDG_RUNTIME_DIR`.
pub fn runtime_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(x).join(c::RUNTIME_SUBDIR);
    }
    let tmp = std::env::temp_dir();
    let uid = current_uid();
    tmp.join(format!("{}-{}", c::APP_NAME, uid))
}

/// Best-effort UID from /proc; only used to name a fallback runtime dir.
fn current_uid() -> u32 {
    // Read the uid without linking libc solely for getuid(2).
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(first) = rest.split_whitespace().next() {
                    if let Ok(v) = first.parse::<u32>() {
                        return v;
                    }
                }
            }
        }
    }
    1000
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join(c::SOCKET_FILENAME)
}

/// Expand a leading `~` (alone or followed by `/`) using the user's home.
pub fn expand_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" {
        if let Some(h) = dirs::home_dir() {
            return h;
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    p.to_path_buf()
}

// ---------------------------------------------------------------------------
// Load / save
// ---------------------------------------------------------------------------

impl Config {
    /// Load config, falling back to defaults when the file is absent.
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_from_template() {
        let t = Config::template();
        let cfg: Config = toml::from_str(&t).unwrap();
        assert_eq!(cfg.storage.max_entries, c::DEFAULT_MAX_ENTRIES);
        assert_eq!(cfg.modules.channel, c::CHANNEL_STABLE);
        assert!(cfg.modules.pins.is_empty());
    }

    #[test]
    fn empty_document_yields_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.general.log_level, "info");
    }

    #[test]
    fn tilde_expansion() {
        assert_eq!(
            expand_path(Path::new("~/foo")),
            dirs::home_dir().unwrap().join("foo")
        );
        assert_eq!(expand_path(Path::new("/tmp/x")), PathBuf::from("/tmp/x"));
    }

    #[test]
    fn resolved_db_path_uses_default_when_empty() {
        let cfg: Config = toml::from_str("").unwrap();
        let p = cfg.storage.resolved_db_path();
        assert!(p.ends_with(c::DB_FILENAME));
    }

    #[test]
    fn resolved_db_path_expands_custom() {
        let cfg: Config = toml::from_str("[storage]\ndb_path = \"/tmp/test.db\"").unwrap();
        assert_eq!(
            cfg.storage.resolved_db_path(),
            PathBuf::from("/tmp/test.db")
        );
    }

    #[test]
    fn resolved_install_dir_uses_local_dir_when_set() {
        let cfg: Config = toml::from_str("[modules]\nlocal_dir = \"/tmp/dev-modules\"").unwrap();
        assert_eq!(
            cfg.modules.resolved_install_dir(),
            PathBuf::from("/tmp/dev-modules")
        );
    }

    #[test]
    fn resolved_install_dir_uses_default_when_empty() {
        let cfg: Config = toml::from_str("").unwrap();
        let d = cfg.modules.resolved_install_dir();
        assert!(d.ends_with(c::MODULES_DIRNAME));
    }

    #[test]
    fn uses_local_dir_true_when_set() {
        let cfg: Config = toml::from_str("[modules]\nlocal_dir = \"/tmp\"").unwrap();
        assert!(cfg.modules.uses_local_dir());
    }

    #[test]
    fn uses_local_dir_false_when_unset() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(!cfg.modules.uses_local_dir());
    }

    #[test]
    fn config_dir_is_nonempty() {
        let d = config_dir();
        assert!(!d.as_os_str().is_empty());
        assert!(d.to_string_lossy().contains("cliphistory"));
    }

    #[test]
    fn data_dir_is_nonempty() {
        let d = data_dir();
        assert!(!d.as_os_str().is_empty());
    }

    #[test]
    fn runtime_dir_is_nonempty() {
        let d = runtime_dir();
        assert!(!d.as_os_str().is_empty());
    }

    #[test]
    fn socket_path_ends_with_filename() {
        let p = socket_path();
        assert!(p.ends_with(c::SOCKET_FILENAME));
    }

    #[test]
    fn keys_defaults_are_valid() {
        let cfg: Config = toml::from_str("").unwrap();
        // Must parse without panicking.
        let _ = &cfg.frontend.keys;
    }
}
