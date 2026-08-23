//! Shared polling-reader skeleton for clipboard modules.
//!
//! Both backends follow the identical lifecycle: fail-fast startup probe,
//! `Ready`, then a loop that multiplexes host control frames with periodic
//! sampling of the platform clipboard, deduplicating by content hash.
//! Backends only implement [`PollingReader`]; this module owns the loop.

use crate::{emit, next_frame};
use anyhow::Result;
use cliphistory_proto::{ClipboardToHost, Content, HostToClipboard};
use std::sync::mpsc;

/// A platform clipboard backend driven by [`run_polling`].
pub trait PollingReader {
    /// Milliseconds between samples.
    fn poll_interval_ms(&self) -> u64;

    /// Fail-fast startup check (compositor present, tool installed…).
    /// Tolerated conditions are reported by the implementation itself via
    /// `Error` frames; returning `Err` kills the module.
    fn probe(&mut self) -> Result<()>;

    /// Current clipboard content, if any is readable.
    fn sample(&self) -> Result<Option<Content>>;

    /// Take ownership of the clipboard so future reads return `content`.
    /// Implementations decide whether to serve inline or detached.
    fn claim_async(&self, content: &Content);
}

/// Run the polling protocol over stdio until `Stop` or EOF.
///
/// Sample errors are reported as `Error` frames but silenced after three
/// consecutive occurrences so a dying session never spams the host.
pub fn run_polling<R: PollingReader>(reader: &mut R) -> Result<()> {
    let poll = std::time::Duration::from_millis(reader.poll_interval_ms());

    // stdin -> control frames
    let (tx, rx) = mpsc::channel::<HostToClipboard>();
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let mut r = std::io::BufReader::new(std::io::stdin().lock());
            while let Some(frame) =
                next_frame::<HostToClipboard, _>(&mut r, |_line, _e| {})
            {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        })
        .expect("spawning stdin thread");

    reader.probe()?;
    emit(&ClipboardToHost::Ready {
        protocol_version: cliphistory_proto::PROTOCOL_VERSION,
    })?;

    let mut last_hash = String::new();
    let mut consecutive_failures: u32 = 0;

    loop {
        use std::sync::mpsc::RecvTimeoutError;
        match rx.recv_timeout(poll) {
            Ok(HostToClipboard::Ping) => emit(&ClipboardToHost::Pong)?,
            Ok(HostToClipboard::SetClipboard { content }) => reader.claim_async(&content),
            Ok(HostToClipboard::Stop)
            | Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => match reader.sample() {
                Ok(Some(content)) => {
                    consecutive_failures = 0;
                    let hash = crate::util::sha256_hex(&content.bytes());
                    if hash != last_hash {
                        last_hash = hash;
                        emit(&ClipboardToHost::Event { content })?;
                    }
                }
                Ok(None) => {
                    consecutive_failures = 0;
                    // Clipboard emptied: forget the hash so re-adding the
                    // same content is reported again.
                    last_hash.clear();
                }
                Err(e) => {
                    consecutive_failures += 1;
                    if consecutive_failures <= 3 {
                        let _ = emit(&ClipboardToHost::Error {
                            message: format!("read failed: {e:#}"),
                        });
                    } else if consecutive_failures == 4 {
                        let _ = emit(&ClipboardToHost::Error {
                            message: "read keeps failing; silencing further reports".into(),
                        });
                    }
                }
            },
        }
    }
}
