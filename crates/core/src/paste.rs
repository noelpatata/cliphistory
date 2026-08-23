//! Auto-paste: replays the paste shortcut into the focused window after a
//! selection is written back to the clipboard.
//!
//! Injection is attempted through, in order:
//!
//! 1. the config's `general.paste_command` expert override,
//! 2. a native paster module (which owns its own key chord),
//! 3. an external tool probed on PATH for the current session.
//!
//! The core stays display-server agnostic: it never knows which keys a
//! module sends.

use anyhow::Result;
use cliphistory_proto::HostToPaster;
use crate::discovery::SessionType;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

/// External paste-injection tools and where they work. Probed on PATH in
/// this order within a matching session.
const TOOLS: &[Tool] = &[
    Tool {
        name: "wtype",
        cmd: "wtype -M ctrl -k v -m ctrl",
        sessions: &[SessionType::Wayland],
    },
    // uinput-level injectors work under any display server.
    Tool {
        name: "ydotool",
        cmd: "ydotool key 29:1 47:1 47:0 29:0",
        sessions: &[SessionType::Wayland, SessionType::X11, SessionType::Tty],
    },
    Tool {
        name: "dotool",
        cmd: "echo 'key ctrl+v' | dotool",
        sessions: &[SessionType::Wayland, SessionType::X11, SessionType::Tty],
    },
    Tool {
        name: "xdotool",
        cmd: "xdotool key --clearmodifiers ctrl+v",
        sessions: &[SessionType::X11],
    },
];

struct Tool {
    name: &'static str,
    cmd: &'static str,
    sessions: &'static [SessionType],
}

fn probe_tool(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Find the first available external tool usable in `session`.
pub fn find_tool(session: SessionType) -> Option<&'static str> {
    TOOLS
        .iter()
        .find(|t| t.sessions.contains(&session) && probe_tool(t.name))
        .map(|t| t.cmd)
}

/// Fire `command` after `delay` ms so the clipboard module has taken
/// ownership of the selection. Detached from the caller.
pub fn schedule_command(command: String, delay_ms: u64) {
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

/// Ask the running paster module to replay its paste chord after `delay` ms.
/// Detached from the caller; failures are logged by the supervisor when the
/// module reports them.
pub fn schedule_module(tx: Sender<HostToPaster>, delay_ms: u64) {
    thread::Builder::new()
        .name("auto-paste".into())
        .spawn(move || {
            thread::sleep(Duration::from_millis(delay_ms));
            match tx.send(HostToPaster::Paste) {
                Ok(()) => log::debug!("paste request sent to paster module"),
                Err(_) => log::warn!("paster module went away before pasting"),
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
