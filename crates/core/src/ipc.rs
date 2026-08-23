//! Daemon <-> client IPC: newline-delimited JSON over a Unix domain socket
//! at `$XDG_RUNTIME_DIR/cliphistory/cliphistory.sock`.

use crate::config;
use crate::constants as c;
use anyhow::{anyhow, Context, Result};
use cliphistory_proto::HistoryItem;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

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
    /// Active paster module, if the platform supports native paste injection.
    pub paster_module: String,
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

pub fn write_request(stream: &mut UnixStream, req: &IpcRequest) -> Result<()> {
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.flush()?;
    Ok(())
}

pub fn read_response(reader: &mut impl BufRead) -> Result<IpcResponse> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| anyhow!("reading response: {e}"))?;
    if line.trim().is_empty() {
        return Err(anyhow!("daemon closed the connection without answering"));
    }
    if line.len() > c::MAX_IPC_LINE_BYTES {
        return Err(anyhow!("response exceeds line limit"));
    }
    serde_json::from_str(line.trim()).context("parsing daemon response")
}

/// One-shot client used by every CLI subcommand.
pub fn roundtrip(req: &IpcRequest) -> Result<IpcResponse> {
    let path = config::socket_path();
    let mut stream =
        UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
    write_request(&mut stream, req)?;
    let mut reader = BufReader::new(stream);
    read_response(&mut reader)
}
