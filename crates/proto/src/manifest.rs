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

    #[test]
    fn module_kind_as_str() {
        assert_eq!(ModuleKind::Clipboard.as_str(), "clipboard");
        assert_eq!(ModuleKind::Frontend.as_str(), "frontend");
        assert_eq!(ModuleKind::Paster.as_str(), "paster");
    }

    #[test]
    fn has_capability_true() {
        let m = ModuleManifest {
            id: "test".into(),
            kind: ModuleKind::Clipboard,
            version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec!["read".into(), "write".into()],
            requires: vec![],
            features: vec![],
            description: String::new(),
        };
        assert!(m.has_capability("read"));
        assert!(m.has_capability("write"));
    }

    #[test]
    fn has_capability_false() {
        let m = ModuleManifest {
            id: "test".into(),
            kind: ModuleKind::Clipboard,
            version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec!["read".into()],
            requires: vec![],
            features: vec![],
            description: String::new(),
        };
        assert!(!m.has_capability("write"));
        assert!(!m.has_capability(""));
    }

    #[test]
    fn has_capability_empty_list() {
        let m = ModuleManifest {
            id: "test".into(),
            kind: ModuleKind::Clipboard,
            version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec![],
            requires: vec![],
            features: vec![],
            description: String::new(),
        };
        assert!(!m.has_capability("read"));
    }

    #[test]
    fn manifest_roundtrip() {
        let m = ModuleManifest {
            id: "test-module".into(),
            kind: ModuleKind::Frontend,
            version: "1.2.3".into(),
            protocol_version: 99,
            capabilities: vec!["render".into()],
            requires: vec!["fontconfig".into()],
            features: vec!["images".into()],
            description: "A test module".into(),
        };
        let json = serde_json::to_string(&m).unwrap();
        let m2: ModuleManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn module_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ModuleKind::Clipboard).unwrap(),
            "\"clipboard\""
        );
        assert_eq!(
            serde_json::to_string(&ModuleKind::Frontend).unwrap(),
            "\"frontend\""
        );
        assert_eq!(
            serde_json::to_string(&ModuleKind::Paster).unwrap(),
            "\"paster\""
        );
    }

    #[test]
    fn manifest_defaults() {
        let m = ModuleManifest {
            id: "x".into(),
            kind: ModuleKind::Paster,
            version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec![],
            requires: vec![],
            features: vec![],
            description: String::new(),
        };
        assert!(m.features.is_empty());
        assert!(m.capabilities.is_empty());
        assert!(m.requires.is_empty());
    }
}
