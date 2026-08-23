//! IPC request processing: everything a client connection can trigger.

use super::bootstrap;
use super::Shared;
use crate::constants as c;
use crate::discovery;
use crate::ipc::{DaemonStatus, IpcRequest, IpcResponse, ModuleInfo};
use crate::plugins;
use anyhow::Result;
use cliphistory_proto::{Content, HostToClipboard};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

/// Read one request line and dispatch it. Long-running operations are
/// answered by worker threads.
pub(crate) fn handle_conn(shared: &Shared, stream: UnixStream) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => {
            log::warn!("client gone before request");
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

    match req {
        IpcRequest::Show | IpcRequest::ModulesInstall { .. } => {
            let st = shared.clone();
            std::thread::Builder::new()
                .name("slow-op".into())
                .spawn(move || {
                    let resp = match req {
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
    use IpcRequest as R;
    match req {
        R::Ping => IpcResponse::ok("pong"),
        R::Status => status(st),
        R::GetHistory { limit, query } => {
            match st
                .storage
                .history_items(limit.or(Some(c::DEFAULT_HISTORY_LIMIT)), query.as_deref())
            {
                Ok(items) => IpcResponse::History { items },
                Err(e) => IpcResponse::err(format!("{e:#}")),
            }
        }
        R::DeleteItem { id } => match st.storage.delete(id) {
            Ok(true) => IpcResponse::ok(format!("deleted entry {id}")),
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        R::SetPinned { id, pinned } => match st.storage.set_pinned(id, pinned) {
            Ok(true) => IpcResponse::ok(format!("entry {id} pinned={pinned}")),
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        R::ClearAll => match st.storage.clear() {
            Ok(n) => IpcResponse::ok(format!("cleared {n} entries (pinned kept)")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        R::CopyEntry { id } => copy_entry(st, id),
        R::Doctor => IpcResponse::ok(doctor_text(st)),
        R::ModulesList => match list_modules(st) {
            Ok(items) => IpcResponse::Modules { items },
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        R::ModulesRemove { id } => match st.mm.uninstall(&id) {
            Ok(()) => IpcResponse::ok(format!("removed {id}")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        R::StopDaemon => {
            let _ = st.app_tx.send(super::AppEvent::Shutdown);
            IpcResponse::ok("stopping")
        }
        R::Show | R::ModulesInstall { .. } => unreachable!(),
    }
}

fn status(st: &Shared) -> IpcResponse {
    let s = DaemonStatus {
        pid: std::process::id(),
        started_at: st.started_at,
        session: st.session.to_string(),
        clipboard_module: st.clipboard_id.read().unwrap().clone(),
        frontend_module: st.frontend_id.read().unwrap().clone(),
        auto_paste: crate::paste::is_available() && st.cfg.general.auto_paste,
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
    let delay = st.cfg.general.paste_delay_ms;
    if st.cfg.general.auto_paste {
        if let Some(cmd) = crate::paste::find_tool() {
            crate::paste::schedule(cmd.to_string(), delay);
        }
    }
    IpcResponse::ok(format!("copied {}", content.preview()))
}

fn send_to_clipboard(st: &Shared, content: &Content) {
    let guard = st.clipboard_tx.read().unwrap();
    match guard.as_ref() {
        Some(tx) => {
            if tx
                .send(HostToClipboard::SetClipboard {
                    content: content.clone(),
                })
                .is_err()
            {
                log::error!("clipboard module stdin closed; clipboard not updated");
            }
        }
        None => log::error!("no clipboard module available to own the selection"),
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
    let Some(module) = st.mm.resolve(&fid) else {
        return IpcResponse::err(format!("frontend '{fid}' not found"));
    };

    match plugins::run_frontend(&module, &items, &st.cfg.frontend.extra_args) {
        Ok(cliphistory_proto::ShowResponse::Selected { id }) => {
            log::info!("show: entry {id} selected");
            copy_entry(st, id)
        }
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
                m.version.trim_start_matches('v').to_string()
            },
            capabilities: m.manifest.capabilities,
            requires: m.manifest.requires,
            description: m.manifest.description,
        })
        .collect())
}

fn doctor_text(st: &Shared) -> String {
    let installed = st.mm.list_installed().unwrap_or_default();
    let infos = bootstrap::to_installed_infos(&installed);
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
        "active:     clipboard={} frontend={}",
        st.clipboard_id.read().unwrap(),
        st.frontend_id.read().unwrap()
    ));
    let paste_tool = crate::paste::find_tool();
    lines.push(format!(
        "auto-paste: {}{}",
        if st.cfg.general.auto_paste && paste_tool.is_some() {
            "on"
        } else {
            "off"
        },
        paste_tool
            .map(|t| format!(" ({t})"))
            .unwrap_or_else(|| " (no tool found; install wtype)".into()),
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
                "  {:<20} {} [{:?}] {}",
                m.id, m.version, m.kind, m.description
            ));
        }
    }
    lines.join("\n")
}
