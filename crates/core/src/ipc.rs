//! Daemon <-> client IPC: newline-delimited JSON over a Unix domain socket
//! at `$XDG_RUNTIME_DIR/cliphistory/cliphistory.sock`.
//!
//! The wire types and transport helpers live in
//! [`cliphistory_proto::ipc`] so modules (e.g. the graphical frontend's
//! delete shortcut) can speak the same protocol; this module only adds the
//! one-shot client used by CLI subcommands.

pub use cliphistory_proto::{
    read_response, write_request, DaemonStatus, IpcRequest, IpcResponse, ModuleInfo,
    MAX_IPC_LINE_BYTES,
};

use crate::config;
use anyhow::{Context, Result};
use std::io::BufReader;
use std::os::unix::net::UnixStream;

/// One-shot client used by every CLI subcommand.
pub fn roundtrip(req: &IpcRequest) -> Result<IpcResponse> {
    let path = config::socket_path();
    let mut stream =
        UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
    write_request(&mut stream, req)?;
    let mut reader = BufReader::new(stream);
    read_response(&mut reader)
}
