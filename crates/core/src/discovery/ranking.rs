//! Tool probing and candidate ranking: which installed module best fits
//! this machine.

use cliphistory_proto::{ModuleKind, ModuleManifest};

/// True when an executable `name` exists on PATH.
pub fn probe_tool(name: &str) -> bool {
    let Ok(path_var) = std::env::var("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        if is_executable(&dir.join(name)) {
            return true;
        }
    }
    false
}

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file()
        && std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

/// What the daemon knows about one installed module.
#[derive(Clone, Debug)]
pub struct InstalledInfo {
    pub manifest: ModuleManifest,
    /// True when every entry of `manifest.requires` was probed successfully.
    pub requirements_met: bool,
}

/// Order candidate module ids for one [`ModuleKind`].
///
/// 1. Configured preference (if installed)
/// 2. Requirements satisfied beats unsatisfied
/// 3. Position in the static candidate list (session-derived for readers)
///
/// Ids absent from both `candidates` and `installed` are appended last so
/// nothing silently disappears; the caller decides whether to act on them.
pub fn rank_candidates(
    candidates: &[&str],
    installed: &[InstalledInfo],
    preferred: Option<&str>,
    kind: ModuleKind,
) -> Vec<String> {
    let by_id = |id: &str| installed.iter().find(|m| m.manifest.id == id);
    let mut known: Vec<String> = candidates
        .iter()
        .filter(|c| by_id(c).is_some())
        .filter(|cid| {
            installed
                .iter()
                .any(|m| m.manifest.kind == kind && &m.manifest.id == *cid)
        })
        .map(|s| s.to_string())
        .collect();

    // Anything installed but not in the static list still deserves a slot.
    for m in installed.iter().filter(|m| m.manifest.kind == kind) {
        if !known.contains(&m.manifest.id) {
            known.push(m.manifest.id.clone());
        }
    }

    let score = |id: &str| -> (u8, u8) {
        let pref_pen = if preferred == Some(id) { 0 } else { 1 };
        let req_pen = match by_id(id) {
            Some(m) if m.requirements_met => 0,
            _ => 1,
        };
        (pref_pen, req_pen)
    };
    // Stable sort keeps the declared priority order inside equal scores.
    known.sort_by_key(|id| score(id));
    known
}

/// Probe a module's declared tool requirements against PATH.
pub fn probe_requirements(requires: &[String]) -> (bool, Vec<(String, bool)>) {
    let mut all_ok = true;
    let mut statuses = Vec::new();
    for r in requires {
        let ok = probe_tool(r);
        if !ok {
            all_ok = false;
        }
        statuses.push((r.clone(), ok));
    }
    (all_ok, statuses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants as c;
    use cliphistory_proto::PROTOCOL_VERSION;

    fn manifest(id: &str, kind: ModuleKind, requires: &[&str]) -> ModuleManifest {
        ModuleManifest {
            id: id.into(),
            kind,
            version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec![],
            requires: requires.iter().map(|s| s.to_string()).collect(),
            features: vec![],
            description: String::new(),
        }
    }

    #[test]
    fn ranking_prefers_config_then_requirements_then_priority() {
        let wl_reader = InstalledInfo {
            manifest: manifest("clipboard-wayland", ModuleKind::Clipboard, &[]),
            requirements_met: true,
        };
        let x11_missing = InstalledInfo {
            manifest: manifest("clipboard-x11", ModuleKind::Clipboard, &["xclip"]),
            requirements_met: false,
        };
        let installed = vec![x11_missing, wl_reader];

        let ranked = rank_candidates(
            c::CLIPBOARD_CANDIDATES_X11
                .iter()
                .chain(c::CLIPBOARD_CANDIDATES_WAYLAND)
                .copied()
                .collect::<Vec<_>>()
                .as_slice(),
            &installed,
            None,
            ModuleKind::Clipboard,
        );
        assert_eq!(ranked.first().unwrap(), "clipboard-wayland");

        let ranked = rank_candidates(
            c::CLIPBOARD_CANDIDATES_X11
                .iter()
                .chain(c::CLIPBOARD_CANDIDATES_WAYLAND)
                .copied()
                .collect::<Vec<_>>()
                .as_slice(),
            &installed,
            Some("clipboard-x11"),
            ModuleKind::Clipboard,
        );
        assert_eq!(ranked.first().unwrap(), "clipboard-x11");
    }
}
