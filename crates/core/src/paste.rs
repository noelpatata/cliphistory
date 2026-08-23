//! Auto-paste: replays the paste shortcut into whatever window has focus
//! right after cliphistory writes a selection back to the clipboard.
//!
//! The actual keystroke is delegated to an external tool configured by the
//! user (`general.paste_command`, e.g. wtype), so this module owns nothing
//! but timing and error reporting.

use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// True when a paste override command is configured.
pub fn has_override(command: &Option<String>) -> bool {
    command.as_deref().is_some_and(|c| !c.trim().is_empty())
}

/// Fire an external paste `command` after `delay`. Detached from the caller;
/// failures are logged as warnings.
pub fn schedule_command(command: String, delay: Duration) {
    thread::Builder::new()
        .name("auto-paste".into())
        .spawn(move || run(&command, delay))
        .ok();
}

fn run(command: &str, delay: Duration) {
    thread::sleep(delay);
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
