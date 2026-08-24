//! Font loading for glyph coverage and custom typefaces.
//!
//! Clipboard text frequently contains private-use-area glyphs (shell-prompt
//! icons and the like) that egui's bundled fonts cannot render. Two modes:
//!
//! * **Configured font** (`font_family` set): the value is resolved to a
//!   file — direct path first, then fontconfig by exact family name, then a
//!   bounded filename scan — and registered as the *primary* face, so the
//!   picker renders in whatever the user chose. Built-in fonts remain as
//!   automatic fallbacks for missing glyphs.
//! * **Unset**: an installed Nerd Font (symbol-only builds preferred) is
//!   auto-detected and appended as pure glyph fallback; everyday text keeps
//!   its default look.
//!
//! Finding nothing is never fatal: the UI just keeps default fonts.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Family keys used when registering loaded fonts.
const PRIMARY_KEY: &str = "cliphistory-primary";
const FALLBACK_KEY: &str = "cliphistory-fallback";
/// How deep below each font root the fallback scan descends.
const MAX_SCAN_DEPTH: usize = 4;

/// Resolve `configured` (family name or file path) and install it as the
/// primary text font. When it is `None` or cannot be resolved, fall back to
/// auto-detecting an installed Nerd Font as glyph-only support.
pub fn install(ctx: &egui::Context, configured: Option<&str>) {
    let mut fonts = egui::FontDefinitions::default();
    let mut have_primary = false;

    if let Some(value) = configured {
        match resolve(value) {
            Some(bytes) => {
                register(&mut fonts, PRIMARY_KEY, bytes, true);
                have_primary = true;
            }
            None => eprintln!(
                "cliphistory: frontend.font_family '{value}' matches no installed \
                 font or file; using defaults"
            ),
        }
    }
    if !have_primary {
        // Unset or unresolvable choice: keep default typefaces and merely
        // cover private-use-area glyphs with an auto-detected Nerd Font.
        if let Some((bytes, _)) = detect_nerd_font() {
            register(&mut fonts, FALLBACK_KEY, bytes, false);
        }
    }
    ctx.set_fonts(fonts);
}

/// Register font bytes under `key` in both families.
///
/// * prepended → becomes the primary rendering face;
/// * appended  → serves only glyphs missing from earlier fonts.
fn register(fonts: &mut egui::FontDefinitions, key: &str, bytes: Vec<u8>, prepend: bool) {
    fonts
        .font_data
        .insert(key.to_string(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let chain = fonts.families.entry(family).or_default();
        if prepend {
            chain.insert(0, key.to_string());
        } else {
            chain.push(key.to_string());
        }
    }
}

/// Resolve a configured value to font bytes.
///
/// Order: existing file path → fontconfig family lookup → directory scan.
fn resolve(value: &str) -> Option<Vec<u8>> {
    let direct = Path::new(value);
    if direct.is_file() {
        return std::fs::read(direct).ok();
    }
    if let Some(path) = fc_list_file(value) {
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(bytes);
        }
    }
    scan_for_name(value).and_then(|p| std::fs::read(p).ok())
}

/// Ask fontconfig which installed files carry exactly this family name.
/// Returns the most regular-looking candidate.
fn fc_list_file(family: &str) -> Option<PathBuf> {
    let out = Command::new("fc-list")
        .arg(format!(":family={family}"))
        .arg("--format=%{file}\n")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    pick_regular(stdout.lines())
}

/// Prefer the Regular style when fontconfig lists several weights.
fn pick_regular<'a>(files: impl Iterator<Item = &'a str>) -> Option<PathBuf> {
    let mut first: Option<&str> = None;
    for line in files {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let lowered = line.to_lowercase();
        if lowered.contains("regular") || lowered.contains("medium") {
            return Some(PathBuf::from(line));
        }
        if first.is_none() {
            first = Some(line);
        }
    }
    first.map(PathBuf::from)
}

/// Filename-based search for `name` across standard font roots.
/// Matching is loose: spaces, dashes and underscores are ignored.
fn scan_for_name(name: &str) -> Option<PathBuf> {
    let needle = normalize_name(name);
    let mut best: Option<(u8, PathBuf)> = None;
    for root in search_dirs() {
        visit_fonts(&root, 0, &mut |path, stem| {
            if normalize_name(stem).contains(&needle) {
                let s = symbol_bonus(stem);
                if best.as_ref().is_none_or(|(best_s, _)| s < *best_s) {
                    best = Some((s, path.to_path_buf()));
                }
            }
        });
    }
    best.map(|(_, path)| path)
}

