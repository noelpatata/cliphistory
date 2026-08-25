//! Daemon-side actions behind IPC requests: history queries, copy/paste
//! orchestration, frontend runs and module bookkeeping.

use super::show_view;
use super::Shared;
use crate::constants as c;
use crate::ipc::{DaemonStatus, IpcRequest, IpcResponse, ModuleInfo};
use crate::paste;
use anyhow::Result;
use cliphistory_proto::{Content, HostToClipboard};

/// Slow operations answered from worker threads.
pub(crate) fn slow(st: &Shared, req: IpcRequest) -> IpcResponse {
    match req {
        IpcRequest::Show => do_show(st),
        IpcRequest::ModulesInstall { ids, force } => do_install(st, ids, force),
        _ => unreachable!("slow() only receives Show/ModulesInstall"),
    }
}

/// Daemon status snapshot for `cliphistory status`.
pub(crate) fn status(st: &Shared) -> IpcResponse {
    let s = DaemonStatus {
        pid: std::process::id(),
        started_at: st.started_at,
        session: st.session.to_string(),
        clipboard_module: st.clipboard_id.read().unwrap().clone(),
        frontend_module: st.frontend_id.read().unwrap().clone(),
        auto_paste: auto_paste_possible(st),
        entry_count: st.storage.count().unwrap_or(-1),
        db_path: st.storage.db_path().display().to_string(),
        db_size_bytes: st.storage.db_size_bytes(),
    };
    IpcResponse::Status(s)
}

/// Push a stored entry to the system clipboard and auto-paste it.
pub(crate) fn copy_entry(st: &Shared, id: i64) -> IpcResponse {
    let t0 = std::time::Instant::now();
    let content = match st.storage.content(id) {
        Ok(Some(c)) => c,
        Ok(None) => return IpcResponse::err(format!("no entry {id}")),
        Err(e) => return IpcResponse::err(format!("{e:#}")),
    };
    let load = t0.elapsed();
    send_to_clipboard(st, &content);
    let sent = t0.elapsed();
    let _ = st.storage.mark_used(id);
    trigger_auto_paste(st);
    log::info!(
        "copy {id}: payload {}B, load={load:?} clipboard_write={:?} total={:?}",
        content.bytes().len(),
        sent - load,
        t0.elapsed()
    );
    IpcResponse::ok(format!("copied {}", content.preview()))
}

/// Replay the paste shortcut into the focused window, honouring:
/// `paste_command` override > native paster module > external tool.
fn trigger_auto_paste(st: &Shared) {
    if !st.cfg.general.auto_paste {
        return;
    }
    let delay = st.cfg.general.paste_delay_ms;

    if let Some(cmd) = st.paste_command_override() {
        paste::schedule_command(cmd.to_string(), delay);
        return;
    }

    if let Some(tx) = st.paster_tx.read().unwrap().clone() {
        paste::schedule_module(tx, delay);
        return;
    }

    match paste::find_tool(st.session) {
        Some(cmd) => paste::schedule_command(cmd.to_string(), delay),
        None => log::warn!("auto-paste enabled but no paster module or injection tool found"),
    }
}

/// True when auto-paste could fire right now.
fn auto_paste_possible(st: &Shared) -> bool {
    if !st.cfg.general.auto_paste {
        return false;
    }
    st.paste_command_override().is_some()
        || st.paster_tx.read().unwrap().is_some()
        || paste::find_tool(st.session).is_some()
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

/// Open the frontend picker and act on its answer.
pub(crate) fn do_show(st: &Shared) -> IpcResponse {
    use cliphistory_proto::{ShowResponse, ViewOptions};

    let items = match st.storage.history_items(Some(c::SHOW_ENTRIES_LIMIT), None) {
        Ok(i) => i,
        Err(e) => return IpcResponse::err(format!("{e:#}")),
    };
    if items.is_empty() {
        return IpcResponse::ok("clipboard history is empty");
    }

    let view = ViewOptions {
        max_preview_lines: st.cfg.frontend.max_preview_lines,
        font_family: st.cfg.frontend.font_family.clone(),
        word_wrap: st.cfg.frontend.word_wrap,
        font_size: st.cfg.frontend.font_size,
        keys: st.cfg.frontend.keys.clone(),
    };
    let head_of = |id| {
        st.storage
            .content_head(id, show_view::PREVIEW_HEAD_BYTES)
            .ok()
            .flatten()
            .filter(|h| h.kind == "text")
            .map(|h| h.text)
    };
    let request = show_view::build_request(items, view, &head_of);

    let fid = st.frontend_id.read().unwrap().clone();
    if fid.is_empty() {
        return IpcResponse::err("no frontend configured; run `cliphistory doctor`");
    }
    let Some(module) = st.mm.resolve(&fid) else {
        return IpcResponse::err(format!("frontend '{fid}' not found"));
    };

    match crate::plugins::run_frontend(
        &module,
        &request,
        &st.cfg.frontend.extra_args,
        &crate::config::socket_path(),
    ) {
        Ok(ShowResponse::Selected { id }) => {
            log::info!("show: entry {id} selected");
            copy_entry(st, id)
        }
        Ok(ShowResponse::Delete { id }) => match st.storage.delete(id) {
            Ok(true) => IpcResponse::ok(format!("deleted entry {id}")),
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        Ok(ShowResponse::Clear) => match st.storage.clear() {
            Ok(n) => IpcResponse::ok(format!("cleared {n} entries (pinned kept)")),
            Err(e) => IpcResponse::err(format!("{e:#}")),
        },
        Ok(ShowResponse::Dismissed) => IpcResponse::ok("dismissed"),
        Err(e) => IpcResponse::err(format!("frontend failed: {e:#}")),
    }
}

pub(crate) fn do_install(st: &Shared, ids: Vec<String>, force: bool) -> IpcResponse {
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

pub(crate) fn list_modules(st: &Shared) -> Result<Vec<ModuleInfo>> {
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
