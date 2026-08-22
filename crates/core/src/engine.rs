//! The cliphistory daemon: clipboard history engine.
//!
//! Responsibilities
//! ----------------
//! * resolve + install the best reader/frontend modules for this system,
//! * supervise the reader (respawn with backoff, give up eventually),
//! * persist every clipboard event (dedup, prune),
//! * serve CLI clients over a Unix socket, including the `show` flow that
//!   renders a frontend menu and pushes the selection back to the clipboard.

use crate::config::{self, Config};
use crate::constants as c;
use crate::discovery::{
    self, detect_session, probe_requirements, probe_tool, InstalledInfo, RealEnv,
};
use crate::ipc::{DaemonStatus, IpcRequest, IpcResponse, ModuleInfo};
use crate::plugins::{InstalledModule, ModuleManager, ReaderHandle};
use crate::storage::{unix_now, Storage};
use anyhow::{bail, Context, Result};
use cliphistory_proto::{HostToReader, ReaderToHost, PROTOCOL_VERSION};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Shared {
    cfg: Config,
    storage: Arc<Storage>,
    mm: Arc<ModuleManager>,
    reader_tx: Arc<RwLock<Option<Sender<HostToReader>>>>,
    reader_id: Arc<RwLock<String>>,
    frontend_id: Arc<RwLock<String>>,
    started_at: u64,
    session: discovery::SessionType,
    app_tx: Sender<AppEvent>,
}

enum AppEvent {
    FromReader(ReaderToHost),
    Conn(UnixStream),
    ReaderExited(String),
    RestartReader,
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
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::remove_file(&socket_path); // stale socket from a crash

    run_inner(cfg, socket_path)
}

