//! Clipboard and paster module lifecycle: spawn, supervise, restart.

use super::{AppEvent, Shared};
use crate::constants as c;
use crate::plugins::ClipboardHandle;
use cliphistory_proto::ClipboardToHost;
use std::process::Child;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::channel;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Spawn the clipboard module and wire its stdout into the event loop.
pub fn start_clipboard(shared: &Shared) {
    let id = shared.clipboard_id.read().unwrap().clone();
    if id.is_empty() {
        return;
    }
    let Some(module) = shared.mm.resolve(&id) else {
        log::error!("clipboard module '{id}' vanished; disabling until restart");
        *shared.clipboard_id.write().unwrap() = String::new();
        return;
    };

    let mut attempts = 0;
    loop {
        match ClipboardHandle::spawn(&module) {
            Ok(handle) => {
                let mut child = handle.child;
                let tx = handle.tx.clone();
                *shared.clipboard_tx.write().unwrap() = Some(tx);
                log::info!("clipboard module '{id}' started");

                forward_output(&mut child, &shared.app_tx);
                supervise(shared.app_tx.clone(), child, id.clone());
                return;
            }
            Err(e) => {
                attempts += 1;
                log::error!(
                    "spawning clipboard module '{id}' failed ({attempts}/{}): {e:#}",
                    c::CLIPBOARD_MAX_SPAWN_ATTEMPTS
                );
                if attempts >= c::CLIPBOARD_MAX_SPAWN_ATTEMPTS {
                    log::error!("giving up on '{id}'; run `cliphistory doctor`");
                    *shared.clipboard_id.write().unwrap() = String::new();
                    *shared.clipboard_tx.write().unwrap() = None;
                    return;
                }
                spin(Duration::from_millis(c::CLIPBOARD_RESPAWN_BACKOFF_MS));
            }
        }
    }
}

fn supervise(app_tx: Sender<AppEvent>, mut child: Child, id: String) {
    std::thread::Builder::new()
        .name("module-supervisor".into())
        .spawn(move || {
            let status = child.wait();
            log::warn!("clipboard module exited: {status:?}");
            let _ = app_tx.send(AppEvent::ClipboardExited(id));
        })
        .ok();
}

/// Forward clipboard stdout frames into the main event loop.
fn forward_output(child: &mut Child, app_tx: &Sender<AppEvent>) {
    let (tx, rx) = channel::<ClipboardToHost>();
    if let Err(e) = ClipboardHandle::pump_output(child, tx) {
        log::error!("pump failed: {e:#}");
        return;
    }
    let app_tx = app_tx.clone();
    std::thread::Builder::new()
        .name("clipboard-events".into())
        .spawn(move || {
            for frame in rx {
                if app_tx.send(AppEvent::FromClipboard(frame)).is_err() {
                    break;
                }
            }
        })
        .ok();
}

pub(crate) fn schedule_restart(shared: &Shared, id: &str) {
    *shared.clipboard_tx.write().unwrap() = None;

    static RESTARTS: AtomicU32 = AtomicU32::new(0);
    let n = RESTARTS.fetch_add(1, Ordering::Relaxed) + 1;
    if n > c::CLIPBOARD_MAX_RUNTIME_RESTARTS {
        log::error!("clipboard module '{id}' died {n} times; disabling.");
        *shared.clipboard_id.write().unwrap() = String::new();
        return;
    }
    log::info!(
        "restarting clipboard module '{id}' in {}ms",
        c::CLIPBOARD_RESPAWN_BACKOFF_MS
    );
    let tx = shared.app_tx.clone();
    std::thread::Builder::new()
        .name("restart-timer".into())
        .spawn(move || {
            spin(Duration::from_millis(c::CLIPBOARD_RESPAWN_BACKOFF_MS));
            let _ = tx.send(AppEvent::RestartClipboard);
        })
        .ok();
}

fn spin(d: Duration) {
    std::thread::sleep(d);
}
