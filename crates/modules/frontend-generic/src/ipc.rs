//! Frontend-side I/O plumbing: reading the show request and talking back
//! to the daemon.
//!
//! Long-lived pickers use [`delete_entry`] to act on the live history
//! without ending their one-response show lifetime: the delete shortcut
//! removes an entry server-side while the window stays open. The socket
//! path arrives via the `CLIPHISTORY_SOCKET` environment variable, set by
//! the daemon when it spawns this frontend.

use anyhow::{Context, Result};
use cliphistory_proto::{read_response, write_request, IpcRequest, IpcResponse, ShowRequest};
use std::io::{BufReader, Read};
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
