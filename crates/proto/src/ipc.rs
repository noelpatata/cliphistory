//! Daemon <-> client wire protocol.
//!
//! Typed frames for the newline-delimited JSON spoken over the daemon's
//! Unix socket, plus transport helpers shared by every client. The socket
//! *path* is deployment detail and stays in the core's config; frontends
//! receive it via the `CLIPHISTORY_SOCKET` environment variable.

use crate::HistoryItem;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};

/// Upper bound for one request/response line on the wire.
pub const MAX_IPC_LINE_BYTES: usize = 33_554_432;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    Ping,
    Status,
    GetHistory {
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        query: Option<String>,
    },
    DeleteItem {
        id: i64,
    },
    SetPinned {
        id: i64,
        pinned: bool,
    },
    ClearAll,
    /// Push a stored entry back to the system clipboard.
    CopyEntry {
        id: i64,
    },
    /// Open the frontend to pick an entry; copies the selection.
    Show,
    Doctor,
    ModulesList,
    ModulesInstall {
        ids: Vec<String>,
        #[serde(default)]
        force: bool,
    },
    ModulesRemove {
        id: String,
    },
    StopDaemon,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleInfo {
    pub id: String,
    pub kind: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub requires: Vec<String>,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub started_at: u64,
    pub session: String,
    /// Active clipboard module (reads and writes the system clipboard).
    pub clipboard_module: String,
    /// Active frontend module (renders the picker).
    pub frontend_module: String,
    /// Whether selections are automatically pasted into the focused window.
    pub auto_paste: bool,
    pub entry_count: i64,
    pub db_path: String,
    pub db_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "res", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok { message: String },
    History { items: Vec<HistoryItem> },
    Status(DaemonStatus),
    Modules { items: Vec<ModuleInfo> },
    Err { message: String },
}

impl IpcResponse {
    pub fn ok(msg: impl Into<String>) -> Self {
        IpcResponse::Ok {
            message: msg.into(),
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        IpcResponse::Err {
            message: msg.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

/// Serialize one request as a single NDJSON line.
pub fn encode_request(req: &IpcRequest) -> std::io::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push(b'\n');
    Ok(line)
}

/// Write one request to a stream as a single NDJSON line.
pub fn write_request<W: Write>(writer: &mut W, req: &IpcRequest) -> anyhow::Result<()> {
    writer.write_all(&encode_request(req)?)?;
    writer.flush()?;
    Ok(())
}

/// Read one response line, bounded by [`MAX_IPC_LINE_BYTES`].
pub fn read_response(reader: &mut impl BufRead) -> anyhow::Result<IpcResponse> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| anyhow::anyhow!("reading response: {e}"))?;
    if line.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "daemon closed the connection without answering"
        ));
    }
    if line.len() > MAX_IPC_LINE_BYTES {
        return Err(anyhow::anyhow!("response exceeds line limit"));
    }
    serde_json::from_str(line.trim()).map_err(|e| anyhow::anyhow!("parsing daemon response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips() {
        let req = IpcRequest::DeleteItem { id: 42 };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<IpcRequest>(&json).unwrap(), req);
    }

    #[test]
    fn response_tags_are_snake_case() {
        let json = serde_json::to_string(&IpcResponse::ok("hi")).unwrap();
        assert!(json.contains(r#""res":"ok""#));
    }

    #[test]
    fn delete_item_wire_shape_is_stable() {
        let json = serde_json::to_string(&IpcRequest::DeleteItem { id: 7 }).unwrap();
        assert_eq!(json, r#"{"cmd":"delete_item","id":7}"#);
    }

    #[test]
    fn read_response_rejects_garbage_and_eof() {
        let mut empty = "".as_bytes();
        assert!(read_response(&mut empty).is_err());
        let mut junk = "not json\n".as_bytes();
        assert!(read_response(&mut junk).is_err());
        let mut good = format!(
            "{}\n",
            serde_json::to_string(&IpcResponse::ok("x")).unwrap()
        )
        .into_bytes();
        assert!(matches!(
            read_response(&mut good.as_slice()).unwrap(),
            IpcResponse::Ok { .. }
        ));
    }

    #[test]
    fn write_request_emits_one_line() {
        let mut buf: Vec<u8> = Vec::new();
        write_request(&mut buf, &IpcRequest::Ping).unwrap();
        assert_eq!(buf, b"{\"cmd\":\"ping\"}\n");
    }
}
