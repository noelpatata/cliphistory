//! TOML configuration: everything that may alter behaviour at runtime.

use crate::constants as c;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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
    /// Run this shell command after a selection is written to the clipboard,
    /// enabling automatic paste. Example:
    ///   "wtype -M ctrl -k v -m ctrl"   (pacman -S wtype)
    /// Empty/unset disables auto-paste.
    pub paste_command: Option<String>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
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
    /// Longest edge of cached image previews, in pixels. 0 disables
    /// thumbnail generation entirely.
    pub thumbnail_size: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: PathBuf::new(),
            max_entries: c::DEFAULT_MAX_ENTRIES,
            max_item_size: c::DEFAULT_MAX_ITEM_SIZE_BYTES,
            max_age_days: c::DEFAULT_MAX_AGE_DAYS,
            thumbnail_size: c::DEFAULT_THUMBNAIL_SIZE,
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
    /// Force a specific frontend id, e.g. `frontend-rofi`. Empty = auto.
    pub preferred_frontend: Option<String>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FrontendConfig {
    /// Extra CLI arguments appended to every frontend invocation.
    pub extra_args: Vec<String>,
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

    /// Write a fully documented template; refuses to overwrite existing files.
    pub fn write_template(path: &Path) -> Result<()> {
        if path.exists() {
            anyhow::bail!("refusing to overwrite {}", path.display());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(path, Self::template()).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    fn template() -> String {
        format!(
            r#"# cliphistory configuration
# Documentation inline; delete keys to fall back to defaults.

[general]
# trace | debug | info | warn | error  (env RUST_LOG overrides)
log_level = "info"
# paste_command = "wtype -M ctrl -k v -m ctrl"   # auto-paste after selecting (pacman -S wtype)

[storage]
# db_path = "~/.local/share/{app}/{db}"
max_entries = {max_entries}
max_item_size = {max_item}      # bytes; larger payloads are ignored
max_age_days = 0                # 0 = keep forever
thumbnail_size = 256            # px, longest edge of image previews; 0 disables

[discovery]
# preferred_clipboard = "clipboard-wayland"     # omit for automatic detection
# preferred_frontend = "frontend-rofi"
strict = false                  # error instead of fallback when tools missing

[modules]
# source_url = "{source}"       # any GitHub repo; file:///path works too
channel = "{channel}"           # stable = latest published release
auto_update = false
# install_dir = "~/.local/share/{app}/modules"
# local_dir = "../target/debug" # dev mode: use binaries from this dir
# platform_override = "x86_64-unknown-linux-musl"

[modules.pins]
# clipboard-wayland = "v0.1.0"

[frontend]
extra_args = []                 # e.g. ["-theme", "~/.config/rofi/cliphistory.rasi"]
"#,
            app = c::APP_NAME,
            db = c::DB_FILENAME,
            source = c::DEFAULT_SOURCE_URL,
            channel = c::CHANNEL_STABLE,
            max_entries = c::DEFAULT_MAX_ENTRIES,
            max_item = c::DEFAULT_MAX_ITEM_SIZE_BYTES,
        )
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
}
