//! Locating installed modules on disk.

use super::model::InstalledModule;
use super::{process, ModuleManager};
use anyhow::Result;
use std::fs;
use std::path::Path;

impl ModuleManager {
    /// Locate an installed module binary by id, honouring `local_dir`.
    pub fn resolve(&self, id: &str) -> Option<InstalledModule> {
        let bin_name = super::bin_name(id);
        if let Some(local) = &self.cfg.local_dir {
            let p = crate::config::expand_path(local).join(&bin_name);
            return self.describe_path(id, &p);
        }
        let current = self.install_root().join(id).join(c::CURRENT_LINK_NAME);
        let bin = current.join(&bin_name);
        self.describe_path(id, &bin)
    }

    pub fn list_installed(&self) -> Result<Vec<InstalledModule>> {
        let mut out = Vec::new();
        if let Some(local) = &self.cfg.local_dir {
            let dir = crate::config::expand_path(local);
            // A stale dev path must degrade, not crash-loop the daemon.
            if !dir.is_dir() {
                log::warn!(
                    "modules.local_dir '{}' does not exist or is not a \
                     directory; no local modules loaded",
                    dir.display()
                );
                return Ok(out);
            }
            let mut found: Vec<InstalledModule> = fs::read_dir(&dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().starts_with(super::bin_prefix().as_str()))
                        .unwrap_or(false)
                })
                .filter_map(|p| {
                    let id = p
                        .file_name()?
                        .to_string_lossy()
                        .trim_start_matches(super::bin_prefix().as_str())
                        .to_string();
                    self.describe_path(&id, &p)
                })
                .collect();
            found.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
            out.extend(found);
            return Ok(out);
        }

        let root = self.install_root();
        if !root.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&root)?.filter_map(|e| e.ok()) {
            let Some(id) = entry.file_name().into_string().ok() else {
                continue;
            };
            if let Some(m) = self.resolve(&id) {
                out.push(m);
            }
        }
        out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        Ok(out)
    }

    fn describe_path(&self, id: &str, bin: &Path) -> Option<InstalledModule> {
        if !bin.exists() {
            return None;
        }
        let manifest = process::query_manifest(bin).ok()?;
        if manifest.id != id || manifest.protocol_version != c::PROTOCOL_VERSION {
            log::warn!(
                "module at {} reports id={} protocol={} (expected {id}/{})",
                bin.display(),
                manifest.id,
                manifest.protocol_version,
                c::PROTOCOL_VERSION
            );
            if manifest.id != id {
                return None;
            }
        }
        // Version recorded next to the binary at install time; local builds
        // have none.
        let version = bin
            .parent()
            .and_then(|dir| fs::read_to_string(dir.join("VERSION")).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        Some(InstalledModule {
            manifest,
            bin_path: bin.to_path_buf(),
            version,
        })
    }
}

use crate::constants as c;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModulesConfig;

    #[test]
    fn missing_local_dir_degrades_to_empty() {
        // Regression: a stale modules.local_dir used to bubble ENOENT all
        // the way up and crash-loop the daemon via systemd restarts.
        let mm = ModuleManager::new(ModulesConfig {
            local_dir: Some("/nonexistent/cliphistory-dev-dir".into()),
            ..Default::default()
        });
        let installed = mm.list_installed().expect("must not error");
        assert!(installed.is_empty());
    }
}
