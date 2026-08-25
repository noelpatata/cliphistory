//! Background history synchronisation for the picker.
//!
//! Preferred transport: a `WatchHistory` subscription — one connection,
//! daemon pushes a snapshot on every mutation, zero traffic while idle.
//! If the subscription cannot be established (daemon predates
//! `WatchHistory`, or it keeps dropping), fall back to timed polling.

use super::model::fingerprint;
use cliphistory_proto::HistoryItem;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// How often the open picker asks the daemon for fresh history when it
/// cannot subscribe (older daemons without `WatchHistory`).
const HISTORY_POLL_INTERVAL: Duration = Duration::from_millis(400);
/// How many entries a refresh fetches (mirrors the daemon's own show cap).
const HISTORY_POLL_LIMIT: usize = 100;
/// Consecutive failed subscription attempts before giving up on push and
/// falling back to timed polling for the rest of the session.
const SUBSCRIBE_MAX_FAILURES: u32 = 2;

/// Keep the picker's list in sync until the process ends. Snapshots are
/// fingerprinted so idle periods cost nothing and only real changes are
/// forwarded to the UI channel.
pub(crate) fn sync_history(socket: std::path::PathBuf, tx: Sender<Vec<HistoryItem>>) {
    let mut failures: u32 = 0;
    loop {
        let mut subscribed = false;
        if let Ok(mut stream) = crate::ipc::subscribe_history(&socket) {
            subscribed = true;
            failures = 0;
            let mut last = None::<u64>;
            let alive = loop {
                match stream.next() {
                    Some(Ok(items)) => {
                        let fp = fingerprint(&items);
                        if last != Some(fp) && tx.send(items).is_err() {
                            break false; // UI gone; window closed.
                        }
                        last = Some(fp);
                    }
                    Some(Err(_)) => break true, // lost mid-stream: retry.
                    None => break true,         // daemon closed the stream.
                }
            };
            if !alive {
                return;
            }
        }

        if !subscribed || !supports_watch(&socket) {
            // Daemon without WatchHistory (or socket hiccup): poll instead.
            if !subscribed && failures + 1 >= SUBSCRIBE_MAX_FAILURES {
                log_fallback();
                poll_loop(&socket, tx);
                return;
            }
        }

        failures += 1;
        std::thread::sleep(subscribe_backoff(failures));
    }
}

/// True when the daemon answers `GetHistory` (i.e. it is reachable at
/// all); used to distinguish "old daemon" from "socket gone".
fn supports_watch(socket: &std::path::Path) -> bool {
    crate::ipc::history(socket, 1).is_ok()
}

/// One-line notice that we downgraded to polling for this session.
fn log_fallback() {
    eprintln!("cliphistory: daemon does not support history push; polling instead");
}

/// Backoff between subscription retries: 0.5s, 1s, 2s … capped at 8s.
fn subscribe_backoff(failures: u32) -> Duration {
    Duration::from_millis((250 * (2 << failures.min(5))).min(8000))
}

/// Timed-polling fallback for daemons without `WatchHistory`.
fn poll_loop(socket: &std::path::Path, tx: Sender<Vec<HistoryItem>>) {
    let mut last = None::<u64>;
    loop {
        if let Ok(items) = crate::ipc::history(socket, HISTORY_POLL_LIMIT) {
            let fp = fingerprint(&items);
            if last != Some(fp) && tx.send(items).is_err() {
                return; // UI gone; window closed.
            }
            last = Some(fp);
        }
        std::thread::sleep(HISTORY_POLL_INTERVAL);
    }
}
