//! Module lifecycle: spawn, supervise, restart.
//!
//! Clipboard and paster modules follow the identical lifecycle
//! (resolve → spawn with retries → supervise → restart with backoff), so
//! the mechanics live once in [`Slot`]-generic functions. Adding a future
//! module kind means implementing [`Slot`], not copying control flow.

use super::{AppEvent, Shared};
use crate::constants as c;
use crate::plugins::process::{ClipboardFrames, FrameSpec, ModuleHandle, PasterFrames};
use cliphistory_proto::{ClipboardToHost, HostToClipboard, HostToPaster, PasterToHost};
use std::process::Child;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Sender slot for a module kind's stdin channel.
pub(crate) type TxSlot<F> = Arc<RwLock<Option<Sender<<F as FrameSpec>::ToModule>>>>;

/// Ties one module kind to its slots in [`Shared`] and its lifecycle events.
pub(crate) trait Slot: 'static {
    type Frames: FrameSpec;
    const NAME: &'static str;

    fn tx_slot(s: &Shared) -> &TxSlot<Self::Frames>;
    fn id_slot(s: &Shared) -> &Arc<RwLock<String>>;
    /// AppEvent emitted when the child exits.
    fn exited(id: String) -> AppEvent;
    /// AppEvent that re-runs `start`.
    fn restart() -> AppEvent;
    /// Map a stdout frame onto the app event loop. `None` = fully handled
    /// here (logged/consumed), nothing forwarded.
    fn frame_event(frame: <Self::Frames as FrameSpec>::FromModule) -> Option<AppEvent>;
    /// Per-kind restart counter so crash loops are tracked independently.
    fn restart_counter() -> &'static AtomicU32;
}

pub(crate) struct ClipboardSlot;

impl Slot for ClipboardSlot {
    type Frames = ClipboardFrames;
    const NAME: &'static str = "clipboard";

    fn tx_slot(s: &Shared) -> &Arc<RwLock<Option<Sender<HostToClipboard>>>> {
        &s.clipboard_tx
    }
    fn id_slot(s: &Shared) -> &Arc<RwLock<String>> {
        &s.clipboard_id
    }
    fn exited(id: String) -> AppEvent {
        AppEvent::ClipboardExited(id)
    }
    fn restart() -> AppEvent {
        AppEvent::RestartClipboard
    }
    fn frame_event(frame: ClipboardToHost) -> Option<AppEvent> {
        Some(AppEvent::FromClipboard(frame))
    }
    fn restart_counter() -> &'static AtomicU32 {
        static C: AtomicU32 = AtomicU32::new(0);
        &C
    }
}

pub(crate) struct PasterSlot;

impl Slot for PasterSlot {
    type Frames = PasterFrames;
    const NAME: &'static str = "paster";

    fn tx_slot(s: &Shared) -> &Arc<RwLock<Option<Sender<HostToPaster>>>> {
        &s.paster_tx
    }
    fn id_slot(s: &Shared) -> &Arc<RwLock<String>> {
        &s.paster_id
    }
    fn exited(id: String) -> AppEvent {
        AppEvent::PasterExited(id)
    }
    fn restart() -> AppEvent {
        AppEvent::RestartPaster
    }
    fn frame_event(frame: PasterToHost) -> Option<AppEvent> {
        match frame {
            PasterToHost::Ready { protocol_version } => {
                log::info!("paster module ready (protocol v{protocol_version})");
                None // consumed: logged above
            }
            other => Some(AppEvent::FromPaster(other)),
        }
    }
    fn restart_counter() -> &'static AtomicU32 {
        static C: AtomicU32 = AtomicU32::new(0);
        &C
    }
}

