//! End-to-end plugin manager test using a fake module binary served from a
//! local `file://` release source. No network, no compositor required.

use anyhow::Result;
use cliphistory_core::config::ModulesConfig;
use cliphistory_core::plugins::{
    AssetRef, ClipboardHandle, ModuleManager, ReleaseManifest, RemoteModule, TargetAssets,
};
use cliphistory_proto::{ClipboardToHost, HostToClipboard, ModuleKind, PROTOCOL_VERSION};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

fn sha256_file(p: &Path) -> String {
    hex::encode(Sha256::digest(fs::read(p).unwrap()))
}

/// A fake reader module: answers `--manifest`, then in `run` mode emits
/// Ready and replies Pong until told to Stop.
const FAKE_CLIPBOARD_TEMPLATE: &str = r#"#!/bin/sh
if [ "$1" = "--manifest" ]; then
  printf '{"id":"clipboard-fake","kind":"clipboard","version":"v0.9.9","protocol_version":__PV__,"capabilities":["read","write"],"requires":[],"description":"fake"}\n'
  exit 0
fi
printf '{"type":"ready","protocol_version":__PV__}\n'
while IFS= read -r line; do
  case "$line" in
    *stop*) break ;;
    *ping*) printf '{"type":"pong"}\n' ;;
  esac
done
"#;

fn write_fake_release(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let bin = dir.join("cliphistory-clipboard-fake-x86_64-unknown-linux-gnu");
    fs::write(
        &bin,
        FAKE_CLIPBOARD_TEMPLATE.replace("__PV__", &PROTOCOL_VERSION.to_string()),
    )?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))?;

    let manifest = ReleaseManifest {
        release: "v0.9.9".into(),
        protocol_version: PROTOCOL_VERSION,
        targets: [(
            "x86_64-unknown-linux-gnu".to_string(),
            TargetAssets {
                core: AssetRef {
                    path: "core".into(),
                    sha256: String::new(),
                },
                modules: vec![RemoteModule {
                    id: "clipboard-fake".into(),
                    kind: ModuleKind::Clipboard,
                    capabilities: vec!["read".into(), "write".into()],
                    requires: vec![],
                    features: vec![],
                    description: "fake clipboard module".into(),
                    file: AssetRef {
                        path: "cliphistory-clipboard-fake-x86_64-unknown-linux-gnu".into(),
                        sha256: sha256_file(&bin),
                    },
                }],
            },
        )]
        .into_iter()
        .collect(),
    };
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

#[test]
fn install_verify_spawn_lifecycle() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let source_dir = tmp.path().join("source");
    let install_dir = tmp.path().join("modules");
    write_fake_release(&source_dir)?;

    let cfg = ModulesConfig {
        source_url: format!("file://{}", source_dir.display()),
        install_dir: install_dir.clone(),
        ..Default::default()
    };
    let mm = ModuleManager::new(cfg);

    // Nothing installed yet.
    assert!(mm.list_installed()?.is_empty());

    // Install through the full pipeline (download -> checksum -> activate).
    for r in mm.ensure_available(&["clipboard-fake".into()], false, &|_| {}) {
        r.expect("install should succeed");
    }

    // Resolvable + correct manifest + version recorded.
    let installed = mm.resolve("clipboard-fake").expect("module installed");
    assert_eq!(installed.manifest.id, "clipboard-fake");
    assert_eq!(installed.version, "v0.9.9");
    let listed = mm.list_installed()?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].version, "v0.9.9");

    // Idempotent ensure_available.
    let again = mm.ensure_available(&["clipboard-fake".into()], false, &|_| {});
    assert!(again[0]
        .as_ref()
        .expect("second pass")
        .starts_with("clipboard-fake: already installed"));

    // Spawn + protocol handshake + ping/pong + stop round trip.
    let mut handle = ClipboardHandle::spawn(&installed)?;
    let (tx, rx) = mpsc::channel::<ClipboardToHost>();
    ClipboardHandle::pump_output(&mut handle.child, tx)?;

    match rx.recv_timeout(Duration::from_secs(5))? {
        ClipboardToHost::Ready { protocol_version } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
        }
        other => panic!("expected Ready, got {other:?}"),
    }

    handle.send(HostToClipboard::Ping)?;
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))?,
        ClipboardToHost::Pong
    );

    handle.send(HostToClipboard::Stop)?;
    let status = handle.child.wait()?;
    assert!(status.success());

    // Uninstall removes everything.
    mm.uninstall("clipboard-fake")?;
    assert!(mm.resolve("clipboard-fake").is_none());
    assert!(mm.list_installed()?.is_empty());
    Ok(())
}

#[test]
fn corrupt_download_is_rejected() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let source_dir = tmp.path().join("source");
    write_fake_release(&source_dir)?;

    // Tamper with the artifact after the manifest was written.
    let bin = source_dir.join("cliphistory-clipboard-fake-x86_64-unknown-linux-gnu");
    fs::write(&bin, b"tampered payload")?;

    let cfg = ModulesConfig {
        source_url: format!("file://{}", source_dir.display()),
        install_dir: tmp.path().join("modules"),
        ..Default::default()
    };
    let mm = ModuleManager::new(cfg);

    let results = mm.ensure_available(&["clipboard-fake".into()], false, &|_| {});
    let err = results.into_iter().next().unwrap().expect_err("must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("sha256")
            || msg.to_lowercase().contains("mismatch")
            || msg.contains("404")
            || msg.contains("GET"),
        "unexpected error: {msg}"
    );
    assert!(
        mm.resolve("clipboard-fake").is_none(),
        "nothing must be installed on failure"
    );
    Ok(())
}
