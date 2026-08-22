//! System discovery: which graphics session is running, which external tools
//! exist, which distro we're on — and how to rank candidate modules.
//!
//! The pure logic (`rank_candidates`, session mapping) is separated from the
//! environment-touching parts so it can be unit-tested deterministically.

use crate::config::Config;
use crate::constants as c;
use anyhow::Result;
use cliphistory_proto::{ModuleKind, ModuleManifest};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

// ---------------------------------------------------------------------------
// Environment abstraction (testable)
// ---------------------------------------------------------------------------

pub trait EnvSource {
    fn get(&self, name: &str) -> Option<String>;
}

pub struct RealEnv;

impl EnvSource for RealEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
}

/// In-memory env for tests.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Default)]
pub struct MapEnv(pub BTreeMap<String, String>);

impl EnvSource for MapEnv {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

// ---------------------------------------------------------------------------
// Session & distro
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionType {
    Wayland,
    X11,
    Tty,
}

impl fmt::Display for SessionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SessionType::Wayland => "wayland",
            SessionType::X11 => "x11",
            SessionType::Tty => "tty",
        })
    }
}

impl SessionType {
    /// Reader module ids to try for this kind of session, best first.
    pub fn clipboard_candidates(self) -> &'static [&'static str] {
        match self {
            SessionType::Wayland => c::CLIPBOARD_CANDIDATES_WAYLAND,
            SessionType::X11 => c::CLIPBOARD_CANDIDATES_X11,
            // A TTY can host either once a compositor appears; let tool
            // probing decide instead of hard-coding a preference.
            SessionType::Tty => &[],
        }
    }
}

/// `$XDG_SESSION_TYPE`, falling back to socket/display variable probes.
pub fn detect_session(env: &dyn EnvSource) -> SessionType {
    if let Some(t) = env.get(c::ENV_SESSION_TYPE) {
        match t.to_ascii_lowercase().as_str() {
            "wayland" => return SessionType::Wayland,
            "x11" => return SessionType::X11,
            "tty" => return SessionType::Tty,
            _ => {}
        }
    }
    if env.get(c::ENV_WAYLAND_DISPLAY).is_some() {
        return SessionType::Wayland;
    }
    if env.get(c::ENV_DISPLAY).is_some() {
        return SessionType::X11;
    }
    SessionType::Tty
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DistroInfo {
    /// `ID` from os-release, e.g. `arch`.
    pub id: String,
    /// `PRETTY_NAME`, e.g. `Arch Linux`.
    pub pretty_name: String,
    /// Package-manager install hint derived from `ID`/`ID_LIKE`.
    pub install_hint: String,
}

impl fmt::Display for DistroInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.pretty_name, self.id)
    }
}

fn install_hint_for(id: &str, id_like: &str) -> String {
    let hay = format!("{id} {id_like}");
    const TABLE: &[(&str, &str)] = &[
        ("arch", "sudo pacman -S {pkgs}"),
        ("cachyos", "sudo pacman -S {pkgs}"),
        ("manjaro", "sudo pacman -S {pkgs}"),
        ("debian", "sudo apt install {pkgs}"),
        ("ubuntu", "sudo apt install {pkgs}"),
        ("fedora", "sudo dnf install {pkgs}"),
        ("suse", "sudo zypper install {pkgs}"),
        ("alpine", "doas apk add {pkgs}"),
        ("nixos", "add to environment.systemPackages: {pkgs}"),
        ("void", "sudo xbps-install -S {pkgs}"),
        ("gentoo", "emerge --ask {pkgs}"),
    ];
    for (needle, hint) in TABLE {
        if hay.contains(needle) {
            return (*hint).to_string();
        }
    }
    "install {pkgs} with your distribution's package manager".into()
}

pub fn detect_distro() -> Option<DistroInfo> {
    parse_os_release(Path::new(c::OS_RELEASE_PATH))
}

pub fn parse_os_release(path: &Path) -> Option<DistroInfo> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut id = String::new();
    let mut pretty = String::new();
    let mut id_like = String::new();
    for line in raw.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim_matches('"');
        match k {
            "ID" => id = v.to_string(),
            "PRETTY_NAME" => pretty = v.to_string(),
            "ID_LIKE" => id_like = v.to_string(),
            _ => {}
        }
    }
    if id.is_empty() {
        return None;
    }
    if pretty.is_empty() {
        pretty = id.clone();
    }
    let hint = install_hint_for(&id, &id_like);
    Some(DistroInfo {
        id,
        pretty_name: pretty,
        install_hint: hint,
    })
}

// ---------------------------------------------------------------------------
// Tool probing
// ---------------------------------------------------------------------------

