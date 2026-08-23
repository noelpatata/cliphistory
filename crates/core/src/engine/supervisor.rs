//! Clipboard and paster module lifecycle: spawn, supervise, restart.

use super::{AppEvent, Shared};
use crate::constants as c;
use crate::plugins::ClipboardHandle;
use cliphistory_proto::{ClipboardToHost, HostToPaster, PasterToHost};
use std::io::Write as _;
use std::process::{Child, Command, Stdio};
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

// ---------------------------------------------------------------------------
// Paster (lazy: spawned on first paste, respawned on demand)
// ---------------------------------------------------------------------------

/// Make sure a paster process is alive, then queue a Paste frame.
pub fn request_paste(shared: &Shared) {
    if !shared.cfg.general.auto_paste {
        return;
    }
    // Expert override wins.
    if shared
        .cfg
        .general
        .paste_command
        .as_deref()
        .is_some_and(|c| !c.trim().is_empty())
    {
        crate::paste::schedule_command(
            shared.cfg.general.paste_command.clone().unwrap(),
            Duration::from_millis(shared.cfg.general.paste_delay_ms),
        );
        return;
    }

    let id = shared.paster_id.read().unwrap().clone();
    if id.is_empty() {
        return; // nothing discovered on this platform
    }

    // Lazy spawn / respawn after a crash.
    let needs_spawn = {
        let guard = shared.paster_tx.read().unwrap();
        guard.is_none()
    };
    if needs_spawn {
        spawn_paster(shared, &id);
    }

    let guard = shared.paster_tx.read().unwrap();
    match guard.as_ref() {
        Some(tx) => {
            if tx.send(HostToPaster::Paste).is_err() {
                log::warn!("paster stdin closed; paste skipped this round");
                *shared.paster_tx.write().unwrap() = None;
            }
        }
        None => log::warn!("paster unavailable; paste skipped"),
    }
}

fn spawn_paster(shared: &Shared, id: &str) {
    let Some(module) = shared.mm.resolve(id) else {
        log::warn!("paster module '{id}' not found");
        *shared.paster_id.write().unwrap() = String::new();
        return;
    };
    let mut child = match Command::new(&module.bin_path)
        .arg("run")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::warn!("spawning paster '{id}' failed: {e:#}");
            return;
        }
    };

    let raw_stdin = child.stdin.take().expect("paster stdin");
    let (tx, rx) = channel::<HostToPaster>();
    std::thread::Builder::new()
        .name("paster-stdin".into())
        .spawn(move || {
            let mut w = std::io::LineWriter::new(raw_stdin);
            for frame in rx {
                if serde_json::to_writer(&mut w, &frame).is_err() {
                    break;
                }
                if w.write_all(b"\n").is_err() {
                    break;
                }
            }
        })
        .ok();
    *shared.paster_tx.write().unwrap() = Some(tx);

    // stdout pump
    if let Some(stdout) = child.stdout.take() {
        let app_tx = shared.app_tx.clone();
        std::thread::Builder::new()
            .name("paster-events".into())
            .spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    match serde_json::from_str::<PasterToHost>(line.trim()) {
                        Ok(f) => {
                            if app_tx.send(AppEvent::FromPaster(f)).is_err() {
                                break;
                            }
                        }
                        Err(_) => continue,
                    }
                }
            })
            .ok();
    }

    let sup_tx = shared.app_tx.clone();
    std::thread::Builder::new()
        .name("paster-supervisor".into())
        .spawn(move || {
            let status = child.wait();
            log::warn!("paster exited: {status:?}");
            let _ = sup_tx.send(AppEvent::PasterDied);
        })
        .ok();

    log::info!("paster module '{id}' started");
}

fn spin(d: Duration) {
    std::thread::sleep(d);
}
