//! Data types crossing the plugin boundary: release manifests, installed
//! module records and install reasons. No behavior lives here.

use serde::{Deserialize, Serialize};

/// A single downloadable artifact: path relative to the release base plus
/// its sha256.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetRef {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteModule {
    pub id: String,
    pub kind: cliphistory_proto::ModuleKind,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub description: String,
    pub file: AssetRef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetAssets {
    pub core: AssetRef,
    pub modules: Vec<RemoteModule>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseManifest {
    /// Tag this release was published under, e.g. `v0.1.2`.
    pub release: String,
    pub protocol_version: u32,
    pub targets: std::collections::BTreeMap<String, TargetAssets>,
}

/// An installed module: manifest snapshot plus executable location.
#[derive(Clone, Debug)]
pub struct InstalledModule {
    pub manifest: ModuleManifest,
    /// Executable path (canonicalised through the `current` symlink).
    pub bin_path: PathBuf,
    pub version: String,
}

use cliphistory_proto::ModuleManifest;
use std::path::PathBuf;

/// Why an install was performed; drives user-facing messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallReason {
    Missing,
    Forced,
    Update { from: String, to: String },
}

impl std::fmt::Display for InstallReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallReason::Missing => write!(f, "not installed"),
            InstallReason::Forced => write!(f, "forced"),
            InstallReason::Update { from, to } => write!(f, "update {from} -> {to}"),
        }
    }
}
