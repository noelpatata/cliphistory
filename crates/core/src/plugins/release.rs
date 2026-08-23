//! Release-manifest access: URL construction, download, checksum and
//! target-triple selection. Read-only — no filesystem side effects.

use super::model::{ReleaseManifest, TargetAssets};
use super::ModuleManager;
use crate::constants as c;
use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Read;

impl ModuleManager {
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
        if rm.protocol_version != c::PROTOCOL_VERSION {
            bail!(
                "remote manifest speaks protocol v{}, this build speaks v{}",
                rm.protocol_version,
                c::PROTOCOL_VERSION
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
        anyhow::bail!(
            "release {} ships no artifacts for this machine ({arch}); targets: {:?}",
            rm.release,
            rm.targets.keys().collect::<Vec<_>>()
        );
    }

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

    pub(crate) fn asset_url(&self, release: &str, rel_path: &str) -> String {
        if self.is_local_source() {
            return format!("{}/{}", self.source_base(), rel_path);
        }
        format!(
            "{}/releases/download/{release}/{rel_path}",
            self.source_base()
        )
    }
}

pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<()> {
    let got = hex::encode(Sha256::digest(bytes));
    if !got.eq_ignore_ascii_case(expected_hex.trim()) {
        bail!("sha256 mismatch: expected {expected_hex}, got {got}");
    }
    Ok(())
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

use anyhow::bail;
