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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::engine::state::{AppEvent, Shared};
    use crate::storage::{InsertOpts, Storage};
    use cliphistory_proto::Content;
    use std::sync::{mpsc, RwLock};

    /// A Shared wired to an in-memory DB and a no-op app channel: enough to
    /// exercise the synchronous IPC surface without a compositor.
    fn test_shared() -> Shared {
        let (app_tx, _rx) = mpsc::channel::<AppEvent>();
        let cfg = Config::default();
        Shared {
            cfg,
            storage: std::sync::Arc::new(Storage::open_in_memory().unwrap()),
            mm: std::sync::Arc::new(crate::plugins::ModuleManager::new(Default::default())),
            clipboard_tx: std::sync::Arc::new(RwLock::new(None)),
            clipboard_id: std::sync::Arc::new(RwLock::new(String::new())),
            paster_tx: std::sync::Arc::new(RwLock::new(None)),
            paster_id: std::sync::Arc::new(RwLock::new(String::new())),
            frontend_id: std::sync::Arc::new(RwLock::new(String::new())),
            started_at: 0,
            session: crate::discovery::SessionType::Tty,
            app_tx,
        }
    }

    fn seed_entry(st: &Shared, text: &str) -> i64 {
        match st.storage.insert(
            &Content::Text { text: text.into() },
            InsertOpts::sized(1000),
        ) {
            Ok(crate::storage::InsertOutcome::Inserted(id)) => id,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn history_roundtrip_through_dispatch() {
        let st = test_shared();
        let id = seed_entry(&st, "hello dispatch");
        let resp = quick_dispatch(&st, IpcRequest::GetHistory { limit: None, query: None });
        match resp {
            IpcResponse::History { items } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].id, id);
                assert_eq!(items[0].preview, "hello dispatch");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn search_filters_history() {
        let st = test_shared();
        seed_entry(&st, "alpha one");
        seed_entry(&st, "beta two");
        let resp = quick_dispatch(
            &st,
            IpcRequest::GetHistory {
                limit: None,
                query: Some("beta".into()),
            },
        );
        match resp {
            IpcResponse::History { items } => assert_eq!(items.len(), 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pin_flow_reports_state() {
        let st = test_shared();
        let id = seed_entry(&st, "pin me");
        assert!(matches!(
            quick_dispatch(&st, IpcRequest::SetPinned { id, pinned: true }),
            IpcResponse::Ok { .. }
        ));
        assert!(matches!(
            quick_dispatch(&st, IpcRequest::SetPinned { id: 999, pinned: true }),
            IpcResponse::Err { .. }
        ));
    }

    #[test]
    fn delete_is_idempotent_aware() {
        let st = test_shared();
        let id = seed_entry(&st, "doomed");
        assert!(matches!(
            quick_dispatch(&st, IpcRequest::DeleteItem { id }),
            IpcResponse::Ok { .. }
        ));
        assert!(matches!(
            quick_dispatch(&st, IpcRequest::DeleteItem { id }),
            IpcResponse::Err { .. }
        ));
    }

    #[test]
    fn copy_without_clipboard_module_still_succeeds() {
        // No clipboard module attached: copy must still store usage state
        // and answer Ok (the daemon degrades gracefully).
        let st = test_shared();
        let id = seed_entry(&st, "target");
        assert!(matches!(
            quick_dispatch(&st, IpcRequest::CopyEntry { id }),
            IpcResponse::Ok { .. }
        ));
    }

    #[test]
    fn stop_requests_shutdown() {
        let st = test_shared();
        assert!(matches!(
            quick_dispatch(&st, IpcRequest::StopDaemon),
            IpcResponse::Ok { .. }
        ));
    }
}
