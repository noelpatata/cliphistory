//! Wayland clipboard reader.
//!
//! Samples the clipboard through `wl-clipboard-rs` (native Wayland client,
//! no external binaries) and can take ownership of the clipboard to fulfil
//! the daemon's write-back requests. The polling lifecycle lives in
//! [`cliphistory_module_common::reader`]; this file is the platform adapter.

use anyhow::{Context, Result};
use cliphistory_module_common as mcommon;
use cliphistory_module_common::reader::PollingReader;
use cliphistory_proto::{
    ClipboardToHost, Content, ModuleKind, ModuleManifest, CAP_READ, CAP_WRITE,
    PROTOCOL_VERSION,
};
use wl_clipboard_rs::{copy as wlc, paste as wlp};

const MODULE_ID: &str = "clipboard-wayland";
/// How often the clipboard is re-sampled when no change notification exists.
const POLL_INTERVAL_MS: u64 = 500;

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

struct WaylandReader;

impl PollingReader for WaylandReader {
    fn poll_interval_ms(&self) -> u64 {
        POLL_INTERVAL_MS
    }

    /// Fail fast when no compositor/data-control exists. An empty clipboard
    /// is a normal state, not an error — the poll loop tolerates it, so
    /// those errors must not abort startup (a daemon that boots before the
    /// first copy would otherwise kill its module 5 times and disable
    /// clipboard tracking entirely).
    fn probe(&mut self) -> Result<()> {
        if let Err(e) = wlp::get_mime_types(wlp::ClipboardType::Regular, wlp::Seat::Unspecified) {
            match e {
                wlp::Error::NoSeats | wlp::Error::ClipboardEmpty | wlp::Error::NoMimeType => {
                    log_frame_error("clipboard currently empty; waiting for content");
                }
                other => {
                    return Err(anyhow::anyhow!("wayland clipboard unavailable: {other}"));
                }
            }
        }
        Ok(())
    }

    /// Ask TARGETS first: browsers offer text/html for images, so flavor
    /// order — not "text first" — decides what we capture.
    fn sample(&self) -> Result<Option<Content>> {
        let offered_set =
            wlp::get_mime_types(wlp::ClipboardType::Regular, wlp::Seat::Unspecified)?;
        let mut offered: Vec<String> = offered_set.into_iter().collect();
        offered.sort();
        let has = |want: &str| offered.iter().any(|o| o.eq_ignore_ascii_case(want));

        if has("image/png") {
            return read_specific("image/png").map(Some);
        }
        const PLAIN_TEXT_MIMES: &[&str] = &[
            "text/plain;charset=utf-8",
            "text/plain",
            "utf8_string",
            "string",
            "text",
        ];
        if PLAIN_TEXT_MIMES.iter().any(|m| has(m)) {
            return read_specific("text/plain;charset=utf-8").map(Some);
        }
        Ok(None)
    }

    /// Take ownership of the clipboard with `content`. Detached thread:
    /// serving the selection lasts until another owner appears; the event
    /// loop must keep running.
    fn claim_async(&self, content: &Content) {
        std::thread::spawn({
            let content = content.clone();
            move || {
                let t = std::time::Instant::now();
                let result = set_clipboard(&content);
                log::info!(
                    "selection claimed in {:?} ({} bytes, {:?})",
                    t.elapsed(),
                    content.bytes().len(),
                    content.kind()
                );
                if let Err(e) = result {
                    log_frame_error(&format!("set-clipboard failed: {e:#}"));
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
    mcommon::init_logging();

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
    let mut reader = WaylandReader;
    // Sanity probe happens inside connect(); a failure here aborts startup
    // with a precise message instead of a silent dead module.
    reader.probe()?;
    mcommon::emit(&ClipboardToHost::Ready {
        protocol_version: PROTOCOL_VERSION,
    })?;
    mcommon::reader::run_polling(&mut reader)
}

fn log_frame_error(msg: &str) {
    let _ = mcommon::emit(&ClipboardToHost::Error {
        message: msg.to_string(),
    });
}

// ---------------------------------------------------------------------------
// Reading / writing via wl-clipboard-rs
// ---------------------------------------------------------------------------

fn read_specific(mime: &str) -> Result<Content> {
    let result = wlp::get_contents(
        wlp::ClipboardType::Regular,
        wlp::Seat::Unspecified,
        wlp::MimeType::Specific(mime),
    );
    match result {
        Ok((mut pipe, _actual_mime)) => {
            use std::io::Read;
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
            anyhow::bail!("flavor vanished: {mime}")
        }
        Err(e) => anyhow::bail!("paste of {mime} failed: {e}"),
    }
}

fn set_clipboard(content: &Content) -> Result<()> {
    fn claim(
        content: &Content,
        clipboard: wlc::ClipboardType,
    ) -> std::result::Result<(), wlc::Error> {
        let mut opts = wlc::Options::new();
        opts.clipboard(clipboard);
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
    }

    // Claim CLIPBOARD, then also PRIMARY, so Shift+Insert / middle-click
    // style pastes find our data regardless of which selection they read.
    // Each claim keeps serving until someone else takes the selection over.
    claim(content, wlc::ClipboardType::Regular).context("claiming clipboard ownership")?;
    if let Err(e) = claim(content, wlc::ClipboardType::Primary) {
        log::debug!("primary selection claim failed: {e}");
    }
    Ok(())
}
