//! Install / uninstall / update flows: download, verify, atomically activate.

use super::model::{InstallReason, InstalledModule, ReleaseManifest};
use super::{release, ModuleManager};
use crate::constants as c;
use anyhow::{anyhow, Result};

impl ModuleManager {
    /// Download + install one module from a fetched release manifest.
    pub fn install_from(
        &self,
        rm: &ReleaseManifest,
        id: &str,
        reason: InstallReason,
        progress: &dyn Fn(&str),
    ) -> Result<InstalledModule> {
        let assets = self.select_target(rm)?;
        let remote = assets
            .modules
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| anyhow!("release {} has no module '{id}'", rm.release))?;

        progress(&format!("downloading {} ({reason})…", remote.id));
        let bytes = release::http_get(&self.asset_url(&rm.release, &remote.file.path))?;
        c_verify(&bytes, &remote.file.sha256)?;

        let root = self.install_root();
        let id_dir = root.join(id);
        fs::create_dir_all(&id_dir)?;
        let version_dir = id_dir.join(&rm.release);
        let staging = tempfile::tempdir_in(&id_dir).context("creating staging dir")?;

        let bin_name = super::bin_name(id);
        let staged_bin = staging.path().join(&bin_name);
        fs::write(&staged_bin, &bytes)?;
        fs::set_permissions(
            &staged_bin,
            fs::Permissions::from_mode(c::MODULE_BINARY_PERMS),
        )?;

