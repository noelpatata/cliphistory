//! Background history synchronisation for the picker.
//!
//! Uses a `WatchHistory` push subscription: one persistent connection,
//! daemon sends a snapshot on every mutation, zero traffic while idle.
//! Reconnects with exponential backoff when the connection drops.

use super::model::fingerprint;
use cliphistory_proto::HistoryItem;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Backoff between reconnection attempts: 0.5s, 1s, 2s … capped at 8s.
fn reconnect_backoff(failures: u32) -> Duration {
    Duration::from_millis((250 * (2 << failures.min(5))).min(8000))
}

/// Subscribe to the daemon's history and forward snapshots to the UI
/// until the process ends. Snapshots are fingerprinted so idle periods
/// cost nothing; only real changes reach the UI channel.
pub(crate) fn sync_history(socket: std::path::PathBuf, tx: Sender<Vec<HistoryItem>>) {
    let mut failures: u32 = 0;
    loop {
        match crate::ipc::subscribe_history(&socket) {
            Ok(mut stream) => {
                failures = 0;
                let mut last = None::<u64>;
                for msg in stream.by_ref() {
                    match msg {
                        Ok(items) => {
                            let fp = fingerprint(&items);
                            if last != Some(fp) && tx.send(items).is_err() {
                                return; // UI gone; window closed.
                            }
                            last = Some(fp);
                        }
                        Err(_) => break, // lost mid-stream: reconnect.
                    }
                }
            }
            Err(_) => {
                failures += 1;
            }
        }
        std::thread::sleep(reconnect_backoff(failures));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_backoff_grows_and_caps() {
        assert_eq!(reconnect_backoff(0), Duration::from_millis(500));
        assert_eq!(reconnect_backoff(1), Duration::from_millis(1000));
        assert_eq!(reconnect_backoff(2), Duration::from_millis(2000));
        // Caps at 8s no matter how many failures pile up.
        assert_eq!(reconnect_backoff(9), Duration::from_millis(8000));
    }
}
