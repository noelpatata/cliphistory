//! Wayland clipboard reader.
//!
//! Samples the clipboard through `wl-clipboard-rs` (native Wayland client,
//! no external binaries) and can take ownership of the clipboard to fulfil
//! the daemon's write-back requests.

use anyhow::{Context, Result};
use cliphistory_clipboard_common::POLL_INTERVAL_MS;
use cliphistory_proto::{
    ClipboardToHost, Content, HostToClipboard, ModuleKind, ModuleManifest, CAP_READ, CAP_WRITE,
    PROTOCOL_VERSION,
};
use sha2::{Digest, Sha256};
use std::io::{BufReader, Read};
use std::sync::mpsc;
use std::time::Duration;
use wl_clipboard_rs::{copy as wlc, paste as wlp};

const MODULE_ID: &str = "clipboard-wayland";

fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: MODULE_ID.into(),
        kind: ModuleKind::Clipboard,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![CAP_READ.into(), CAP_WRITE.into()],
        features: vec![],
        requires: vec![],
        description: "Watches and owns the Wayland clipboard (wl-clipboard-rs)".into(),
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
    let (tx, rx) = mpsc::channel::<Incoming>();

    // stdin -> control frames
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let mut reader = BufReader::new(std::io::stdin().lock());
            while let Some(frame) = cliphistory_clipboard_common::next_host_frame(&mut reader) {
                if tx.send(Incoming::Control(frame)).is_err() {
                    break;
                }
            }
        })
        .context("spawning stdin thread")?;

    // Sanity probe: fail fast with a useful message when no compositor is up.
    if let Err(e) = wlp::get_mime_types(wlp::ClipboardType::Regular, wlp::Seat::Unspecified) {
        return Err(anyhow::anyhow!("wayland clipboard unavailable: {e}"));
    }

    cliphistory_clipboard_common::emit(&ClipboardToHost::Ready {
        protocol_version: PROTOCOL_VERSION,
    })?;
    let mut last_hash = String::new();

    loop {
        match rx.recv_timeout(Duration::from_millis(POLL_INTERVAL_MS)) {
            Ok(Incoming::Control(HostToClipboard::Ping)) => {
                cliphistory_clipboard_common::emit(&ClipboardToHost::Pong)?;
            }
            Ok(Incoming::Control(HostToClipboard::SetClipboard { content })) => {
                // Detached thread: serving the selection lasts until another
                // owner appears; the event loop must keep running.
                std::thread::spawn(move || {
                    if let Err(e) = set_clipboard(&content) {
                        log_frame_error(&format!("set-clipboard failed: {e:#}"));
                    }
                });
            }
            Ok(Incoming::Control(HostToClipboard::Stop))
            | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => match read_clipboard() {
                Ok(Some(content)) => {
                    let hash = content_hash(&content.bytes());
                    if hash != last_hash {
                        last_hash = hash;
                        cliphistory_clipboard_common::emit(&ClipboardToHost::Event { content })?;
                    }
                }
                Ok(None) => {}
                Err(e) => log_frame_error(&format!("read failed: {e:#}")),
            },
        }
    }
}

fn log_frame_error(msg: &str) {
    let _ = cliphistory_clipboard_common::emit(&ClipboardToHost::Error {
        message: msg.to_string(),
    });
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Plain-text MIME types we accept (lowercased). `text/html` is
/// deliberately absent: browsers offer it alongside everything else, and
/// capturing it turns copied images into `<img src=...>` markup soup.
const PLAIN_TEXT_MIMES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "utf8_string",
    "string",
    "text",
];

/// Read the regular clipboard, choosing the best offered flavor:
/// `image/png` beats plain text; anything else is ignored.
fn read_clipboard() -> Result<Option<Content>> {
    let offered_set = wlp::get_mime_types(wlp::ClipboardType::Regular, wlp::Seat::Unspecified)?;
    let mut offered: Vec<String> = offered_set.into_iter().collect();
    offered.sort();
    let has = |want: &str| offered.iter().any(|o| o.eq_ignore_ascii_case(want));

    if has("image/png") {
        return read_specific("image/png").map(Some);
    }
    if PLAIN_TEXT_MIMES.iter().any(|m| has(m)) {
        return read_specific("text/plain;charset=utf-8").map(Some);
    }
    Ok(None)
}

fn read_specific(mime: &str) -> Result<Content> {
    let result = wlp::get_contents(
        wlp::ClipboardType::Regular,
        wlp::Seat::Unspecified,
        wlp::MimeType::Specific(mime),
    );
    match result {
        Ok((mut pipe, _actual_mime)) => {
            let mut buf = Vec::new();
            pipe.read_to_end(&mut buf)?;
            if buf.is_empty() {
                anyhow::bail!("empty payload for {mime}");
            }
            if mime.starts_with("text/") {
                Ok(Content::Text {
                    text: String::from_utf8_lossy(&buf).into_owned(),
                })
            } else {
                let dims = cliphistory_image_utils::dimensions(&buf);
                Ok(Content::Image {
                    mime: mime.to_string(),
                    data: buf,
                    width: dims.map(|d| d.0),
                    height: dims.map(|d| d.1),
                })
            }
        }
        Err(wlp::Error::NoSeats | wlp::Error::ClipboardEmpty | wlp::Error::NoMimeType) => {
            Err(anyhow::anyhow!("flavor vanished: {mime}"))
        }
        Err(e) => Err(anyhow::anyhow!("paste of {mime} failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

fn set_clipboard(content: &Content) -> Result<()> {
    let opts = wlc::Options::new();
    match content {
        Content::Text { text } => opts.copy(
            wlc::Source::Bytes(text.clone().into_bytes().into_boxed_slice()),
            wlc::MimeType::Text,
        ),
        Content::Image { mime, data, .. } => opts.copy(
            wlc::Source::Bytes(data.clone().into_boxed_slice()),
            wlc::MimeType::Specific(mime.clone()),
        ),
    }
    .context("claiming clipboard ownership")
}

pub(crate) fn content_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
