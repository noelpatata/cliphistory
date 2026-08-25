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

#[cfg(test)]
mod tests {
    use super::*;

    fn mm_with_source(url: &str) -> ModuleManager {
        let mut cfg = crate::config::ModulesConfig::default();
        cfg.source_url = url.into();
        ModuleManager::new(cfg)
    }

    #[test]
    fn verify_sha256_accepts_correct_hash() {
        let bytes = b"hello world";
        let hash = hex::encode(Sha256::digest(bytes));
        assert!(verify_sha256(bytes, &hash).is_ok());
    }

    #[test]
    fn verify_sha256_rejects_wrong_hash() {
        assert!(verify_sha256(
            b"hello",
            "0000000000000000000000000000000000000000000000000000000000000000"
        )
        .is_err());
    }

    #[test]
    fn verify_sha256_is_case_insensitive() {
        let bytes = b"test";
        let hash_upper = hex::encode(Sha256::digest(bytes)).to_uppercase();
        assert!(verify_sha256(bytes, &hash_upper).is_ok());
    }

    #[test]
    fn verify_sha256_trims_whitespace() {
        let bytes = b"test";
        let hash = format!("  {}  ", hex::encode(Sha256::digest(bytes)));
        assert!(verify_sha256(bytes, &hash).is_ok());
    }

    #[test]
    fn verify_sha256_empty_input() {
        let hash = hex::encode(Sha256::digest(b""));
        assert!(verify_sha256(b"", &hash).is_ok());
    }

    #[test]
    fn manifest_url_local_no_tag() {
        let mm = mm_with_source("file:///tmp/test");
        assert_eq!(mm.manifest_url(None), "file:///tmp/test/manifest.json");
    }

    #[test]
    fn manifest_url_local_with_tag() {
        let mm = mm_with_source("file:///tmp/test");
        assert_eq!(
            mm.manifest_url(Some("v1.0")),
            "file:///tmp/test/v1.0/manifest.json"
        );
    }

    #[test]
    fn manifest_url_remote_no_tag() {
        let mm = mm_with_source("https://github.com/noelpatata/cliphistory");
        assert_eq!(
            mm.manifest_url(None),
            "https://github.com/noelpatata/cliphistory/releases/latest/download/manifest.json"
        );
    }

    #[test]
    fn manifest_url_remote_with_tag() {
        let mm = mm_with_source("https://github.com/noelpatata/cliphistory");
        assert_eq!(
            mm.manifest_url(Some("v1.0")),
            "https://github.com/noelpatata/cliphistory/releases/download/v1.0/manifest.json"
        );
    }

    #[test]
    fn asset_url_local_source() {
        let mm = mm_with_source("file:///tmp/test");
        assert_eq!(
            mm.asset_url("v1.0", "modules/clipboard-wayland"),
            "file:///tmp/test/modules/clipboard-wayland"
        );
    }

    #[test]
    fn asset_url_remote_source() {
        let mm = mm_with_source("https://github.com/noelpatata/cliphistory");
        assert_eq!(
            mm.asset_url("v1.0", "cliphistory-clipboard-wayland"),
            "https://github.com/noelpatata/cliphistory/releases/download/v1.0/cliphistory-clipboard-wayland"
        );
    }

    #[test]
    fn select_target_prefers_gnu_then_musl() {
        use super::super::model::{AssetRef, ReleaseManifest, TargetAssets};
        let arch = std::env::consts::ARCH;
        let mut targets = std::collections::BTreeMap::new();
        targets.insert(
            format!("{arch}-unknown-linux-musl"),
            TargetAssets {
                core: AssetRef {
                    path: "core".into(),
                    sha256: "aa".into(),
                },
                modules: vec![],
            },
        );
        let rm = ReleaseManifest {
            release: "v1.0".into(),
            protocol_version: crate::constants::PROTOCOL_VERSION,
            targets,
        };
        let mm = mm_with_source("https://example.com");
        let ta = mm.select_target(&rm).unwrap();
        assert_eq!(ta.core.path, "core");
    }

    #[test]
    fn select_target_prefers_gnu_over_musl() {
        use super::super::model::{AssetRef, ReleaseManifest, TargetAssets};
        let arch = std::env::consts::ARCH;
        let mut targets = std::collections::BTreeMap::new();
        targets.insert(
            format!("{arch}-unknown-linux-musl"),
            TargetAssets {
                core: AssetRef {
                    path: "musl-core".into(),
                    sha256: "aa".into(),
                },
                modules: vec![],
            },
        );
        targets.insert(
            format!("{arch}-unknown-linux-gnu"),
            TargetAssets {
                core: AssetRef {
                    path: "gnu-core".into(),
                    sha256: "bb".into(),
                },
                modules: vec![],
            },
        );
        let rm = ReleaseManifest {
            release: "v1.0".into(),
            protocol_version: crate::constants::PROTOCOL_VERSION,
            targets,
        };
        let mm = mm_with_source("https://example.com");
        let ta = mm.select_target(&rm).unwrap();
        assert_eq!(ta.core.path, "gnu-core");
    }

    #[test]
    fn select_target_fails_on_missing_arch() {
        use super::super::model::{AssetRef, ReleaseManifest, TargetAssets};
        let mut targets = std::collections::BTreeMap::new();
        targets.insert(
            "aarch64-unknown-linux-gnu".into(),
            TargetAssets {
                core: AssetRef {
                    path: "core".into(),
                    sha256: "aa".into(),
                },
                modules: vec![],
            },
        );
        let rm = ReleaseManifest {
            release: "v1.0".into(),
            protocol_version: crate::constants::PROTOCOL_VERSION,
            targets,
        };
        let mm = mm_with_source("https://example.com");
        assert!(mm.select_target(&rm).is_err());
    }

    #[test]
    fn source_base_strips_trailing_slash() {
        let mm = mm_with_source("https://example.com/repo/");
        assert_eq!(mm.source_base(), "https://example.com/repo");
    }

    #[test]
    fn is_local_source_file_scheme() {
        assert!(mm_with_source("file:///tmp").is_local_source());
    }

    #[test]
    fn is_local_source_plain_path() {
        assert!(mm_with_source("/tmp/test").is_local_source());
    }

    #[test]
    fn is_local_source_http() {
        assert!(!mm_with_source("https://example.com").is_local_source());
    }
}