/// Auto-detect an installed Nerd Font (symbol-only builds win).
/// Returns `(bytes, display_name)`.
fn detect_nerd_font() -> Option<(Vec<u8>, String)> {
    let mut best: Option<(u8, PathBuf)> = None;
    for root in search_dirs() {
        visit_fonts(&root, 0, &mut |path, stem| {
            let lowered = stem.to_lowercase();
            if is_nerd_font_stem(&lowered) {
                let s = symbol_bonus(stem);
                if best.as_ref().is_none_or(|(best_s, _)| s < *best_s) {
                    best = Some((s, path.to_path_buf()));
                }
            }
        });
    }
    let (_, path) = best?;
    let name = path.file_stem()?.to_string_lossy().into_owned();
    Some((std::fs::read(&path).ok()?, name))
}

/// Depth-first walk over font files; `visit` receives each candidate path
/// plus its file stem (lowercased).
fn visit_fonts(dir: &Path, depth: usize, visit: &mut dyn FnMut(&Path, &str)) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_fonts(&path, depth + 1, visit);
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
        else {
            continue;
        };
        let lowered_ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if lowered_ext == "ttf" || lowered_ext == "otf" {
            visit(&path, &stem);
        }
    }
}

/// Filenames that identify a Nerd Font build.
fn is_nerd_font_stem(stem_lowered: &str) -> bool {
    stem_lowered.contains("nerdfont") || stem_lowered.contains("nerd font")
}

/// Symbol-only builds cover every PUA codepoint with the least weight;
/// prefer them over full themed fonts.
fn symbol_bonus(stem_lowered: &str) -> u8 {
    if stem_lowered.contains("symbols") {
        0
    } else {
        1
    }
}

/// Loose comparison key: case- and separator-insensitive.
fn normalize_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .collect()
}

/// Standard per-user then system-wide font locations, best first.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let xdg = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        dirs.push(xdg.unwrap_or(home.join(".local/share")).join("fonts"));
        dirs.push(home.join(".fonts"));
    }
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs.push(PathBuf::from("/usr/local/share/fonts"));
    dirs.push(PathBuf::from("/run/current-system/sw/share/fonts")); // NixOS
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_nerd_font_stems() {
        assert!(is_nerd_font_stem("symbolsnerdfont-monoregular"));
        assert!(is_nerd_font_stem("jetbrainsmono nerd font regular"));
        assert!(!is_nerd_font_stem("ubuntu-light"));
    }

    #[test]
    fn symbol_builds_rank_first() {
        assert_eq!(
            symbol_bonus("symbolsnerdfontmonoregular"),
            symbol_bonus("nerdfontssymbolsonly-regular")
        );
        assert!(symbol_bonus("symbolsnerdfont") < symbol_bonus("hacknerdfont"));
    }

    #[test]
    fn normalisation_is_loose() {
        assert_eq!(normalize_name("JetBrainsMono NerdFont"), "jetbrainsmononerdfont");
        assert_eq!(normalize_name("jet-brains_mono"), "jetbrainsmono");
    }

    #[test]
    fn regular_style_is_preferred_from_fc_list_output() {
        let files = [
            "/usr/share/fonts/x/JetBrainsMono-Bold.ttf",
            "/usr/share/fonts/x/JetBrainsMono-Regular.ttf",
            "/usr/share/fonts/x/JetBrainsMono-Italic.ttf",
        ];
        let picked = pick_regular(files.iter().copied()).unwrap();
        assert!(picked.to_string_lossy().contains("Regular"));
    }

    #[test]
    fn fc_list_output_falls_back_to_first_line() {
        let files = ["/fonts/a.ttf", "", "/fonts/b.ttf"];
        let picked = pick_regular(files.iter().copied()).unwrap();
        assert!(picked.to_string_lossy().ends_with("/a.ttf"));
    }

    #[test]
    fn fc_list_empty_output_yields_nothing() {
        assert!(pick_regular(std::iter::empty()).is_none());
    }

    #[test]
    fn search_dirs_are_well_formed_and_ordered() {
        let dirs = search_dirs();
        assert!(!dirs.is_empty());
        if let Some(home) = dirs::home_dir() {
            let user_pos = dirs.iter().position(|d| d.starts_with(&home));
            let sys_pos = dirs.iter().position(|d| d.starts_with("/usr/share/fonts"));
            if let (Some(u), Some(s)) = (user_pos, sys_pos) {
                assert!(u < s);
            }
        }
    }
}
