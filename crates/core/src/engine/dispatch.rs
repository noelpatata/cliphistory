//! IPC connection handling: read one request, dispatch it, write one
//! response. Business logic lives in [`super::actions`]; long-running
//! operations are answered by worker threads.

use super::actions;
use super::report;
use super::Shared;
use crate::ipc::{IpcRequest, IpcResponse};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

/// Read one request line and dispatch it.
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

    // Frontend runs and module installs can block for minutes; answer them
    // from a worker so the socket write happens whenever they finish.
    if matches!(req, IpcRequest::Show | IpcRequest::ModulesInstall { .. }) {
        let st = shared.clone();
        std::thread::Builder::new()
            .name("slow-op".into())
            .spawn(move || {
                let resp = actions::slow(&st, req);
                respond(&mut writer, resp);
            })
            .ok();
        return;
    }

    respond(&mut writer, quick_dispatch(shared, req));
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


/// Map a fallible operation onto an `Err` IPC response.
fn err_resp(e: impl std::fmt::Display) -> IpcResponse {
    IpcResponse::err(format!("{e:#}"))
}

/// Requests answered synchronously on the connection thread.
fn quick_dispatch(st: &Shared, req: IpcRequest) -> IpcResponse {
    use crate::ipc::IpcRequest as R;
    match req {
        R::Ping => IpcResponse::ok("pong"),
        R::Status => actions::status(st),
        R::GetHistory { limit, query } => match st
            .storage
            .history_items(limit.or(Some(crate::constants::DEFAULT_HISTORY_LIMIT)), query.as_deref())
        {
            Ok(items) => IpcResponse::History { items },
            Err(e) => err_resp(e),
        },
        R::DeleteItem { id } => match st.storage.delete(id) {
            Ok(true) => IpcResponse::ok(format!("deleted entry {id}")),
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => err_resp(e),
        },
        R::SetPinned { id, pinned } => match st.storage.set_pinned(id, pinned) {
            Ok(true) => IpcResponse::ok(format!("entry {id} pinned={pinned}")),
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => err_resp(e),
        },
        R::ClearAll => match st.storage.clear() {
            Ok(n) => IpcResponse::ok(format!("cleared {n} entries (pinned kept)")),
            Err(e) => err_resp(e),
        },
        R::CopyEntry { id } => actions::copy_entry(st, id),
        R::Doctor => IpcResponse::ok(report::doctor_text(st)),
        R::ModulesList => match actions::list_modules(st) {
            Ok(items) => IpcResponse::Modules { items },
            Err(e) => err_resp(e),
        },
        R::ModulesRemove { id } => match st.mm.uninstall(&id) {
            Ok(()) => IpcResponse::ok(format!("removed {id}")),
            Err(e) => err_resp(e),
        },
        R::StopDaemon => {
            let _ = st.app_tx.send(super::AppEvent::Shutdown);
            IpcResponse::ok("stopping")
        }
        // Handled on the slow-op path before reaching here.
        R::Show | R::ModulesInstall { .. } => unreachable!(),
    }
}
