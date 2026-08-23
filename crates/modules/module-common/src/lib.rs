//! Shared plumbing for cliphistory modules.
//!
//! Everything a module binary needs that is not specific to its platform:
//! NDJSON frame I/O on stdio, manifest printing, and the paste-chord model
//! shared by all paster backends.

pub mod chord;
pub mod paster;
pub mod reader;
pub mod util;

use anyhow::{Context, Result};
use cliphistory_proto::ModuleManifest;
use serde::de::DeserializeOwned;
use std::io::{BufRead, Write};

/// Print the module's self-description (invoked as `<module> --manifest`).
pub fn print_manifest(manifest: &ModuleManifest) -> Result<()> {
    let json = serde_json::to_string(manifest).context("serialize manifest")?;
    println!("{json}");
    Ok(())
}

/// Emit one frame to the host on stdout as a single NDJSON line.
pub fn emit<T: serde::Serialize>(frame: &T) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, frame)?;
    lock.write_all(b"\n")?;
    lock.flush()?;
    Ok(())
}

/// Entry point for the `--manifest` convention shared by every module:
/// prints the manifest JSON on stdout, or the error on stderr with a
/// failure exit code.
pub fn manifest_main(build: impl FnOnce() -> ModuleManifest) -> std::process::ExitCode {
    match print_manifest(&build()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Blocking read of the next NDJSON frame from stdin.
///
/// Returns `None` on EOF (host went away) and skips unparsable lines so a
/// stray write never kills the module; garbage is reported through
/// `on_garbage` when provided.
pub fn next_frame<T, F>(reader: &mut impl BufRead, on_garbage: F) -> Option<T>
where
    T: DeserializeOwned,
    F: Fn(&str, &serde_json::Error),
{
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<T>(trimmed) {
            Ok(frame) => return Some(frame),
            Err(e) => on_garbage(trimmed, &e),
        }
    }
}
