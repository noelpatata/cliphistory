//! IPC connection handling: read one request, dispatch it, write one
//! response. Business logic lives in [`super::actions`]; long-running
//! operations are answered by worker threads.

use super::actions;
use super::report;
use super::show_view;
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
            let _ = respond(&mut writer, IpcResponse::err(format!("bad request: {e}")));
            return;
        }
    };
    log::debug!("request: {req:?}");

    // History subscriptions hold the connection open and receive a frame
    // on every mutation — handled by a dedicated worker.
    if matches!(req, IpcRequest::WatchHistory) {
        let st = shared.clone();
        std::thread::Builder::new()
            .name("history-watch".into())
            .spawn(move || serve_history_watch(&st, &writer))
            .ok();
        return;
    }

    // Frontend runs and module installs can block for minutes; answer them
    // from a worker so the socket write happens whenever they finish.
    if matches!(req, IpcRequest::Show | IpcRequest::ModulesInstall { .. }) {
        let st = shared.clone();
        std::thread::Builder::new()
            .name("slow-op".into())
            .spawn(move || {
                let resp = actions::slow(&st, req);
                let _ = respond(&mut writer, resp);
            })
            .ok();
        return;
    }

    let _ = respond(&mut writer, quick_dispatch(shared, req));
}

/// Write one response frame; `Err` means the client is gone.
fn respond(writer: &mut UnixStream, resp: IpcResponse) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(&resp).map_err(|e| {
        log::error!("serialize response: {e}");
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
    line.push(b'\n');
    writer.write_all(&line)
}

/// Map a fallible operation onto an `Err` IPC response.
fn err_resp(e: impl std::fmt::Display) -> IpcResponse {
    IpcResponse::err(format!("{e:#}"))
}

/// Serve one `WatchHistory` connection: push a fresh history snapshot on
/// every mutation until the client goes away.
///
/// The initial snapshot is sent immediately so the subscriber starts with
/// a complete picture without a separate `GetHistory` round trip.
fn serve_history_watch(st: &Shared, writer: &UnixStream) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    st.history_watchers
        .lock()
        .expect("history watchers lock poisoned")
        .push(tx.clone());
    // Wake the new subscriber right away with the current state.
    let _ = tx.send(());

    let view = cliphistory_proto::ViewOptions {
        max_preview_lines: st.cfg.frontend.max_preview_lines,
        font_family: st.cfg.frontend.font_family.clone(),
        word_wrap: st.cfg.frontend.word_wrap,
        font_size: st.cfg.frontend.font_size,
        keys: st.cfg.frontend.keys.clone(),
        window_width: st.cfg.frontend.window_width,
    };

    let mut writer = match writer.try_clone() {
        Ok(w) => w,
        Err(_) => return, // registry entry is pruned by its dead receiver.
    };
    while rx.recv().is_ok() {
        let resp = match st
            .storage
            .history_items(Some(crate::constants::SHOW_ENTRIES_LIMIT), None)
        {
            Ok(items) => {
                let head_of = |id| {
                    st.storage
                        .content_head(id, show_view::PREVIEW_HEAD_BYTES)
                        .ok()
                        .flatten()
                        .filter(|h| h.kind == "text")
                        .map(|h| h.text)
                };
                let request = show_view::build_request(items, view.clone(), &head_of);
                IpcResponse::History {
                    items: request.entries,
                }
            }
            Err(e) => err_resp(e),
        };
        if respond(&mut writer, resp).is_err() {
            break; // client closed; dead receiver prunes itself on notify.
        }
    }
}

/// Requests answered synchronously on the connection thread.
fn quick_dispatch(st: &Shared, req: IpcRequest) -> IpcResponse {
    use crate::ipc::IpcRequest as R;
    match req {
        R::Ping => IpcResponse::ok("pong"),
        R::Status => actions::status(st),
        R::GetHistory { limit, query } => match st.storage.history_items(
            limit.or(Some(crate::constants::DEFAULT_HISTORY_LIMIT)),
            query.as_deref(),
        ) {
            Ok(items) => IpcResponse::History { items },
            Err(e) => err_resp(e),
        },
        R::DeleteItem { id } => match st.storage.delete(id) {
            Ok(true) => {
                st.notify_history_changed();
                IpcResponse::ok(format!("deleted entry {id}"))
            }
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => err_resp(e),
        },
        R::SetPinned { id, pinned } => match st.storage.set_pinned(id, pinned) {
            Ok(true) => {
                st.notify_history_changed();
                IpcResponse::ok(format!("entry {id} pinned={pinned}"))
            }
            Ok(false) => IpcResponse::err(format!("no entry {id}")),
            Err(e) => err_resp(e),
        },
        R::ClearAll => match st.storage.clear() {
            Ok(n) => {
                st.notify_history_changed();
                IpcResponse::ok(format!("cleared {n} entries (pinned kept)"))
            }
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
        // Handled by its own streaming worker before reaching here.
        R::WatchHistory => unreachable!(),
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
            history_watchers: Default::default(),
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
        let resp = quick_dispatch(
            &st,
            IpcRequest::GetHistory {
                limit: None,
                query: None,
            },
        );
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
            quick_dispatch(
                &st,
                IpcRequest::SetPinned {
                    id: 999,
                    pinned: true
                }
            ),
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
