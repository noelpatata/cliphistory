//! X11 clipboard reader backed by `xclip`.
//!
//! X11 exposes no clipboard-change notification usable from a plain client,
//! so this module samples the CLIPBOARD selection every `POLL_INTERVAL_MS`
//! and emits an event whenever the content hash changes. Writes go through
//! `xclip` as well. The polling lifecycle lives in
//! [`cliphistory_module_common::reader`]; this file is the platform adapter.

use anyhow::{Context, Result};
use cliphistory_module_common as mcommon;
use cliphistory_module_common::reader::{PollingReader, run_polling};
use cliphistory_proto::{
    ClipboardToHost, Content, ModuleKind, ModuleManifest, CAP_READ, CAP_WRITE,
    PROTOCOL_VERSION,
};
use std::process::{Command, Stdio};

const MODULE_ID: &str = "clipboard-x11";
const TOOL: &str = "xclip";
/// How often the CLIPBOARD selection is re-sampled.
const POLL_INTERVAL_MS: u64 = 500;
/// MIME types probed in order; first hit wins.
const IMAGE_MIME: &str = "image/png";

fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: MODULE_ID.into(),
        kind: ModuleKind::Clipboard,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![CAP_READ.into(), CAP_WRITE.into()],
        features: vec![],
        requires: vec![TOOL.into()],
        description: "Polls the X11 CLIPBOARD selection via xclip".into(),
    }
}

struct X11Reader;

impl PollingReader for X11Reader {
    fn poll_interval_ms(&self) -> u64 {
        POLL_INTERVAL_MS
    }

    /// Fail fast with a precise message when xclip is missing.
    fn probe(&mut self) -> Result<()> {
        if which_xclip().is_none() {
            anyhow::bail!("{TOOL} not found on PATH");
        }
        Ok(())
    }

    /// Ask TARGETS first: browsers offer text/html for images, so flavor
    /// order — not "text first" — decides what we capture.
    fn sample(&self) -> Result<Option<Content>> {
        let targets = String::from_utf8_lossy(
            &run_xclip_output(&["-o", "-selection", "clipboard", "-t", "TARGETS"])
                .unwrap_or_default(),
        )
        .to_lowercase();

        if targets.lines().any(|t| t.trim() == IMAGE_MIME) {
            let png = run_xclip_output(&["-o", "-selection", "clipboard", "-t", IMAGE_MIME])?;
            if !png.is_empty() {
                let dims = cliphistory_image_utils::dimensions(&png);
                return Ok(Some(Content::Image {
                    mime: IMAGE_MIME.into(),
                    data: png,
                    width: dims.map(|d| d.0),
                    height: dims.map(|d| d.1),
                }));
            }
            return Ok(None);
        }

        let text = run_xclip_output(&["-o", "-selection", "clipboard"])?;
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(Content::Text {
            text: String::from_utf8_lossy(&text).into_owned(),
        }))
    }

    /// Detached: wait in background so slow xclip forks never block us.
    fn claim_async(&self, content: &Content) {
        std::thread::spawn({
            let content = content.clone();
            move || {
                if let Err(e) = set_clipboard(&content) {
                    let _ = mcommon::emit(&ClipboardToHost::Error {
                        message: format!("set-clipboard failed: {e:#}"),
                    });
                }
            }
        });
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--manifest") {
        return mcommon::manifest_main(manifest);
    }

    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            let _ = mcommon::emit(&ClipboardToHost::Error {
                message: format!("{e:#}"),
            });
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut reader = X11Reader;
    // Fail fast with a precise message when xclip is missing.
    reader.probe()?;
    mcommon::emit(&ClipboardToHost::Ready {
        protocol_version: PROTOCOL_VERSION,
    })?;
    run_polling(&mut reader)
}

// ---------------------------------------------------------------------------
// xclip plumbing
// ---------------------------------------------------------------------------

fn which_xclip() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(TOOL))
        .find(|p| p.is_file())
}

fn run_xclip_output(args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(TOOL)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("running {TOOL}"))?;
    if out.status.success() && out.stdout.is_empty() {
        // Distinguish "empty selection" from "error": both look similar here,
        // but a failing X connection also yields empty stdout. Probe TARGETS
        // to tell them apart cheaply.
        let probe = Command::new(TOOL)
            .args(["-o", "-selection", "clipboard", "-t", "TARGETS"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        if !probe.status.success() {
            anyhow::bail!("{TOOL} cannot access the selection");
        }
    }
    Ok(out.stdout)
}

fn set_clipboard(content: &Content) -> Result<()> {
    let (mime_flag, bytes): (Vec<String>, Vec<u8>) = match content {
        Content::Text { text } => (vec![], text.clone().into_bytes()),
        Content::Image { mime, data, .. } => (vec!["-t".into(), mime.clone()], data.clone()),
    };

    let mut child = Command::new(TOOL)
        .arg("-selection")
        .arg("clipboard")
        .args(&mime_flag)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {TOOL} for write"))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(&bytes)?;
        stdin.flush()?;
        // Dropping stdin lets xclip fork its serving child and exit.
    }
    // Detached: wait in background so slow forks never block us.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
