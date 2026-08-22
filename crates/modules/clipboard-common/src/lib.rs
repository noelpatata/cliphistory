//! Shared plumbing for reader modules: manifest output and NDJSON frames.

use anyhow::{Context, Result};
use cliphistory_proto::{ClipboardToHost, HostToClipboard, ModuleManifest};
use std::io::{BufRead, Write};

/// How often readers without change notifications re-sample the clipboard.
pub const POLL_INTERVAL_MS: u64 = 500;

/// Print the module's self-description (invoked as `<module> --manifest`).
pub fn print_manifest(manifest: &ModuleManifest) -> Result<()> {
    let json = serde_json::to_string(manifest).context("serialize manifest")?;
    println!("{json}");
    Ok(())
}

/// Emit one frame to the host on stdout.
pub fn emit(frame: &ClipboardToHost) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, frame)?;
    lock.write_all(b"\n")?;
    lock.flush()?;
    Ok(())
}

/// Blocking read of the next control frame from stdin.
pub fn next_host_frame(reader: &mut impl BufRead) -> Option<HostToClipboard> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => match serde_json::from_str(line.trim()) {
            Ok(f) => Some(f),
            Err(_) => Some(HostToClipboard::Ping), // ignore garbage; keep alive
        },
    }
}