/// True when an executable `name` exists on PATH.
pub fn probe_tool(name: &str) -> bool {
    let Ok(path_var) = std::env::var("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return true;
        }
    }
    false
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file()
        && std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Ranking
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DiscoveryReport {
    pub session: SessionType,
    pub distro: Option<DistroInfo>,
    pub tools: Vec<(String, bool)>,
    pub readers: Vec<String>,
    pub frontends: Vec<String>,
}

impl fmt::Display for DiscoveryReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "session:   {}", self.session)?;
        if let Some(d) = &self.distro {
            writeln!(f, "distro:    {d}")?;
        } else {
            writeln!(f, "distro:    <unknown>")?;
        }
        writeln!(f, "tools:")?;
        for (name, found) in &self.tools {
            writeln!(
                f,
                "  {:<12} {}",
                name,
                if *found { "found" } else { "missing" }
            )?;
        }
        writeln!(f, "readers:    {}", self.readers.join(" > "))?;
        write!(f, "frontends:  {}", self.frontends.join(" > "))
    }
}

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

/// Run full discovery against the real system.
pub fn discover(cfg: &Config, installed: &[InstalledInfo]) -> Result<DiscoveryReport> {
    let session = detect_session(&RealEnv);
    let distro = detect_distro();

    let relevant = installed.to_vec();
    let mut tools: Vec<(String, bool)> = Vec::new();
    let mut push_tools = |reqs: &[(String, bool)]| {
        for (name, ok) in reqs {
            if !tools.iter().any(|(n, _)| n == name) {
                tools.push((name.clone(), *ok));
            }
        }
    };

    let reader_cands = session.clipboard_candidates().to_vec();
    let readers = rank_candidates(
        &reader_cands,
        &relevant,
        cfg.discovery.preferred_clipboard.as_deref(),
        ModuleKind::Clipboard,
    );
    let frontends = rank_candidates(
        c::FRONTEND_CANDIDATES,
        &relevant,
        cfg.discovery.preferred_frontend.as_deref(),
        ModuleKind::Frontend,
    );

    for m in &relevant {
        let (ok, statuses) = probe_requirements(&m.manifest.requires);
        let _ = ok;
        push_tools(&statuses);
    }

    Ok(DiscoveryReport {
        session,
        distro,
        tools,
        readers,
        frontends,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cliphistory_proto::{ModuleKind, PROTOCOL_VERSION};

    fn manifest(id: &str, kind: ModuleKind, requires: &[&str]) -> ModuleManifest {
        ModuleManifest {
            id: id.into(),
            kind,
            version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec!["read".into(), "write".into()],
            requires: requires.iter().map(|s| s.to_string()).collect(),
            description: String::new(),
        }
    }

    #[test]
    fn session_from_env_vars() {
        let mk = |pairs: &[(&str, &str)]| {
            MapEnv(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        };
        assert_eq!(
            detect_session(&mk(&[(c::ENV_SESSION_TYPE, "wayland")])),
            SessionType::Wayland
        );
        assert_eq!(
            detect_session(&mk(&[
                (c::ENV_SESSION_TYPE, "unspecified"),
                (c::ENV_DISPLAY, ":0"),
            ])),
            SessionType::X11
        );
        assert_eq!(
            detect_session(&mk(&[
                (c::ENV_WAYLAND_DISPLAY, "wayland-1"),
                (c::ENV_DISPLAY, ":0"),
            ])),
            SessionType::Wayland
        );
        assert_eq!(detect_session(&MapEnv::default()), SessionType::Tty);
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

        // No preference: requirement-satisfied wayland wins over broken x11.
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

        // Explicit preference wins even when another candidate is healthy.
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

    #[test]
    fn frontend_priority_order() {
        let rofi = InstalledInfo {
            manifest: manifest("frontend-rofi", ModuleKind::Frontend, &["rofi"]),
            requirements_met: true,
        };
        let wofi = InstalledInfo {
            manifest: manifest("frontend-wofi", ModuleKind::Frontend, &["wofi"]),
            requirements_met: true,
        };
        let ranked = rank_candidates(
            c::FRONTEND_CANDIDATES,
            &[wofi, rofi],
            None,
            ModuleKind::Frontend,
        );
        assert_eq!(ranked[0], "frontend-rofi");
        assert_eq!(ranked[1], "frontend-wofi");
    }

    #[test]
    fn os_release_parsing_and_hints() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            f,
            "NAME=\"Arch Linux\"\nID=arch\nID_LIKE=arch\nPRETTY_NAME=\"Arch Linux\""
        )
        .unwrap();
        let d = parse_os_release(f.path()).unwrap();
        assert_eq!(d.id, "arch");
        assert!(d.install_hint.contains("pacman"));
    }
}
