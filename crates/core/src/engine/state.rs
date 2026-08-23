//! Engine state shared across the event loop, dispatch and actions.

use crate::config::Config;
use crate::discovery;
use crate::plugins::ModuleManager;
use crate::storage::Storage;
use cliphistory_proto::{ClipboardToHost, HostToClipboard, HostToPaster, PasterToHost};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};

/// Immutable-after-start configuration plus shared module channels.
///
/// The event loop owns one `Shared`; every connection worker gets a clone.
#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) cfg: Config,
    pub(crate) storage: Arc<Storage>,
    pub(crate) mm: Arc<ModuleManager>,
    pub(crate) clipboard_tx: Arc<RwLock<Option<Sender<HostToClipboard>>>>,
    pub(crate) clipboard_id: Arc<RwLock<String>>,
    pub(crate) paster_tx: Arc<RwLock<Option<Sender<HostToPaster>>>>,
    pub(crate) paster_id: Arc<RwLock<String>>,
    pub(crate) frontend_id: Arc<RwLock<String>>,
    pub(crate) started_at: u64,
    pub(crate) session: discovery::SessionType,
    pub(crate) app_tx: Sender<AppEvent>,
}

/// Events feeding the daemon's main loop.
pub(crate) enum AppEvent {
    FromClipboard(ClipboardToHost),
    FromPaster(PasterToHost),
    /// Consumed inside slot forwarding (e.g. paster Ready logging).
    Noop,
    Conn(UnixStream),
    ClipboardExited(String),
    PasterExited(String),
    RestartClipboard,
    RestartPaster,
    Shutdown,
}