/// Spawn the module for slot `S`, retrying up to the configured limit.
pub(crate) fn start_clipboard(shared: &Shared) {
    start_impl::<ClipboardSlot>(shared);
}
pub(crate) fn start_paster(shared: &Shared) {
    start_impl::<PasterSlot>(shared);
}
pub(crate) fn schedule_restart(shared: &Shared, id: &str) {
    schedule_restart_impl::<ClipboardSlot>(shared, id);
}
pub(crate) fn schedule_paster_restart(shared: &Shared, id: &str) {
    schedule_restart_impl::<PasterSlot>(shared, id);
}
fn start_impl<S: Slot>(shared: &Shared) {
    let id = S::id_slot(shared).read().unwrap().clone();
    if id.is_empty() {
        return;
    }
    let Some(module) = shared.mm.resolve(&id) else {
        log::error!("{} module '{id}' vanished; disabling until restart", S::NAME);
        *S::id_slot(shared).write().unwrap() = String::new();
        return;
    };

    let mut attempts = 0;
    loop {
        match ModuleHandle::<S::Frames>::spawn(&module) {
            Ok(handle) => {
                let (mut child, tx) = handle.into_parts();
                *S::tx_slot(shared).write().unwrap() = Some(tx);
                log::info!("{} module '{id}' started", S::NAME);

                forward_output::<S>(&mut child, &shared.app_tx);
                supervise::<S>(shared.app_tx.clone(), child, id.clone());
                return;
            }
            Err(e) => {
                attempts += 1;
                log::error!(
                    "spawning {} module '{id}' failed ({attempts}/{}): {e:#}",
                    S::NAME,
                    c::CLIPBOARD_MAX_SPAWN_ATTEMPTS
                );
                if attempts >= c::CLIPBOARD_MAX_SPAWN_ATTEMPTS {
                    log::error!("giving up on {0} '{1}'; run `cliphistory doctor`", S::NAME, id);
                    *S::id_slot(shared).write().unwrap() = String::new();
                    *S::tx_slot(shared).write().unwrap() = None;
                    return;
                }
                spin(Duration::from_millis(c::CLIPBOARD_RESPAWN_BACKOFF_MS));
            }
        }
    }
}

fn supervise<S: Slot>(app_tx: Sender<AppEvent>, mut child: Child, id: String) {
    std::thread::Builder::new()
        .name(format!("{}-supervisor", S::NAME))
        .spawn(move || {
            let status = child.wait();
            log::warn!("{} module exited: {status:?}", S::NAME);
            let _ = app_tx.send(S::exited(id));
        })
        .ok();
}

/// Forward a module's stdout frames into the main event loop.
fn forward_output<S: Slot>(child: &mut Child, app_tx: &Sender<AppEvent>) {
    let (tx, rx) = channel::<<S::Frames as FrameSpec>::FromModule>();
    if let Err(e) = ModuleHandle::<S::Frames>::pump_output(child, tx) {
        log::error!("{} pump failed: {e:#}", S::NAME);
        return;
    }
    let app_tx = app_tx.clone();
    std::thread::Builder::new()
        .name(format!("{}-events", S::NAME))
        .spawn(move || {
            for frame in rx {
                if let Some(ev) = S::frame_event(frame) {
                    if app_tx.send(ev).is_err() {
                        break;
                    }
                }
            }
        })
        .ok();
}

fn schedule_restart_impl<S: Slot>(shared: &Shared, id: &str) {
    *S::tx_slot(shared).write().unwrap() = None;

    let n = S::restart_counter().fetch_add(1, Ordering::Relaxed) + 1;
    if n > c::CLIPBOARD_MAX_RUNTIME_RESTARTS {
        log::error!("{} module '{id}' died {n} times; disabling.", S::NAME);
        *S::id_slot(shared).write().unwrap() = String::new();
        return;
    }
    log::info!(
        "restarting {} module '{id}' in {}ms",
        S::NAME,
        c::CLIPBOARD_RESPAWN_BACKOFF_MS
    );
    let tx = shared.app_tx.clone();
    std::thread::Builder::new()
        .name(format!("{}-restart-timer", S::NAME))
        .spawn(move || {
            spin(Duration::from_millis(c::CLIPBOARD_RESPAWN_BACKOFF_MS));
            let _ = tx.send(S::restart());
        })
        .ok();
}

fn spin(d: Duration) {
    std::thread::sleep(d);
}

