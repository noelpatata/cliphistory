//! Shared reader skeleton for clipboard modules.
//!
//! Both backends follow the identical lifecycle: fail-fast startup probe,
//! `Ready`, then a loop that multiplexes host control frames with
//! clipboard-change detection and content deduplication by hash. Backends
//! only implement [`PollingReader`]; this module owns the loop.
//!
//! Change detection is **event-driven on both platforms** — the loop
//! sleeps in `recv()` until something actually happens:
//!
//! * **Wayland** — data-control `selection` events from the compositor.
//! * **X11** — XFixes `SelectionNotify` on a registered window.
//!
//! There is no polling anywhere. A backend whose platform lacks change
//! events cannot detect copies and therefore cannot be supported; the
//! event source is mandatory (see [`PollingReader::attach_wake`]).

use crate::{emit, next_frame};
use anyhow::Result;
use cliphistory_proto::{ClipboardToHost, Content, HostToClipboard};
use std::sync::mpsc;

/// What woke the main loop.
pub enum Wake {
    /// A control frame arrived from the host.
    Frame(HostToClipboard),
    /// The platform clipboard changed; call `sample`.
    Changed,
}

/// A platform clipboard backend driven by [`run_event_loop`].
pub trait PollingReader {
    /// Fail-fast startup check (compositor present, tool installed…).
    /// Tolerated conditions are reported by the implementation itself via
    /// `Error` frames; returning `Err` kills the module.
    fn probe(&mut self) -> Result<()>;

    /// Current clipboard content, if any is readable.
    fn sample(&self) -> Result<Option<Content>>;

    /// Take ownership of the clipboard so future reads return `content`.
    /// Implementations decide whether to serve inline or detached.
    fn claim_async(&self, content: &Content);

    /// Attach the platform's change-notification source: spawn a watcher
    /// that sends [`Wake::Changed`] on every observed clipboard change.
    ///
    /// This source is mandatory — without it copies cannot be detected —
    /// so implementations are expected to require whatever platform
    /// facility provides it (Wayland data-control, X11 XFixes) exactly as
    /// their read path already does. Internal failures are logged and may
    /// silence further detection; the supervisor restarts the module if
    /// its connection drops.
    fn attach_wake(&self, tx: mpsc::Sender<Wake>);
}

/// Run the reader protocol over stdio until `Stop`, EOF or watcher death.
///
/// Sample errors are reported as `Error` frames but silenced after three
/// consecutive occurrences so a dying session never spams the host.
pub fn run_event_loop<R: PollingReader>(reader: &mut R) -> Result<()> {
    // stdin -> control frames
    let (tx, rx) = mpsc::channel::<Wake>();
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn({
            let tx = tx.clone();
            move || {
                let mut r = std::io::BufReader::new(std::io::stdin().lock());
                while let Some(frame) = next_frame::<HostToClipboard, _>(&mut r, |_line, _e| {}) {
                    if tx.send(Wake::Frame(frame)).is_err() {
                        break;
                    }
                }
            }
        })
        .expect("spawning stdin thread");

    reader.probe()?;
    emit(&ClipboardToHost::Ready {
        protocol_version: cliphistory_proto::PROTOCOL_VERSION,
    })?;
    reader.attach_wake(tx);

    let mut last_hash = String::new();
    let mut consecutive_failures: u32 = 0;

    loop {
        match rx.recv() {
            Ok(Wake::Frame(HostToClipboard::Ping)) => emit(&ClipboardToHost::Pong)?,
            Ok(Wake::Frame(HostToClipboard::SetClipboard { content })) => {
                reader.claim_async(&content)
            }
            Ok(Wake::Frame(HostToClipboard::Stop)) => return Ok(()),
            // Real change; hash-dedup makes redundant wakes cheap.
            Ok(Wake::Changed) => sample(reader, &mut last_hash, &mut consecutive_failures)?,
            // Both senders gone: stdin closed *and* the watcher died.
            Err(_) => return Ok(()),
        }
    }
}

/// One clipboard sample with deduplication and error silencing.
fn sample<R: PollingReader>(
    reader: &mut R,
    last_hash: &mut String,
    consecutive_failures: &mut u32,
) -> Result<()> {
    match reader.sample() {
        Ok(Some(content)) => {
            *consecutive_failures = 0;
            let hash = crate::util::sha256_hex(&content.bytes());
            if hash != *last_hash {
                *last_hash = hash;
                emit(&ClipboardToHost::Event { content })?;
            }
        }
        Ok(None) => {
            *consecutive_failures = 0;
            // Clipboard emptied: forget the hash so re-adding the same
            // content is reported again.
            last_hash.clear();
        }
        Err(e) => {
            *consecutive_failures += 1;
            if *consecutive_failures <= 3 {
                let _ = emit(&ClipboardToHost::Error {
                    message: format!("read failed: {e:#}"),
                });
            } else if *consecutive_failures == 4 {
                let _ = emit(&ClipboardToHost::Error {
                    message: "read keeps failing; silencing further reports".into(),
                });
            }
        }
    }
    Ok(())
}