fn run_inner(cfg: Config, socket_path: std::path::PathBuf) -> Result<()> {
    let db_path = cfg.storage.resolved_db_path();
    let storage = Arc::new(Storage::open(&db_path)?);
    let mm = Arc::new(ModuleManager::new(cfg.modules.clone()));

    let (app_tx, app_rx): (Sender<AppEvent>, Receiver<AppEvent>) = channel();

    let mut shared = Shared {
        storage: storage.clone(),
        mm: mm.clone(),
        reader_tx: Arc::new(RwLock::new(None)),
        reader_id: Arc::new(RwLock::new(String::new())),
        frontend_id: Arc::new(RwLock::new(String::new())),
        started_at: unix_now(),
        session: detect_session(&RealEnv),
        app_tx: app_tx.clone(),
        cfg: cfg.clone(),
    };

    // ----- module resolution ----------------------------------------------
    select_modules(&mut shared)?;

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

    // ----- reader -----------------------------------------------------------
    if !shared.reader_id.read().unwrap().is_empty() {
        start_reader(&shared);
    }

    // ----- main loop ----------------------------------------------------------
    while let Ok(event) = app_rx.recv() {
        match event {
            AppEvent::FromReader(ReaderToHost::Event { content }) => {
                match shared
                    .storage
                    .insert(&content, shared.cfg.storage.max_item_size)
                {
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
            AppEvent::FromReader(frame) => {
                log::debug!("reader frame: {frame:?}");
            }
            AppEvent::Conn(stream) => {
                handle_conn(&shared, stream);
            }
            AppEvent::ReaderExited(id) => schedule_restart(&shared, &id),
            AppEvent::RestartReader => start_reader(&shared),
            AppEvent::Shutdown => break,
        }
    }

    shutdown(&shared, &socket_path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Module selection / installation
// ---------------------------------------------------------------------------

/// For a machine with no modules installed yet: fetch the remote release
/// manifest, synthesise candidate infos from its metadata (requirements
/// probed against this system) and rank them exactly like installed ones
/// would be. Returns `(reader, frontend)` picks; `None` when unreachable.
fn remote_candidates(shared: &Shared) -> Option<(Option<String>, Option<String>)> {
    let rm = shared.mm.fetch_remote_manifest(None).ok()?;
    let assets = shared.mm.select_target(&rm).ok()?;

    let pseudo: Vec<discovery::InstalledInfo> = assets
        .modules
        .iter()
        .map(|m| discovery::InstalledInfo {
            manifest: cliphistory_proto::ModuleManifest {
                id: m.id.clone(),
                kind: m.kind,
                version: rm.release.clone(),
                protocol_version: PROTOCOL_VERSION,
                capabilities: m.capabilities.clone(),
                requires: m.requires.clone(),
                description: m.description.clone(),
            },
            requirements_met: m.requires.iter().all(|t| probe_tool(t)),
        })
        .collect();

    let reader = discovery::rank_candidates(
        shared.session.reader_candidates(),
        &pseudo,
        shared.cfg.discovery.preferred_reader.as_deref(),
        cliphistory_proto::ModuleKind::Reader,
    )
    .into_iter()
    .next();

    let frontend = discovery::rank_candidates(
        c::FRONTEND_CANDIDATES,
        &pseudo,
        shared.cfg.discovery.preferred_frontend.as_deref(),
        cliphistory_proto::ModuleKind::Frontend,
    )
    .into_iter()
    .next();

    Some((reader, frontend))
}

fn select_modules(shared: &mut Shared) -> Result<()> {
    let installed = shared.mm.list_installed()?;
    let infos = to_installed_infos(&installed);

    let report = discovery::discover(&shared.cfg, &infos)?;
    log::info!("discovery:\n{report}");

    let pick = |ranked: &[String], preferred: Option<&str>| -> Option<String> {
        if let Some(p) = preferred {
            return Some(p.to_string());
        }
        ranked.first().cloned()
    };

    let reader_pick = pick(
        &report.readers,
        shared.cfg.discovery.preferred_reader.as_deref(),
    );
    let frontend_pick = pick(
        &report.frontends,
        shared.cfg.discovery.preferred_frontend.as_deref(),
    );

    // Fresh machine: nothing installed, so discovery ranked nothing. Decide
    // what to download using the release manifest's own `requires` metadata
    // (core stays decoupled from module internals) and fetch only the winners.
    let mut wanted: Vec<String> = Vec::new();
    if reader_pick.is_none() && frontend_pick.is_none() {
        match remote_candidates(shared) {
            Some((reader, frontend)) => {
                wanted.extend([reader, frontend].into_iter().flatten());
            }
            None => {
                // Offline or unreachable source: try every known candidate so
                // a later online run / manual install can still succeed.
                log::warn!("cannot reach release manifest; queueing all candidates");
                wanted.extend(
                    shared
                        .session
                        .reader_candidates()
                        .iter()
                        .map(|s| s.to_string()),
                );
                wanted.extend(c::FRONTEND_CANDIDATES.iter().map(|s| s.to_string()));
            }
        }
    } else {
        wanted.extend([reader_pick, frontend_pick].into_iter().flatten());
    }

    // Download whatever is missing (skipped in local-dir dev mode).
    let mut missing: Vec<String> = Vec::new();
    for id in wanted {
        if shared.mm.resolve(&id).is_none() && !missing.iter().any(|m| m == &id) {
            missing.push(id);
        }
    }
    if !missing.is_empty() && !shared.mm.install_root().exists() {
        let root = shared.mm.install_root();
        let _ = std::fs::create_dir_all(root);
    }
    if !missing.is_empty() {
        log::info!("installing missing modules: {:?}", missing);
        for res in shared
            .mm
            .ensure_available(&missing, false, &|msg| log::info!("{msg}"))
        {
            if let Err(e) = res {
                log::warn!("install failed: {e:#}");
            }
        }
    }

    // Validate picks against freshly installed state; requirements must hold
    // unless running non-strict (then we degrade gracefully).
    let installed = shared.mm.list_installed()?;
    let infos = to_installed_infos(&installed);

    let choose_reader = || -> Option<String> {
        let cands = shared.session.reader_candidates();
        let mut ranked = discovery::rank_candidates(
            cands,
            &infos,
            shared.cfg.discovery.preferred_reader.as_deref(),
            cliphistory_proto::ModuleKind::Reader,
        );
        if ranked.is_empty() {
            // Fall back to any installed reader-capable module.
            ranked = installed
                .iter()
                .filter(|m| m.manifest.kind == cliphistory_proto::ModuleKind::Reader)
                .map(|m| m.manifest.id.clone())
                .collect();
        }
        strict_filter(ranked, &infos, shared.cfg.discovery.strict)
    };

    let choose_frontend = || -> Option<String> {
        let ranked = discovery::rank_candidates(
            c::FRONTEND_CANDIDATES,
            &infos,
            shared.cfg.discovery.preferred_frontend.as_deref(),
            cliphistory_proto::ModuleKind::Frontend,
        );
        strict_filter(ranked, &infos, shared.cfg.discovery.strict)
    };

    let reader = choose_reader();
    let frontend = choose_frontend();

    *shared.reader_id.write().unwrap() = reader.unwrap_or_default();
    *shared.frontend_id.write().unwrap() = frontend.unwrap_or_default();

    if shared.reader_id.read().unwrap().is_empty() {
        log::warn!("no usable reader module; run `cliphistory doctor`");
    }
    if shared.frontend_id.read().unwrap().is_empty() {
        log::warn!("no usable frontend module; run `cliphistory doctor`");
    }
    Ok(())
}

fn strict_filter(ranked: Vec<String>, infos: &[InstalledInfo], strict: bool) -> Option<String> {
    for id in ranked {
        let info = infos.iter().find(|i| i.manifest.id == id);
        match info {
            Some(i) if i.requirements_met || !strict => return Some(id),
            Some(_) if strict => continue,
            None => continue,
            _ => {}
        }
    }
    None
}

pub(crate) fn to_installed_infos(installed: &[InstalledModule]) -> Vec<InstalledInfo> {
    installed
        .iter()
        .map(|m| {
            let (met, _) = probe_requirements(&m.manifest.requires);
            InstalledInfo {
                manifest: m.manifest.clone(),
                requirements_met: met,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Reader lifecycle
// ---------------------------------------------------------------------------

fn start_reader(shared: &Shared) {
    let id = shared.reader_id.read().unwrap().clone();
    if id.is_empty() {
        return;
    }
    let module = match shared.mm.resolve(&id) {
        Some(m) => m,
        None => {
            log::error!("reader '{id}' vanished; disabling until restart");
            *shared.reader_id.write().unwrap() = String::new();
            return;
        }
    };

    let mut attempts = 0u32;
    loop {
        match ReaderHandle::spawn(&module) {
            Ok(handle) => {
                let mut child = handle.child;
                let tx = handle.tx;
                *shared.reader_tx.write().unwrap() = Some(tx.clone());
                log::info!("reader '{id}' started");

                let (out_tx, out_rx) = channel::<ReaderToHost>();
                let app_tx = shared.app_tx.clone();
                if let Err(e) = ReaderHandle::pump_output(&mut child, out_tx) {
                    log::error!("pump failed: {e:#}");
                }
                std::thread::Builder::new()
                    .name("reader-events".into())
                    .spawn(move || {
                        for frame in out_rx {
                            if app_tx.send(AppEvent::FromReader(frame)).is_err() {
                                break;
                            }
                        }
                    })
                    .ok();

                // Supervisor: wait for exit and ask the main loop to restart.
                let sup_tx = shared.app_tx.clone();
                std::thread::Builder::new()
                    .name("reader-supervisor".into())
                    .spawn(move || {
                        let status = child.wait();
                        log::warn!("reader exited: {:?}", status);
                        let _ = sup_tx.send(AppEvent::ReaderExited(id));
                    })
                    .ok();
                return;
            }
            Err(e) => {
                attempts += 1;
                log::error!(
                    "spawning reader '{id}' failed ({attempts}/{}): {e:#}",
                    c::READER_MAX_SPAWN_ATTEMPTS
                );
                if attempts >= c::READER_MAX_SPAWN_ATTEMPTS {
                    log::error!("giving up on reader '{id}'; run `cliphistory doctor`");
                    *shared.reader_id.write().unwrap() = String::new();
                    *shared.reader_tx.write().unwrap() = None;
                    return;
                }
                std::thread::sleep(Duration::from_millis(c::READER_RESPAWN_BACKOFF_MS));
            }
        }
    }
}

fn schedule_restart(shared: &Shared, id: &str) {
    // Clear the dead handle immediately.
    *shared.reader_tx.write().unwrap() = None;

    static RESTARTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = RESTARTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if n > c::READER_MAX_RUNTIME_RESTARTS {
        log::error!(
            "reader '{id}' died {n} times; disabling. Investigate with `cliphistory doctor`."
        );
        *shared.reader_id.write().unwrap() = String::new();
        return;
    }
    log::info!(
        "restarting reader '{id}' in {}ms",
        c::READER_RESPAWN_BACKOFF_MS
    );
    let tx = shared.app_tx.clone();
    std::thread::Builder::new()
        .name("restart-timer".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(c::READER_RESPAWN_BACKOFF_MS));
            let _ = tx.send(AppEvent::RestartReader);
        })
        .ok();
}

// ---------------------------------------------------------------------------
// IPC handling
// ---------------------------------------------------------------------------

fn handle_conn(shared: &Shared, stream: UnixStream) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            log::warn!("client gone before request: {e}");
            return;
        }
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let req: IpcRequest = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            respond(&mut writer, IpcResponse::err(format!("bad request: {e}")));
            return;
        }
    };
    log::debug!("request: {req:?}");

    // Long-running operations are answered by worker threads.
    match req {
        IpcRequest::Show | IpcRequest::ModulesInstall { .. } => {
            let st = shared.clone();
            let req_clone = req.clone();
            std::thread::Builder::new()
                .name("slow-op".into())
                .spawn(move || {
                    let resp = match req_clone {
                        IpcRequest::Show => do_show(&st),
                        IpcRequest::ModulesInstall { ids, force } => do_install(&st, ids, force),
                        _ => unreachable!(),
                    };
                    respond(&mut writer, resp);
                })
                .ok();
        }
        other => respond(&mut writer, quick_dispatch(shared, other)),
    }
}

fn respond(writer: &mut UnixStream, resp: IpcResponse) {
    let mut line = match serde_json::to_vec(&resp) {
        Ok(v) => v,
        Err(e) => {
            log::error!("serialize response: {e}");
            return;
        }
    };
    line.push(b'\n');
    if writer.write_all(&line).is_err() {
        log::debug!("client disconnected before response");
    }
}

fn quick_dispatch(st: &Shared, req: IpcRequest) -> IpcResponse {
    match req {
        IpcRequest::Ping => IpcResponse::ok("pong"),
        IpcRequest::Status => status(st),
        IpcRequest::GetHistory { limit, query } => {
            match st
                .storage
                .history_items(limit.or(Some(c::DEFAULT_HISTORY_LIMIT)), query.as_deref())
            {
                Ok(items) => IpcResponse::History { items },
                Err(e) => IpcResponse::err(format!("{e:#}")),
            }
        }
        IpcRequest::DeleteItem { id } => match st.storage.delete(id) {
            Ok(true) => IpcResponse::ok(format!("deleted entry {id}")),
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        IpcRequest::SetPinned { id, pinned } => match st.storage.set_pinned(id, pinned) {
            Ok(true) => IpcResponse::ok(format!("entry {id} pinned={pinned}")),
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        IpcRequest::ClearAll => match st.storage.clear() {
            Ok(n) => IpcResponse::ok(format!("cleared {n} entries (pinned kept)")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        IpcRequest::CopyEntry { id } => copy_entry(st, id),
        IpcRequest::Doctor => IpcResponse::ok(doctor_text(st)),
        IpcRequest::ModulesList => match list_modules(st) {
            Ok(items) => IpcResponse::Modules { items },
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        IpcRequest::ModulesRemove { id } => match st.mm.uninstall(&id) {
            Ok(()) => IpcResponse::ok(format!("removed {id}")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        IpcRequest::StopDaemon => {
            let _ = st.app_tx.send(AppEvent::Shutdown);
            IpcResponse::ok("stopping")
        }
        IpcRequest::Show | IpcRequest::ModulesInstall { .. } => unreachable!(),
    }
}

fn status(st: &Shared) -> IpcResponse {
    let s = DaemonStatus {
        pid: std::process::id(),
        started_at: st.started_at,
        session: st.session.to_string(),
        reader: st.reader_id.read().unwrap().clone(),
        frontend: st.frontend_id.read().unwrap().clone(),
        entry_count: st.storage.count().unwrap_or(-1),
        db_path: st.storage.db_path().display().to_string(),
        db_size_bytes: st.storage.db_size_bytes(),
    };
    IpcResponse::Status(s)
}

fn copy_entry(st: &Shared, id: i64) -> IpcResponse {
    let content = match st.storage.content(id) {
        Ok(Some(c)) => c,
        Ok(None) => return IpcResponse::err(format!("no entry {id}")),
        Err(e) => return IpcResponse::err(format!("{e:#}")),
    };
    send_to_clipboard(st, &content);
    let _ = st.storage.mark_used(id);
    IpcResponse::ok(format!("copied {}", content.preview()))
}

fn send_to_clipboard(st: &Shared, content: &cliphistory_proto::Content) {
    let guard = st.reader_tx.read().unwrap();
    match guard.as_ref() {
        Some(tx) => {
            if tx
                .send(HostToReader::SetClipboard {
                    content: content.clone(),
                })
                .is_err()
            {
                log::error!("reader stdin closed; clipboard not updated");
            }
        }
        None => log::error!("no reader available to own the clipboard"),
    }
}

fn do_show(st: &Shared) -> IpcResponse {
    let items = match st.storage.history_items(Some(c::SHOW_ENTRIES_LIMIT), None) {
        Ok(i) => i,
        Err(e) => return IpcResponse::err(format!("{e:#}")),
    };
    if items.is_empty() {
        return IpcResponse::ok("clipboard history is empty");
    }

    let fid = st.frontend_id.read().unwrap().clone();
    if fid.is_empty() {
        return IpcResponse::err("no frontend configured; run `cliphistory doctor`");
    }
    let module = match st.mm.resolve(&fid) {
        Some(m) => m,
        None => return IpcResponse::err(format!("frontend '{fid}' not found")),
    };

    let response = crate::plugins::run_frontend(&module, &items, &st.cfg.frontend.extra_args);
    match response {
        Ok(cliphistory_proto::ShowResponse::Selected { id }) => copy_entry(st, id),
        Ok(cliphistory_proto::ShowResponse::Delete { id }) => match st.storage.delete(id) {
            Ok(true) => IpcResponse::ok(format!("deleted entry {id}")),
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        Ok(cliphistory_proto::ShowResponse::Clear) => quick_dispatch(st, IpcRequest::ClearAll),
        Ok(cliphistory_proto::ShowResponse::Dismissed) => IpcResponse::ok("dismissed"),
        Err(e) => IpcResponse::err(format!("frontend failed: {e:#}")),
    }
}

fn do_install(st: &Shared, ids: Vec<String>, force: bool) -> IpcResponse {
    if ids.is_empty() {
        return IpcResponse::err("no modules requested");
    }
    let mut msgs = Vec::new();
    for res in st.mm.ensure_available(&ids, force, &|m| log::info!("{m}")) {
        match res {
            Ok(m) => msgs.push(m),
            Err(e) => msgs.push(format!("error: {e:#}")),
        }
    }
    IpcResponse::ok(msgs.join("\n"))
}

fn list_modules(st: &Shared) -> Result<Vec<ModuleInfo>> {
    Ok(st
        .mm
        .list_installed()?
        .into_iter()
        .map(|m| ModuleInfo {
            id: m.manifest.id,
            kind: m.manifest.kind.as_str().to_string(),
            version: if m.version.is_empty() {
                "local".into()
            } else {
                m.version
            },
            capabilities: m.manifest.capabilities,
            requires: m.manifest.requires,
            description: m.manifest.description,
        })
        .collect())
}

fn doctor_text(st: &Shared) -> String {
    let installed = st.mm.list_installed().unwrap_or_default();
    let infos = to_installed_infos(&installed);
    let mut lines = Vec::new();

    match discovery::discover(&st.cfg, &infos) {
        Ok(report) => lines.push(report.to_string()),
        Err(e) => lines.push(format!("discovery failed: {e:#}")),
    }

    lines.push(format!(
        "daemon:     pid {}, up since unix {}",
        std::process::id(),
        st.started_at
    ));
    lines.push(format!(
        "active:     reader={} frontend={}",
        st.reader_id.read().unwrap(),
        st.frontend_id.read().unwrap()
    ));
    match st.storage.count() {
        Ok(n) => lines.push(format!(
            "storage:    {n} entries at {}",
            st.storage.db_path().display()
        )),
        Err(e) => lines.push(format!("storage:    ERROR {e:#}")),
    }
    if let Ok(items) = list_modules(st) {
        lines.push("modules:".into());
        for m in items {
            lines.push(format!(
                "  {:<18} v{} [{:?}] {}",
                m.id, m.version, m.kind, m.description
            ));
        }
    }
    lines.join("\n")
}

fn shutdown(st: &Shared, socket_path: &std::path::Path) {
    log::info!("shutting down");
    if let Some(tx) = st.reader_tx.read().unwrap().as_ref() {
        tx.send(HostToReader::Stop).ok();
    }
    let _ = std::fs::remove_file(socket_path);
}
