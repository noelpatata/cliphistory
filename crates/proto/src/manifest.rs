//! Module self-description: kinds, capabilities and the manifest document
//! printed by every module binary via `--manifest`.

use serde::{Deserialize, Serialize};

pub const KIND_CLIPBOARD: &str = "clipboard";
pub const KIND_FRONTEND: &str = "frontend";
pub const KIND_PASTER: &str = "paster";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleKind {
    Clipboard,
    Frontend,
    Paster,
}

impl ModuleKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ModuleKind::Clipboard => KIND_CLIPBOARD,
            ModuleKind::Frontend => KIND_FRONTEND,
            ModuleKind::Paster => KIND_PASTER,
        }
    }
}

/// Self-description printed by every module binary via `--manifest`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleManifest {
    /// Stable identifier, e.g. `clipboard-wayland`, `frontend-generic`.
    pub id: String,
    pub kind: ModuleKind,
    pub version: String,
    pub protocol_version: u32,
    /// Subset of `read` / `write`.
    pub capabilities: Vec<String>,
    /// External executables this module needs on PATH at runtime.
    pub requires: Vec<String>,
    /// Optional feature switches, e.g. `["images"]` for frontends that can
    /// render thumbnails.
    #[serde(default)]
    pub features: Vec<String>,
    pub description: String,
}

impl ModuleManifest {
    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }

    pub fn missing_tools<F>(&self, probe: F) -> Vec<String>
    where
        F: Fn(&str) -> bool,
    {
        self.requires
            .iter()
            .filter(|t| !probe(t))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CAP_READ, CAP_WRITE, PROTOCOL_VERSION};

    #[test]
    fn manifest_missing_tools() {
        let m = ModuleManifest {
            id: "clipboard-x11".into(),
            kind: ModuleKind::Clipboard,
            version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec![CAP_READ.into(), CAP_WRITE.into()],
            requires: vec!["xclip".into()],
            features: vec![],
            description: String::new(),
        };
        assert_eq!(m.missing_tools(|_| true), Vec::<String>::new());
        assert_eq!(m.missing_tools(|_| false), vec!["xclip".to_string()]);
    }
}
