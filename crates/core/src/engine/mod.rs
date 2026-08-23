//! The cliphistory daemon: clipboard history engine.
//!
//! Split by responsibility:
//! * [`bootstrap`]  – resolve/install the best modules for this machine
//! * [`supervisor`] – clipboard module lifecycle (spawn/respawn)
//! * [`handlers`]   – IPC request processing (show/copy/status/doctor)

pub(crate) mod bootstrap;
pub(crate) mod handlers;
pub(crate) mod supervisor;

use crate::config::{self, Config};
use crate::discovery;
use crate::discovery::{detect_session, RealEnv};

use crate::plugins::ModuleManager;
use crate::storage::{unix_now, Storage};
use anyhow::{bail, Context, Result};
use cliphistory_proto::{ClipboardToHost, HostToClipboard};

use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub(crate) struct Shared {
    cfg: Config,
    storage: Arc<Storage>,
    mm: Arc<ModuleManager>,
    clipboard_tx: Arc<RwLock<Option<Sender<HostToClipboard>>>>,
    clipboard_id: Arc<RwLock<String>>,
    frontend_id: Arc<RwLock<String>>,
    started_at: u64,
    session: discovery::SessionType,
    app_tx: Sender<AppEvent>,
}

pub(crate) enum AppEvent {
    FromClipboard(ClipboardToHost),
    Conn(UnixStream),
    ClipboardExited(String),
    RestartClipboard,
    Shutdown,
}

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
    storage.prune(cfg.storage.max_entries, cfg.storage.max_age_days)?;

    let mm = Arc::new(ModuleManager::new(cfg.modules.clone()));
    let (app_tx, app_rx) = channel();

    let mut shared = Shared {
        storage,
        mm,
        clipboard_tx: Arc::new(RwLock::new(None)),
        clipboard_id: Arc::new(RwLock::new(String::new())),
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

    // ----- clipboard module --------------------------------------------------
    if !shared.clipboard_id.read().unwrap().is_empty() {
        supervisor::start_clipboard(&shared);
    }
    // Auto-paste is handled by crate::paste (external tool injection).

    // ----- main loop ----------------------------------------------------------
    while let Ok(event) = app_rx.recv() {
        match event {
            AppEvent::FromClipboard(ClipboardToHost::Event { content }) => {
                handle_clipboard_event(&shared, content);
            }
            AppEvent::FromClipboard(frame) => {
                log::debug!("clipboard module frame: {frame:?}");
            }
            AppEvent::Conn(stream) => handlers::handle_conn(&shared, stream),
            AppEvent::ClipboardExited(id) => supervisor::schedule_restart(&shared, &id),
            AppEvent::RestartClipboard => supervisor::start_clipboard(&shared),
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
            let _ = shared.storage.prune(
                shared.cfg.storage.max_entries,
                shared.cfg.storage.max_age_days,
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
    let _ = std::fs::remove_file(socket_path);
}
