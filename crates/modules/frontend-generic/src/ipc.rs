//! Frontend-side I/O plumbing: reading the show request and talking back
//! to the daemon.
//!
//! Long-lived pickers use [`delete_entry`] to act on the live history
//! without ending their one-response show lifetime: the delete shortcut
//! removes an entry server-side while the window stays open. The socket
//! path arrives via the `CLIPHISTORY_SOCKET` environment variable, set by
//! the daemon when it spawns this frontend.

use anyhow::{Context, Result};
use cliphistory_proto::{
    read_response, write_request, HistoryItem, IpcRequest, IpcResponse, ShowRequest,
};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// Read the one [`ShowRequest`] JSON document piped to our stdin.
pub fn read_request() -> Result<ShowRequest> {
    let mut buf = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buf)
        .context("reading show request")?;
    serde_json::from_slice(&buf).context("parsing show request")
}

/// Socket path handed to us by the daemon, if we run under one.
pub fn socket_from_env() -> Option<PathBuf> {
    std::env::var_os("CLIPHISTORY_SOCKET")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Ask the daemon to delete one history entry. Returns the daemon's
/// human-readable reply (e.g. `deleted entry 42`).
pub fn delete_entry(socket: &std::path::Path, id: i64) -> Result<String> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    write_request(&mut stream, &IpcRequest::DeleteItem { id })?;
    match read_response(&mut BufReader::new(stream))? {
        IpcResponse::Ok { message } => Ok(message),
        IpcResponse::Err { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected daemon reply: {other:?}")),
    }
}

/// Ask the daemon to clear history; pinned entries are kept.
pub fn clear_history(socket: &std::path::Path) -> Result<String> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    write_request(&mut stream, &IpcRequest::ClearAll)?;
    match read_response(&mut BufReader::new(stream))? {
        IpcResponse::Ok { message } => Ok(message),
        IpcResponse::Err { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected daemon reply: {other:?}")),
    }
}

/// Pin or unpin one history entry.
pub fn set_pinned(socket: &std::path::Path, id: i64, pinned: bool) -> Result<String> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    write_request(&mut stream, &IpcRequest::SetPinned { id, pinned })?;
    match read_response(&mut BufReader::new(stream))? {
        IpcResponse::Ok { message } => Ok(message),
        IpcResponse::Err { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected daemon reply: {other:?}")),
    }
}

/// A stream of history snapshots pushed by the daemon on every mutation.
///
/// Created by [`subscribe_history`]; each item is one full snapshot
/// (newest first, pinned on top). Ends with `Err` when the connection is
/// lost or the daemon does not support `WatchHistory`.
pub struct HistoryStream {
    reader: BufReader<UnixStream>,
}

impl Iterator for HistoryStream {
    type Item = Result<Vec<HistoryItem>>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => match serde_json::from_str::<IpcResponse>(line.trim()) {
                Ok(IpcResponse::History { items }) => Some(Ok(items)),
                Ok(IpcResponse::Err { message }) => Some(Err(anyhow::anyhow!(message))),
                Ok(other) => Some(Err(anyhow::anyhow!("unexpected daemon push: {other:?}"))),
                Err(e) => Some(Err(anyhow::anyhow!("bad daemon frame: {e}"))),
            },
            Err(_) => None,
        }
    }
}

/// Open a `WatchHistory` subscription against the daemon.
pub fn subscribe_history(socket: &std::path::Path) -> Result<HistoryStream> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    write_request(&mut stream, &IpcRequest::WatchHistory)?;
    Ok(HistoryStream {
        reader: BufReader::new(stream),
    })
}
