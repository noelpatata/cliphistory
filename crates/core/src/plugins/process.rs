//! Child-process plumbing shared by every module kind: spawn, frame
//! serialization on stdin, NDJSON parsing on stdout, and frontend runs.
//!
//! One generic [`ModuleHandle`] replaces what used to be near-identical
//! clipboard/paster handle types.

use super::model::InstalledModule;
use anyhow::{anyhow, Result};
use cliphistory_proto::{ClipboardToHost, HostToClipboard, HostToPaster, PasterToHost};
use serde::de::DeserializeOwned;
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;

/// Associates a module's two wire-frame directions so process plumbing can
/// be written once, generically.
pub trait FrameSpec: 'static {
    /// Frames sent by the host to the module (stdin).
    type ToModule: serde::Serialize + Send + 'static;
    /// Frames emitted by the module (stdout).
    type FromModule: DeserializeOwned + Send + 'static;

    fn parse_line(line: &str) -> Result<Self::FromModule>;
}

pub struct ClipboardFrames;

impl FrameSpec for ClipboardFrames {
    type ToModule = HostToClipboard;
    type FromModule = ClipboardToHost;

    fn parse_line(line: &str) -> Result<Self::FromModule> {
        serde_json::from_str(line).map_err(|e| anyhow!("unparsable reader line: {e}"))
    }
}

pub struct PasterFrames;

impl FrameSpec for PasterFrames {
    type ToModule = HostToPaster;
    type FromModule = PasterToHost;

    fn parse_line(line: &str) -> Result<Self::FromModule> {
        serde_json::from_str(line).map_err(|e| anyhow!("unparsable paster line: {e}"))
    }
}

/// Handle for a running clipboard module.
pub type ClipboardHandle = ModuleHandle<ClipboardFrames>;
/// Handle for a running paster module.
pub type PasterHandle = ModuleHandle<PasterFrames>;

/// A running module child plus a thread-safe channel to its stdin.
pub struct ModuleHandle<F: FrameSpec> {
    pub child: Child,
    tx: Sender<F::ToModule>,
}

impl<F: FrameSpec> ModuleHandle<F> {
    /// Spawn `<module> run` and wire a background writer for stdin frames.
    pub fn spawn(module: &InstalledModule) -> Result<Self> {
        let mut child = Command::new(&module.bin_path)
            .arg("run")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning module {}", module.bin_path.display()))?;

        let raw_stdin: ChildStdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("module stdin unavailable"))?;
        let (tx, rx) = std::sync::mpsc::channel::<F::ToModule>();
        std::thread::Builder::new()
            .name("module-stdin".into())
            .spawn(move || {
                let mut w = std::io::LineWriter::new(raw_stdin);
                for frame in rx {
                    if serde_json::to_writer(&mut w, &frame).is_err() {
                        break;
                    }
                    if w.write_all(b"\n").is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self { child, tx })
    }

    /// Queue a control frame for the module's stdin.
    pub fn send(&self, frame: F::ToModule) -> Result<()> {
        self.tx
            .send(frame)
            .map_err(|_| anyhow!("module stdin closed"))
    }

    /// Channel handle for cloning into long-lived consumers.
    pub fn sender(&self) -> &Sender<F::ToModule> {
        &self.tx
    }

    /// Destructure into the raw child and the stdin sender.
    pub fn into_parts(self) -> (Child, Sender<F::ToModule>) {
        (self.child, self.tx)
    }

    /// Spawn a thread parsing NDJSON stdout frames into a channel.
    pub fn pump_output(child: &mut Child, out: Sender<F::FromModule>) -> Result<()> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("module stdout unavailable"))?;
        std::thread::Builder::new()
            .name("module-stdout".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match F::parse_line(&line) {
                        Ok(frame) => {
                            if out.send(frame).is_err() {
                                break;
                            }
                        }
                        Err(e) => log::warn!("{e}: {line:.120}"),
                    }
                }
            })?;
        Ok(())
    }
}

use anyhow::Context;
use std::io::Write;

/// Run `<binary> --manifest` and parse its self-description.
pub fn query_manifest(bin: &std::path::Path) -> Result<cliphistory_proto::ModuleManifest> {
    let out = Command::new(bin)
        .arg("--manifest")
        .output()
        .with_context(|| format!("executing {}", bin.display()))?;
    if !out.status.success() {
        anyhow::bail!("{} --manifest failed: {}", bin.display(), out.status);
    }
    let m: cliphistory_proto::ModuleManifest = serde_json::from_slice(&out.stdout)
        .with_context(|| format!("parsing manifest of {}", bin.display()))?;
    if m.protocol_version != cliphistory_proto::PROTOCOL_VERSION {
        anyhow::bail!(
            "{} speaks protocol v{}, need v{}",
            bin.display(),
            m.protocol_version,
            cliphistory_proto::PROTOCOL_VERSION
        );
    }
    Ok(m)
}

/// Feed entries to a frontend and translate its answer.
pub fn run_frontend(
    module: &InstalledModule,
    entries: &[cliphistory_proto::HistoryItem],
    extra_args: &[String],
) -> Result<cliphistory_proto::ShowResponse> {
    let mut child = Command::new(&module.bin_path)
        .arg("run")
        .args(extra_args.iter())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning frontend {}", module.bin_path.display()))?;

    if let Some(mut stdin) = child.stdin.take() {
        serde_json::to_writer(
            &mut stdin,
            &cliphistory_proto::ShowRequest {
                entries: entries.to_vec(),
            },
        )?;
        stdin.flush()?;
        drop(stdin); // signals EOF so menus can render
    }

    let mut response_line = String::new();
    if let Some(stdout) = child.stdout.as_mut() {
        BufReader::new(stdout).read_line(&mut response_line)?;
    }
    let status = child.wait()?;

    if response_line.trim().is_empty() {
        if status.success() {
            return Ok(cliphistory_proto::ShowResponse::Dismissed);
        }
        anyhow::bail!("frontend exited with {status} without answering");
    }
    serde_json::from_str(response_line.trim())
        .with_context(|| format!("bad frontend reply: {response_line:?}"))
}
