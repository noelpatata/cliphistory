//! Auto-paste: replays the paste shortcut into the focused window after a
//! selection is written back to the clipboard.
//!
//! Injection is delegated to the best available external tool, probed in
//! order. Each tool has its own invocation string; adding support for a new
//! one is a single table entry.

use anyhow::Result;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Paste injection tools, probed on PATH in this order.
const TOOLS: &[(&str, &str)] = &[
    ("wtype", "wtype -M ctrl -k v -m ctrl"),
    ("ydotool", "ydotool key 29:1 47:1 47:0 29:0"),
    ("dotool", "echo 'key ctrl+v' | dotool"),
];

/// Find the first available paste tool and return its command.
pub fn find_tool() -> Option<&'static str> {
    TOOLS
        .iter()
        .find(|(name, _)| probe_tool(name))
        .map(|(_, cmd)| *cmd)
}

fn probe_tool(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// True when auto-paste can actually fire (tool found + enabled).
pub fn is_available() -> bool {
    TOOLS.iter().any(|(name, _)| probe_tool(name))
}

/// Fire `command` after `delay` ms so the clipboard module has taken
/// ownership of the selection. Detached from the caller.
pub fn schedule(command: String, delay_ms: u64) {
    thread::Builder::new()
        .name("auto-paste".into())
        .spawn(move || {
            thread::sleep(Duration::from_millis(delay_ms));
            if let Err(e) = run(&command) {
                log::warn!("auto-paste failed: {e:#}");
            } else {
                log::debug!("auto-paste command succeeded");
            }
        })
        .ok();
}

fn run(command: &str) -> Result<()> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("exit {}: {stderr:.200}", output.status)
    }
}
