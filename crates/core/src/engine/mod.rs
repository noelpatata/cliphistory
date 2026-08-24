//! The cliphistory daemon: clipboard history engine.
//!
//! Split by responsibility:
//! * [`bootstrap`]  – resolve/install the best modules for this machine
//! * [`supervisor`] – clipboard module lifecycle (spawn/respawn)
//! * [`dispatch`]   – IPC routing; [`actions`] business logic; [`report`] doctor
//! * [`state`]      – the `Shared` engine state and app events

pub(crate) mod bootstrap;
pub(crate) mod actions;
pub(crate) mod dispatch;
pub(crate) mod report;
pub(crate) mod show_view;
pub(crate) mod state;
pub(crate) mod supervisor;

use crate::config::{self, Config};
use crate::discovery::{detect_session, RealEnv};
use crate::plugins::ModuleManager;
use crate::storage::{unix_now, Storage};
use anyhow::{bail, Context, Result};
use cliphistory_proto::{ClipboardToHost, HostToClipboard, HostToPaster, PasterToHost};

use state::{AppEvent, Shared};

use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::channel;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(cfg: Config) -> Result<()> {
    let socket_path = config::socket_path();

    // Refuse to start twice.
    if UnixStream::connect(&socket_path).is_ok() {
        bail!(
            "cliphistory daemon already running at {}",
            socket_path.display()
        );
    }

    let runtime_dir = config::runtime_dir();
    std::fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("creating {}", runtime_dir.display()))?;
    // Permissions: only the current user may talk to the daemon.
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700));
    }
    let _ = std::fs::remove_file(&socket_path); // stale socket from a crash

    run_inner(cfg, socket_path)
}

fn run_inner(cfg: Config, socket_path: std::path::PathBuf) -> Result<()> {
    let db_path = cfg.storage.resolved_db_path();
    let storage = Arc::new(Storage::open(&db_path)?);
    storage.prune_with_budget(
        cfg.storage.max_entries,
        cfg.storage.max_age_days,
        Some(cfg.storage.max_total_bytes),
    )?;

    let mm = Arc::new(ModuleManager::new(cfg.modules.clone()));
    let (app_tx, app_rx) = channel();

    let mut shared = Shared {
        storage,
        mm,
        clipboard_tx: Arc::new(RwLock::new(None)),
        clipboard_id: Arc::new(RwLock::new(String::new())),
        paster_tx: Arc::new(RwLock::new(None)),
        paster_id: Arc::new(RwLock::new(String::new())),
        frontend_id: Arc::new(RwLock::new(String::new())),
        started_at: unix_now(),
        session: detect_session(&RealEnv),
        app_tx: app_tx.clone(),
        cfg: cfg.clone(),
    };

    // ----- module selection / installation ---------------------------------
    let desired = bootstrap::resolve_desired(&mut shared)?;
    *shared.clipboard_id.write().unwrap() = desired.clipboard.unwrap_or_default();
    *shared.frontend_id.write().unwrap() = desired.frontend.unwrap_or_default();
    *shared.paster_id.write().unwrap() = desired.paster.unwrap_or_default();

    if shared.clipboard_id.read().unwrap().is_empty() {
        log::warn!("no usable clipboard module; run `cliphistory doctor`");
    }
    if shared.frontend_id.read().unwrap().is_empty() {
        log::warn!("no usable frontend module; run `cliphistory doctor`");
    }

    // ----- listener ---------------------------------------------------------
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding {}", socket_path.display()))?;
    log::info!("listening on {}", socket_path.display());

    {
        let app_tx = app_tx.clone();
        std::thread::Builder::new()
            .name("accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(s) => {
                            if app_tx.send(AppEvent::Conn(s)).is_err() {
                                break;
                            }
                        }
                        Err(e) => log::warn!("accept failed: {e}"),
                    }
                }
            })?;
    }

    // ----- clipboard / paster modules ---------------------------------------
    if !shared.clipboard_id.read().unwrap().is_empty() {
        supervisor::start_clipboard(&shared);
    }
    if !shared.paster_id.read().unwrap().is_empty() {
        supervisor::start_paster(&shared);
    }

    // ----- main loop ----------------------------------------------------------
    while let Ok(event) = app_rx.recv() {
        match event {
            AppEvent::FromClipboard(ClipboardToHost::Event { content }) => {
                handle_clipboard_event(&shared, content);
            }
            AppEvent::FromClipboard(frame) => {
                log::debug!("clipboard module frame: {frame:?}");
            }
            AppEvent::FromPaster(PasterToHost::Ready { protocol_version }) => {
                log::info!("paster module ready (protocol v{protocol_version})");
            }
            AppEvent::FromPaster(PasterToHost::Pong) => {}
            AppEvent::FromPaster(PasterToHost::Error { message }) => {
                log::warn!("paster: {message}");
            }
            AppEvent::Conn(stream) => dispatch::handle_conn(&shared, stream),
            AppEvent::ClipboardExited(id) => supervisor::schedule_restart(&shared, &id),
            AppEvent::RestartClipboard => supervisor::start_clipboard(&shared),
            AppEvent::PasterExited(id) => supervisor::schedule_paster_restart(&shared, &id),
            AppEvent::RestartPaster => supervisor::start_paster(&shared),
            AppEvent::Shutdown => break,
        }
    }

    shutdown(&shared, &socket_path);
    Ok(())
}

fn handle_clipboard_event(shared: &Shared, content: cliphistory_proto::Content) {
    match shared.storage.insert(
        &content,
        crate::storage::InsertOpts {
            max_item_size: shared.cfg.storage.max_item_size,
            thumbnail_size: shared.cfg.storage.thumbnail_size,
        },
    ) {
        Ok(crate::storage::InsertOutcome::Inserted(id)) => {
            log::info!("[{id}] stored {}", content.preview());
            let _ = shared.storage.prune_with_budget(
                shared.cfg.storage.max_entries,
                shared.cfg.storage.max_age_days,
                Some(shared.cfg.storage.max_total_bytes),
            );
        }
        Ok(crate::storage::InsertOutcome::Duplicate(id)) => {
            log::debug!("[{id}] promoted duplicate {}", content.preview());
        }
        Ok(crate::storage::InsertOutcome::TooLarge { size, limit }) => {
            log::info!("skipped {}B payload (limit {limit}B)", size);
        }
        Err(e) => log::error!("insert failed: {e:#}"),
    }
}

fn shutdown(shared: &Shared, socket_path: &std::path::Path) {
    log::info!("shutting down");
    if let Some(tx) = shared.clipboard_tx.read().unwrap().as_ref() {
        tx.send(HostToClipboard::Stop).ok();
    }
    if let Some(tx) = shared.paster_tx.read().unwrap().as_ref() {
        tx.send(HostToPaster::Stop).ok();
    }
    let _ = std::fs::remove_file(socket_path);
}
