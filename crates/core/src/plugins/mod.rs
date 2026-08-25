//! Module manager: discovery of installed modules, downloading from GitHub
//! releases (or local/file sources), checksum verification, atomic
//! installation and child-process plumbing.
//!
//! Layout under `install_dir`:
//!
//! ```text
//! modules/<id>/<version>/cliphistory-<id>      # binary
//! modules/<id>/<version>/manifest.json      # self-description snapshot
//! modules/<id>/current                      # symlink -> active version
//! ```
//!
//! Responsibilities are split across sibling modules, each with one job:
//!
//! * [`model`]    — wire/filesystem data types
//! * [`registry`] — locating installed modules
//! * [`release`]  — manifest URLs, downloads, checksums, target selection
//! * [`install`]  — install/update/uninstall flows
//! * [`process`]  — child-process handles and frontend runs

pub(crate) mod install;
pub(crate) mod model;
pub(crate) mod process;
pub(crate) mod registry;
pub(crate) mod release;

use crate::config::ModulesConfig;

pub use model::{
    AssetRef, InstallReason, InstalledModule, ReleaseManifest, RemoteModule, TargetAssets,
};
pub use process::{query_manifest, run_frontend, ClipboardHandle, ModuleHandle, PasterHandle};
pub use release::verify_sha256;

/// Binary name for an installed module id (`cliphistory-<id>`).
pub(crate) fn bin_name(id: &str) -> String {
    format!("{}-{}", c::APP_NAME, id)
}

/// Filename prefix shared by every module binary.
pub(crate) fn bin_prefix() -> String {
    format!("{}-", c::APP_NAME)
}

use crate::constants as c;

/// Facade over the plugin subsystem: owns the module configuration and
/// delegates to the sibling modules.
pub struct ModuleManager {
    pub(crate) cfg: ModulesConfig,
}

impl ModuleManager {
    pub fn new(cfg: ModulesConfig) -> Self {
        Self { cfg }
    }

    pub fn install_root(&self) -> std::path::PathBuf {
        self.cfg.resolved_install_dir()
    }

    pub fn cfg_uses_local_dir(&self) -> bool {
        self.cfg.uses_local_dir()
    }
}
