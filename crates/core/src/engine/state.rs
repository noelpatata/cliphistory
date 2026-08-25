//! Engine state shared across the event loop, dispatch and actions.

use crate::config::Config;
use crate::discovery;
use crate::plugins::ModuleManager;
use crate::storage::Storage;
use cliphistory_proto::{ClipboardToHost, HostToClipboard, HostToPaster, PasterToHost};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};

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
    /// Frontends subscribed via `WatchHistory`: one wake signal per open
    /// subscription. A dead receiver makes `send` fail, which is how the
    /// registry prunes itself.
    pub(crate) history_watchers: Arc<Mutex<Vec<Sender<()>>>>,
    pub(crate) started_at: u64,
    pub(crate) session: discovery::SessionType,
    pub(crate) app_tx: Sender<AppEvent>,
}

impl Shared {
    /// Wake every history subscriber; receivers that went away (picker
    /// closed, connection dropped) are pruned on the way.
    pub(crate) fn notify_history_changed(&self) {
        self.history_watchers
            .lock()
            .expect("history watchers lock poisoned")
            .retain(|tx| tx.send(()).is_ok());
    }
}

impl Shared {
    /// Trimmed `general.paste_command` override, when set.
    pub(crate) fn paste_command_override(&self) -> Option<&str> {
        self.cfg
            .general
            .paste_command
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
    }
}

/// Events feeding the daemon's main loop.
pub(crate) enum AppEvent {
    FromClipboard(ClipboardToHost),
    FromPaster(PasterToHost),
    Conn(UnixStream),
    ClipboardExited(String),
    PasterExited(String),
    RestartClipboard,
    RestartPaster,
    Shutdown,
}
