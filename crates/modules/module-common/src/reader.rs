//! Shared reader skeleton for clipboard modules.
//!
//! Both backends follow the identical lifecycle: fail-fast startup probe,
//! `Ready`, then a loop that multiplexes host control frames with
//! clipboard-change detection and content deduplication by hash. Backends
//! only implement [`PollingReader`]; this module owns the loop.
//!
//! Change detection comes in two flavours:
//!
//! * **event-driven** — the backend attaches a watcher thread via
//!   [`PollingReader::attach_wake`] (Wayland data-control events) and the
//!   loop sleeps in `recv()`, waking only for real changes or control
//!   frames. A slow safety-net sample every [`EVENT_SAMPLE_FLOOR`] guards
//!   against missed events.
//! * **polling** — backends without any change notification mechanism
//!   fall back to sampling every [`PollingReader::poll_interval_ms`].
//!
//! This split is deliberate: X11's clipboard protocol has no change
//! notification mechanism at all, so polling there is a platform
//! limitation, not an implementation choice. Wayland is strictly
//! event-driven.

use crate::{emit, next_frame};
use anyhow::Result;
use cliphistory_proto::{ClipboardToHost, Content, HostToClipboard};
use std::sync::mpsc;
use std::time::Duration;

/// In event-driven mode, force one sample at least this often as a
/// safety net against missed compositor events. Purely a correctness
/// guard: it costs one cheap read per interval.
const EVENT_SAMPLE_FLOOR: Duration = Duration::from_secs(30);

/// What woke the main loop.
pub enum Wake {
    /// A control frame arrived from the host.
    Frame(HostToClipboard),
    /// The platform clipboard changed; call `sample`.
    Changed,
}

/// A platform clipboard backend driven by [`run_event_loop`].
pub trait PollingReader {
    /// Sampling interval used **only** when no change notifications are
    /// available, i.e. [`attach_wake`](Self::attach_wake) returned `false`
    /// (X11: the protocol has no change events). Event-driven backends
    /// (Wayland data-control) never poll and may ignore this entirely.
    fn poll_interval_ms(&self) -> u64 {
        500
    }

    /// Fail-fast startup check (compositor present, tool installed…).
    /// Tolerated conditions are reported by the implementation itself via
    /// `Error` frames; returning `Err` kills the module.
    fn probe(&mut self) -> Result<()>;

    /// Current clipboard content, if any is readable.
    fn sample(&self) -> Result<Option<Content>>;

    /// Take ownership of the clipboard so future reads return `content`.
    /// Implementations decide whether to serve inline or detached.
    fn claim_async(&self, content: &Content);

    /// Attach a change-notification source: implementations capable of
    /// detecting clipboard changes spawn their watcher and send
    /// [`Wake::Changed`] on `tx`, then return `true`. Poll-only backends
    /// return `false` unchanged.
    ///
    /// The default is polling: sleep through [`Self::poll_interval_ms`] and
    /// let the loop sample on timeout.
    fn attach_wake(&self, _tx: mpsc::Sender<Wake>) -> bool {
        false
    }
}

/// Run the reader protocol over stdio until `Stop` or EOF.
///
/// Sample errors are reported as `Error` frames but silenced after three
/// consecutive occurrences so a dying session never spams the host.
/// Event-driven backends sleep between real changes; poll-only ones wake
/// every `poll_interval_ms`.
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

    // Event-driven backends register a watcher; poll-only ones drive the
    // loop with timeouts instead.
    let event_driven = reader.attach_wake(tx);
    let poll = (!event_driven).then(|| Duration::from_millis(reader.poll_interval_ms()));

    let mut last_hash = String::new();
    let mut consecutive_failures: u32 = 0;

    loop {
        use std::sync::mpsc::RecvTimeoutError;
        enum Tick {
            Wake(Wake),
            Timeout,
        }
        // Event-driven mode blocks until something happens (with a slow
        // safety-net timeout); polling mode wakes every interval.
        let tick = if event_driven {
            match rx.recv_timeout(EVENT_SAMPLE_FLOOR) {
                Ok(wake) => Tick::Wake(wake),
                Err(RecvTimeoutError::Timeout) => Tick::Timeout,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        } else {
            match rx.recv_timeout(poll.expect("polling mode has an interval")) {
                Ok(wake) => Tick::Wake(wake),
                Err(RecvTimeoutError::Timeout) => Tick::Timeout,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        };
        match tick {
            Tick::Wake(Wake::Frame(HostToClipboard::Ping)) => emit(&ClipboardToHost::Pong)?,
            Tick::Wake(Wake::Frame(HostToClipboard::SetClipboard { content })) => {
                reader.claim_async(&content)
            }
            Tick::Wake(Wake::Frame(HostToClipboard::Stop)) => return Ok(()),
            // Real change (event mode) or scheduled sample (polling mode /
            // safety net). Dedup makes redundant samples cheap.
            Tick::Wake(Wake::Changed) | Tick::Timeout => {
                sample(reader, &mut last_hash, &mut consecutive_failures)?
            }
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
