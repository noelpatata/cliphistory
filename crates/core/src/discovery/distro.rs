//! /etc/os-release parsing and per-distro install hints.

use crate::constants as c;
use std::fmt;
use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn os_release_parsing_and_hints() {
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