        use cliphistory_proto::{ModuleManifest, PROTOCOL_VERSION};
        let manifest = ModuleManifest {
            id: remote.id.clone(),
            kind: remote.kind,
            version: rm.release.clone(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: remote.capabilities.clone(),
            requires: remote.requires.clone(),
            features: remote.features.clone(),
            description: remote.description.clone(),
        };
        fs::write(
            staging.path().join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        fs::write(staging.path().join("VERSION"), format!("{}\n", rm.release))?;

        if version_dir.exists() {
            fs::remove_dir_all(&version_dir)?;
        }
        fs::rename(staging.path(), &version_dir)
            .with_context(|| format!("activating {}", version_dir.display()))?;
        swap_symlink(&root.join(id), c::CURRENT_LINK_NAME, &version_dir)?;

        progress(&format!("installed {} @ {}", id, version_dir.display()));
        Ok(InstalledModule {
            manifest,
            bin_path: version_dir.join(bin_name),
            version: rm.release.clone(),
        })
    }

    /// Make sure every listed module is present (and optionally current).
    /// Returns per-id result messages; failures do not abort the batch.
    pub fn ensure_available(
        &self,
        ids: &[String],
        force: bool,
        progress: &dyn Fn(&str),
    ) -> Vec<Result<String>> {
        let mut results = Vec::new();
        if self.cfg.uses_local_dir() {
            for id in ids {
                results.push(match self.resolve(id) {
                    Some(_) => Ok(format!("{id}: using local build")),
                    None => Err(anyhow!(
                        "{id}: not found in local dir {}; build it first",
                        self.install_root().display()
                    )),
                });
            }
            return results;
        }

        let rm = match self.fetch_remote_manifest(None) {
            Ok(rm) => rm,
            Err(e) => {
                return ids
                    .iter()
                    .map(|_| Err(anyhow!("cannot reach releases: {e}")))
                    .collect()
            }
        };

        for id in ids {
            let outcome: Result<String> = (|| {
                let installed = self.resolve(id);
                let pin_tag = self.cfg.pins.get(id);

                let (target_release, needs_install) = if force {
                    (pin_tag.cloned().unwrap_or_else(|| rm.release.clone()), true)
                } else {
                    match &installed {
                        Some(inst) if !inst.version.is_empty() => {
                            let want_same = match pin_tag {
                                Some(t) => inst.version == *t,
                                None => inst.version == rm.release,
                            };
                            if want_same {
                                (inst.version.clone(), false)
                            } else if self.cfg.auto_update || pin_tag.is_some() {
                                (pin_tag.cloned().unwrap_or_else(|| rm.release.clone()), true)
                            } else {
                                (inst.version.clone(), false)
                            }
                        }
                        _ => (pin_tag.cloned().unwrap_or_else(|| rm.release.clone()), true),
                    }
                };

                if !needs_install {
                    return Ok(format!(
                        "{id}: already installed ({})",
                        installed.map(|i| i.version).unwrap_or_default()
                    ));
                }

                let reason = if force {
                    InstallReason::Forced
                } else {
                    match &installed {
                        Some(i) => InstallReason::Update {
                            from: i.version.clone(),
                            to: target_release.clone(),
                        },
                        None => InstallReason::Missing,
                    }
                };

                let effective_rm = if target_release == rm.release {
                    rm.clone()
                } else {
                    self.fetch_remote_manifest(Some(&target_release))?
                };
                self.install_from(&effective_rm, id, reason, progress)?;
                Ok(format!("{id}: installed {target_release}"))
            })();
            results.push(outcome);
        }
        results
    }

    /// Remove an installed module.
    ///
    /// Honours the active layout: dev checkouts (`modules.local_dir`) keep
    /// bare binaries in one flat directory; release installs keep a
    /// per-module versioned directory under the install root.
    pub fn uninstall(&self, id: &str) -> Result<()> {
        if let Some(local) = &self.cfg.local_dir {
            let bin = crate::config::expand_path(local).join(super::bin_name(id));
            if !bin.exists() {
                anyhow::bail!("{id} is not installed");
            }
            fs::remove_file(&bin).with_context(|| format!("removing {}", bin.display()))?;
            return Ok(());
        }
        let dir = self.install_root().join(id);
        if !dir.exists() {
            anyhow::bail!("{id} is not installed");
        }
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}

use anyhow::Context;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn c_verify(bytes: &[u8], expected: &str) -> Result<()> {
    release::verify_sha256(bytes, expected)
}

fn swap_symlink(dir: &Path, link_name: &str, target: &Path) -> Result<()> {
    let final_link = dir.join(link_name);
    let tmp = dir.join(format!("{link_name}.tmp.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target.canonicalize()?, &tmp)
        .with_context(|| format!("linking {}", tmp.display()))?;
    fs::rename(&tmp, &final_link)
        .with_context(|| format!("activating {}", final_link.display()))
}

#[cfg(test)]
mod tests {
    use super::super::{bin_name, ModuleManager};
    use crate::config::ModulesConfig;
    use tempfile::TempDir;

    fn manager_with_local_dir(dir: &std::path::Path) -> ModuleManager {
        ModuleManager::new(ModulesConfig {
            local_dir: Some(dir.to_path_buf()),
            ..Default::default()
        })
    }

    #[test]
    fn uninstall_removes_local_dir_binary() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join(bin_name("testmod"));
        std::fs::write(&bin, b"fake binary").unwrap();

        let mm = manager_with_local_dir(tmp.path());
        mm.uninstall("testmod").expect("uninstall should succeed");

        assert!(!bin.exists(), "binary must be gone");
    }

    #[test]
    fn uninstall_missing_local_module_is_a_clear_error() {
        let tmp = TempDir::new().unwrap();
        let mm = manager_with_local_dir(tmp.path());
        let err = mm.uninstall("nope").expect_err("must fail");
        assert!(err.to_string().contains("not installed"), "{err}");
    }

    #[test]
    fn uninstall_release_layout_missing_reports_not_installed() {
        // Default config, nothing installed: must fail with the accurate
        // message rather than touch anything.
        let mm = ModuleManager::new(ModulesConfig::default());
        let err = mm
            .uninstall("definitely-not-installed")
            .expect_err("must fail");
        assert!(err.to_string().contains("not installed"), "{err}");
    }
}
