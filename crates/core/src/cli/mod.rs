//! CLI surface: argument types and IPC-backed command execution.
//!
//! Daemon-less commands (discover/config/modules) live in [`local`].

use crate::constants as c;
use crate::ipc::{self, IpcRequest, IpcResponse};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::Write;

pub mod local;

#[derive(Parser)]
#[command(
    name = c::APP_NAME,
    version,
    about = "Modular clipboard history manager for Linux",
    long_about = "cliphistory records everything you copy into a local history database.\n\
                  Bind a key to `cliphistory show` to browse and re-copy old entries."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the daemon (foreground; let your service manager background it)
    Serve,
    /// Open the frontend picker and copy the selected entry
    Show,
    /// List recent entries
    History {
        #[arg(short = 'n', long = "limit", default_value_t = c::DEFAULT_HISTORY_LIMIT)]
        limit: usize,
        #[arg(short, long)]
        query: Option<String>,
    },
    /// Remove one entry by id
    Remove { id: i64 },
    /// Push a stored entry back onto the clipboard
    Copy { id: i64 },
    /// Clear history (pinned entries survive)
    Clear,
    /// Protect an entry from pruning
    Pin { id: i64 },
    /// Remove an entry's protection again
    Unpin { id: i64 },
    /// Daemon status summary
    Status,
    /// Full diagnostic report (session, tools, distro, modules)
    Doctor,
    /// Print what discovery would pick right now, without the daemon
    Discover,
    /// Stop the daemon
    Stop,
    /// Manage configuration
    Config {
        #[command(subcommand)]
        cmd: ConfigCommand,
    },
    /// Manage reader/frontend modules
    Modules {
        #[command(subcommand)]
        cmd: ModulesCommand,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Create the default config file if missing
    Init,
    /// Print the config file location
    Path,
    /// Dump the effective configuration as JSON
    Print,
}

#[derive(Subcommand)]
pub enum ModulesCommand {
    /// List installed modules
    List,
    /// Download + install modules (defaults: whatever discovery wants)
    Install {
        ids: Vec<String>,
        /// Reinstall even when up to date
        #[arg(long)]
        force: bool,
    },
    /// Update installed modules from the configured source
    Update,
    /// Uninstall a module
    Remove { id: String },
}

/// Route a parsed command. IPC commands go through [`ask`]; everything else
/// delegates to [`local`].
pub fn execute(cmd: Command, cfg: &crate::config::Config) -> Result<i32> {
    match cmd {
        Command::Serve => crate::engine::run(cfg.clone()).map(|_| 0),
        Command::Show => ask(cfg, IpcRequest::Show),
        Command::History { limit, query } => ask(
            cfg,
            IpcRequest::GetHistory {
                limit: Some(limit),
                query,
            },
        ),
        Command::Remove { id } => ask(cfg, IpcRequest::DeleteItem { id }),
        Command::Copy { id } => ask(cfg, IpcRequest::CopyEntry { id }),
        Command::Clear => ask(cfg, IpcRequest::ClearAll),
        Command::Pin { id } => ask(cfg, IpcRequest::SetPinned { id, pinned: true }),
        Command::Unpin { id } => ask(cfg, IpcRequest::SetPinned { id, pinned: false }),
        Command::Status => ask(cfg, IpcRequest::Status),
        Command::Doctor => ask(cfg, IpcRequest::Doctor),
        Command::Stop => ask(cfg, IpcRequest::StopDaemon),
        Command::Discover => local::discover(cfg).map(|_| 0),
        Command::Config { cmd } => local::config_cmd(cmd).map(|_| 0),
        Command::Modules { cmd } => local::modules_cmd(cmd, cfg),
    }
}

/// Send a request to the daemon and render the response.
fn ask(_cfg: &crate::config::Config, req: IpcRequest) -> Result<i32> {
    match ipc::roundtrip(&req) {
        Ok(resp) => render(resp),
        Err(e) => {
            eprintln!("error: is the cliphistory daemon running? (`cliphistory serve`)\n  {e:#}");
            Ok(2)
        }
    }
}

fn render(resp: IpcResponse) -> Result<i32> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match resp {
        IpcResponse::Ok { message } => {
            writeln!(out, "{message}")?;
            Ok(0)
        }
        IpcResponse::Err { message } => {
            writeln!(out, "error: {message}")?;
            Ok(1)
        }
        IpcResponse::History { items } => {
            if items.is_empty() {
                writeln!(out, "(no entries)")?;
            }
            for it in items {
                let pin = if it.pinned { "*" } else { " " };
                writeln!(out, "{pin}{:>6}  {}", it.id, it.preview)?;
            }
            Ok(0)
        }
        IpcResponse::Status(s) => {
            writeln!(out, "pid:       {}", s.pid)?;
            writeln!(out, "session:   {}", s.session)?;
            writeln!(out, "clipboard: {}", s.clipboard_module)?;
            writeln!(out, "frontend:  {}", s.frontend_module)?;
            writeln!(out, "entries:   {}", s.entry_count)?;
            writeln!(out, "db:        {} ({} bytes)", s.db_path, s.db_size_bytes)?;
            Ok(0)
        }
        IpcResponse::Modules { items } => {
            if items.is_empty() {
                writeln!(out, "(no modules installed)")?;
            }
            for m in items {
                writeln!(
                    out,
                    "{:<18} {:<9} v{:<10} requires=[{}]",
                    m.id,
                    m.kind,
                    m.version,
                    m.requires.join(",")
                )?;
                if !m.description.is_empty() {
                    writeln!(out, "  {}", m.description)?;
                }
            }
            Ok(0)
        }
    }
}
