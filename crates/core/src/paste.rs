//! Auto-paste: replays the paste shortcut into whatever window has focus
//! right after cliphistory writes a selection back to the clipboard.
//!
//! The actual keystroke is delegated to an external tool configured by the
//! user (`general.paste_command`, e.g. wtype), so this module owns nothing
//! but timing and error reporting.

use crate::constants as c;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Fire `command` once the clipboard module had `[PASTE_DELAY_MS]` to take
/// ownership of the selection. Detached: never blocks the daemon.
/// A missing/blank command is a no-op (feature disabled).
pub fn schedule_opt(command: Option<String>) {
    let Some(command) = command.filter(|c| !c.trim().is_empty()) else {
        return;
    };
    thread::Builder::new()
        .name("auto-paste".into())
        .spawn(move || run(&command))
        .ok();
}

fn run(command: &str) {
    thread::sleep(Duration::from_millis(c::PASTE_DELAY_MS));
    match Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(out) if out.status.success() => {
            log::debug!("auto-paste command succeeded");
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            log::warn!("auto-paste command failed ({}): {stderr:.200}", out.status);
        }
        Err(e) => log::warn!("auto-paste command could not start: {e}"),
    }
}
