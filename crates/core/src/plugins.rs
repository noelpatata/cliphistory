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

use crate::config::ModulesConfig;
use crate::constants as c;
use anyhow::{anyhow, bail, Context, Result};
use cliphistory_proto::{
    ClipboardToHost, HistoryItem, HostToClipboard, ModuleKind, ModuleManifest, ShowRequest,
    ShowResponse, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;

// ---------------------------------------------------------------------------
// Release manifest (published alongside every release)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetRef {
    /// Path relative to the release download base.
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteModule {
    pub id: String,
    pub kind: ModuleKind,
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

// ---------------------------------------------------------------------------
// Installed modules
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct InstalledModule {
    pub manifest: ModuleManifest,
    /// Executable path (canonicalised through the `current` symlink).
    pub bin_path: PathBuf,
    pub version: String,
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

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

pub struct ModuleManager {
    cfg: ModulesConfig,
}

impl ModuleManager {
    pub fn new(cfg: ModulesConfig) -> Self {
        Self { cfg }
    }

    pub fn install_root(&self) -> PathBuf {
        self.cfg.resolved_install_dir()
    }

    pub fn cfg_uses_local_dir(&self) -> bool {
        self.cfg.uses_local_dir()
    }

    // -- locating -----------------------------------------------------------

    /// Locate an installed module binary by id, honouring `local_dir`.
    pub fn resolve(&self, id: &str) -> Option<InstalledModule> {
        let bin_name = format!("{}-{}", c::APP_NAME, id);
        if let Some(local) = &self.cfg.local_dir {
            let p = crate::config::expand_path(local).join(&bin_name);
            return self.describe_path(id, &p);
        }
        let current = self.install_root().join(id).join(c::CURRENT_LINK_NAME);
        let bin = current.join(&bin_name);
        self.describe_path(id, &bin)
    }

    fn describe_path(&self, id: &str, bin: &Path) -> Option<InstalledModule> {
        if !bin.exists() {
            return None;
        }
        let manifest = query_manifest(bin).ok()?;
        if manifest.id != id || manifest.protocol_version != PROTOCOL_VERSION {
            log::warn!(
                "module at {} reports id={} protocol={} (expected {id}/{PROTOCOL_VERSION})",
                bin.display(),
                manifest.id,
                manifest.protocol_version
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

    pub fn list_installed(&self) -> Result<Vec<InstalledModule>> {
        let mut out = Vec::new();
        if let Some(local) = &self.cfg.local_dir {
            let dir = crate::config::expand_path(local);
            let mut found: Vec<InstalledModule> = std::fs::read_dir(&dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| {
                            n.to_string_lossy()
                                .starts_with(&format!("{}-", c::APP_NAME))
                        })
                        .unwrap_or(false)
                })
                .filter_map(|p| {
                    let id = p
                        .file_name()?
                        .to_string_lossy()
                        .trim_start_matches(&format!("{}-", c::APP_NAME))
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
        for entry in std::fs::read_dir(&root)?.filter_map(|e| e.ok()) {
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

    // -- remote -------------------------------------------------------------

    fn source_base(&self) -> &str {
        self.cfg.source_url.trim_end_matches('/')
    }

    fn is_local_source(&self) -> bool {
        self.cfg.source_url.starts_with("file://") || self.cfg.source_url.starts_with('/')
    }

    fn manifest_url(&self, tag: Option<&str>) -> String {
        if self.is_local_source() {
            let base = self.source_base();
            return match tag {
                Some(t) => format!("{base}/{t}/manifest.json"),
                None => format!("{base}/manifest.json"),
            };
        }
        match tag {
            Some(t) => format!("{}/releases/download/{t}/manifest.json", self.source_base()),
            None => format!(
                "{}/releases/latest/download/manifest.json",
                self.source_base()
            ),
        }
    }

    fn asset_url(&self, release: &str, rel_path: &str) -> String {
        if self.is_local_source() {
            return format!("{}/{}", self.source_base(), rel_path);
        }
        format!(
            "{}/releases/download/{release}/{rel_path}",
            self.source_base()
        )
    }

    /// Fetch and cache the release manifest. `None` tag = channel default.
    pub fn fetch_remote_manifest(&self, tag: Option<&str>) -> Result<ReleaseManifest> {
        let url = self.manifest_url(tag);
        let bytes = http_get(&url)?;
        if bytes.len() > c::MAX_RELEASE_MANIFEST_BYTES {
            bail!(
                "release manifest exceeds {} bytes",
                c::MAX_RELEASE_MANIFEST_BYTES
            );
        }
        let rm: ReleaseManifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing manifest from {url}"))?;
        if rm.protocol_version != PROTOCOL_VERSION {
            bail!(
                "remote manifest speaks protocol v{}, this build speaks v{PROTOCOL_VERSION}",
                rm.protocol_version
            );
        }
        // Cache for offline inspection / update checks.
        let cache = self.install_root().join(c::RELEASE_MANIFEST_CACHE_FILENAME);
        if let Some(parent) = cache.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cache, &bytes);
        Ok(rm)
    }

    /// Pick the best target triple available in the manifest.
    pub fn select_target<'a>(&self, rm: &'a ReleaseManifest) -> Result<&'a TargetAssets> {
        let arch = std::env::consts::ARCH;
        let mut order: Vec<String> = Vec::new();
        if let Some(ovr) = &self.cfg.platform_override {
            order.push(ovr.clone());
        }
        order.push(format!("{arch}-unknown-linux-gnu"));
        order.push(format!("{arch}-unknown-linux-musl"));
        for t in order {
            if let Some(a) = rm.targets.get(&t) {
                return Ok(a);
            }
        }
        bail!(
            "release {} ships no artifacts for this machine ({arch}); targets: {:?}",
            rm.release,
            rm.targets.keys().collect::<Vec<_>>()
        );
    }

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
        let url = self.asset_url(&rm.release, &remote.file.path);
        let bytes = http_get(&url)?;
        verify_sha256(&bytes, &remote.file.sha256).context("checksum mismatch")?;

        let root = self.install_root();
        let id_dir = root.join(id);
        std::fs::create_dir_all(&id_dir)?;
        let version_dir = id_dir.join(&rm.release);
        let staging = tempfile::tempdir_in(&id_dir).context("creating staging dir")?;

        let bin_name = format!("{}-{}", c::APP_NAME, id);
        let staged_bin = staging.path().join(&bin_name);
        std::fs::write(&staged_bin, &bytes)?;
        std::fs::set_permissions(
            &staged_bin,
            std::fs::Permissions::from_mode(c::MODULE_BINARY_PERMS),
        )?;

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
        std::fs::write(
            staging.path().join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        std::fs::write(staging.path().join("VERSION"), format!("{}\n", rm.release))?;

        if version_dir.exists() {
            std::fs::remove_dir_all(&version_dir)?;
        }
        std::fs::rename(staging.path(), &version_dir)
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

    pub fn uninstall(&self, id: &str) -> Result<()> {
        let dir = self.install_root().join(id);
        if !dir.exists() {
            bail!("{id} is not installed");
        }
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    // -- process plumbing ----------------------------------------------------
}
// ---------------------------------------------------------------------------
// Free helpers (also used by CLI / daemon)
// ---------------------------------------------------------------------------

/// Run `<binary> --manifest` and parse its self-description.
pub fn query_manifest(bin: &Path) -> Result<ModuleManifest> {
    let out = Command::new(bin)
        .arg("--manifest")
        .output()
        .with_context(|| format!("executing {}", bin.display()))?;
    if !out.status.success() {
        bail!("{} --manifest failed: {}", bin.display(), out.status);
    }
    let m: ModuleManifest = serde_json::from_slice(&out.stdout)
        .with_context(|| format!("parsing manifest of {}", bin.display()))?;
    if m.protocol_version != PROTOCOL_VERSION {
        bail!(
            "{} speaks protocol v{}, need v{PROTOCOL_VERSION}",
            bin.display(),
            m.protocol_version
        );
    }
    Ok(m)
}

/// Feed entries to a frontend and translate its answer.
pub fn run_frontend(
    module: &InstalledModule,
    entries: &[HistoryItem],
    extra_args: &[String],
) -> Result<ShowResponse> {
    use std::io::Write;
    let mut child = Command::new(&module.bin_path)
        .arg("run")
        .args(extra_args.iter())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning frontend {}", module.bin_path.display()))?;

    if let Some(mut stdin) = child.stdin.take() {
        serde_json::to_writer(
            &mut stdin,
            &ShowRequest {
                entries: entries.to_vec(),
            },
        )?;
        stdin.flush()?;
        drop(stdin); // signals EOF so menus can render
    }

    let mut response_line = String::new();
    if let Some(stdout) = child.stdout.as_mut() {
        BufReader::new(stdout).read_line(&mut response_line)?;
    }
    let status = child.wait()?;

    if response_line.trim().is_empty() {
        if status.success() {
            return Ok(ShowResponse::Dismissed);
        }
        bail!("frontend exited with {} without answering", status);
    }
    serde_json::from_str(response_line.trim())
        .with_context(|| format!("bad frontend reply: {response_line:?}"))
}

/// Writer half of a running reader: sends control frames on stdin.
pub struct ClipboardHandle {
    pub child: Child,
    pub(crate) tx: Sender<HostToClipboard>,
}

impl ClipboardHandle {
    pub fn spawn(module: &InstalledModule) -> Result<Self> {
        let mut child = Command::new(&module.bin_path)
            .arg("run")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning reader {}", module.bin_path.display()))?;

        let raw_stdin: ChildStdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("reader stdin unavailable"))?;
        let (tx, rx) = std::sync::mpsc::channel::<HostToClipboard>();
        std::thread::Builder::new()
            .name("reader-stdin".into())
            .spawn(move || {
                let mut w = std::io::LineWriter::new(raw_stdin);
                for frame in rx {
                    if serde_json::to_writer(&mut w, &frame).is_err() {
                        break;
                    }
                    if w.write_all(b"\n").is_err() {
                        break;
                    }
                    if matches!(frame, HostToClipboard::Stop) {
                        break;
                    }
                }
            })?;
        Ok(Self { child, tx })
    }

    /// Queue a control frame for the reader's stdin.
    pub fn send(&self, frame: HostToClipboard) -> Result<()> {
        self.tx
            .send(frame)
            .map_err(|_| anyhow::anyhow!("reader stdin closed"))
    }

    /// Spawn a thread parsing NDJSON stdout frames into a channel.
    pub fn pump_output(child: &mut Child, out: Sender<ClipboardToHost>) -> Result<()> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("reader stdout unavailable"))?;
        std::thread::Builder::new()
            .name("reader-stdout".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<ClipboardToHost>(&line) {
                        Ok(frame) => {
                            if out.send(frame).is_err() {
                                break;
                            }
                        }
                        Err(e) => log::warn!("unparsable reader line: {e}: {line:.120}"),
                    }
                }
            })?;
        Ok(())
    }
}

pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<()> {
    let got = hex::encode(Sha256::digest(bytes));
    if !got.eq_ignore_ascii_case(expected_hex.trim()) {
        bail!("sha256 mismatch: expected {expected_hex}, got {got}");
    }
    Ok(())
}

fn swap_symlink(dir: &Path, link_name: &str, target: &Path) -> Result<()> {
    let final_link = dir.join(link_name);
    let tmp = dir.join(format!("{link_name}.tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target.canonicalize()?, &tmp)
        .with_context(|| format!("linking {}", tmp.display()))?;
    std::fs::rename(&tmp, &final_link)
        .with_context(|| format!("activating {}", final_link.display()))
}

pub fn http_get(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(path).with_context(|| format!("reading {path}"));
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(c::CONNECT_TIMEOUT_SECS))
        .timeout(std::time::Duration::from_secs(c::DOWNLOAD_TIMEOUT_SECS))
        .user_agent(c::USER_AGENT)
        .build();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| anyhow!("GET {url} failed: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(c::MAX_RELEASE_MANIFEST_BYTES as u64 * 1024)
        .read_to_end(&mut buf)
        .with_context(|| format!("reading body of {url}"))?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_verification() {
        let data = b"cliphistory";
        let good = hex::encode(Sha256::digest(data));
        assert!(verify_sha256(data, &good).is_ok());
        assert!(verify_sha256(data, "deadbeef").is_err());
    }

    #[test]
    fn target_selection_prefers_override_then_gnu_then_musl() {
        let mk = |targets: &[&str]| ReleaseManifest {
            release: "v0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            targets: targets
                .iter()
                .map(|t| {
                    (
                        t.to_string(),
                        TargetAssets {
                            core: AssetRef {
                                path: "core".into(),
                                sha256: String::new(),
                            },
                            modules: vec![],
                        },
                    )
                })
                .collect(),
        };
        let cfg = ModulesConfig {
            platform_override: Some("custom-target".into()),
            ..Default::default()
        };
        let mgr = ModuleManager::new(cfg);
        let rm = mk(&[
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "custom-target",
        ]);
        assert!(mgr.select_target(&rm).is_ok());

        let cfg2 = ModulesConfig::default();
        let mgr2 = ModuleManager::new(cfg2);
        let rm_musl_only = mk(&["x86_64-unknown-linux-musl"]);
        assert_eq!(mgr2.select_target(&rm_musl_only).unwrap().core.path, "core");
        let rm_nothing = mk(&["mips-unknown-linux-uclibc"]);
        assert!(mgr2.select_target(&rm_nothing).is_err());
    }
}
