//! X11 clipboard reader backed by `xclip`.
//!
//! X11 exposes no clipboard-change notification usable from a plain client,
//! so this module samples the CLIPBOARD selection every
//! [`cliphistory_clipboard_common::POLL_INTERVAL_MS`] and emits an event whenever
//! the content hash changes. Writes go through `xclip` as well.

use anyhow::{Context, Result};
use cliphistory_clipboard_common::POLL_INTERVAL_MS;
use cliphistory_proto::{
    ClipboardToHost, Content, HostToClipboard, ModuleKind, ModuleManifest, CAP_READ, CAP_WRITE,
    PROTOCOL_VERSION,
};
use sha2::{Digest, Sha256};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const MODULE_ID: &str = "clipboard-x11";
const TOOL: &str = "xclip";
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

enum Incoming {
    Control(HostToClipboard),
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--manifest") {
        return match cliphistory_clipboard_common::print_manifest(&manifest()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e:#}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            let _ = cliphistory_clipboard_common::emit(&ClipboardToHost::Error {
                message: format!("{e:#}"),
            });
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    // Fail fast with a precise message when xclip is missing.
    if which_xclip().is_none() {
        anyhow::bail!("{TOOL} not found on PATH");
    }

    let (tx, rx) = mpsc::channel::<Incoming>();
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(std::io::stdin().lock());
            while let Some(frame) = cliphistory_clipboard_common::next_host_frame(&mut reader) {
                if tx.send(Incoming::Control(frame)).is_err() {
                    break;
                }
            }
        })
        .context("spawning stdin thread")?;

    cliphistory_clipboard_common::emit(&ClipboardToHost::Ready {
        protocol_version: PROTOCOL_VERSION,
    })?;
    let mut last_hash = String::new();
    let mut consecutive_failures: u32 = 0;

    loop {
        match rx.recv_timeout(Duration::from_millis(POLL_INTERVAL_MS)) {
            Ok(Incoming::Control(HostToClipboard::Ping)) => {
                cliphistory_clipboard_common::emit(&ClipboardToHost::Pong)?;
            }
            Ok(Incoming::Control(HostToClipboard::SetClipboard { content })) => {
                std::thread::spawn(move || {
                    if let Err(e) = set_clipboard(&content) {
                        log_frame_error(&format!("set-clipboard failed: {e:#}"));
                    }
                });
            }
            Ok(Incoming::Control(HostToClipboard::Stop))
            | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                match read_clipboard() {
                    Ok(Some(content)) => {
                        consecutive_failures = 0;
                        let hash = content_hash(&content.bytes());
                        if hash != last_hash {
                            last_hash = hash;
                            cliphistory_clipboard_common::emit(&ClipboardToHost::Event {
                                content,
                            })?;
                        }
                    }
                    Ok(None) => {
                        consecutive_failures = 0;
                        last_hash.clear(); // clipboard emptied
                    }
                    Err(e) => {
                        // A transient X error (e.g. session teardown) must not
                        // spam the host forever.
                        consecutive_failures += 1;
                        if consecutive_failures <= 3 {
                            log_frame_error(&format!("read failed: {e:#}"));
                        } else if consecutive_failures == 4 {
                            log_frame_error("read keeps failing; silencing further reports");
                        }
                    }
                }
            }
        }
    }
}

fn log_frame_error(msg: &str) {
    let _ = cliphistory_clipboard_common::emit(&ClipboardToHost::Error {
        message: msg.to_string(),
    });
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

fn read_clipboard() -> Result<Option<Content>> {
    // Ask TARGETS first: browsers offer text/html for images, so flavor
    // order — not "text first" — decides what we capture.
    let targets = String::from_utf8_lossy(
        &run_xclip_output(&["-o", "-selection", "clipboard", "-t", "TARGETS"]).unwrap_or_default(),
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

pub(crate) fn content_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
