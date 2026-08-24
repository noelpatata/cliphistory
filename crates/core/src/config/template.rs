//! Default configuration file generation.

use super::Config;
use crate::constants as c;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

impl Config {
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

    pub(crate) fn template() -> String {
        format!(
            r#"# cliphistory configuration
# Documentation inline; delete keys to fall back to defaults.

[general]
# trace | debug | info | warn | error  (env RUST_LOG overrides)
log_level = "info"
auto_paste = true               # replay the paste chord into the focused window after selecting
paste_delay_ms = 150
# paste_command = ""            # expert override: shell command instead of paster module

[storage]
# db_path = "~/.local/share/{app}/{db}"
max_entries = {max_entries}
max_item_size = {max_item}      # bytes; larger payloads are ignored
max_age_days = 0                # 0 = keep forever
max_total_bytes = {max_total}   # hard payload budget; 0 = unlimited
thumbnail_size = 256            # px, longest edge of image previews; 0 disables

[discovery]
# preferred_clipboard = "clipboard-wayland"     # omit for automatic detection
# preferred_frontend = "frontend-generic"
# preferred_paster = "paster-wayland"
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
            max_total = c::DEFAULT_MAX_TOTAL_BYTES,
        )
    }
}
