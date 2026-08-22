//! CLI surface. Every subcommand either talks to a running daemon over IPC
//! or performs local operations (config, discovery, module management).

use crate::config::{self, Config};
use crate::constants as c;
use crate::discovery;
use crate::ipc::{self, IpcRequest, IpcResponse};
use crate::plugins::ModuleManager;
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::io::Write;

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

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub fn execute(cmd: Command, cfg: &Config) -> Result<i32> {
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
        Command::Discover => discover_local(cfg).map(|_| 0),
        Command::Config { cmd } => config_cmd(cmd, cfg).map(|_| 0),
        Command::Modules { cmd } => modules_cmd(cmd, cfg),
    }
}

/// Send a request to the daemon and render the response.
fn ask(_cfg: &Config, req: IpcRequest) -> Result<i32> {
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
            writeln!(out, "reader:    {}", s.reader)?;
            writeln!(out, "frontend:  {}", s.frontend)?;
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

// ---------------------------------------------------------------------------
// Local (daemon-less) commands
// ---------------------------------------------------------------------------

fn discover_local(cfg: &Config) -> Result<()> {
    let mm = ModuleManager::new(cfg.modules.clone());
    let installed = mm.list_installed()?;
    println!("modules installed at: {}", mm.install_root().display());
    let infos = crate::engine::to_installed_infos(&installed);
    let report = discovery::discover(cfg, &infos)?;
    println!("{report}");

    let session = report.session;
    println!("\nwould use:");
    println!(
        "  reader:   {}",
        report
            .readers
            .first()
            .map(String::as_str)
            .unwrap_or("<none available>")
    );
    println!(
        "  frontend: {}",
        report
            .frontends
            .first()
            .map(String::as_str)
            .unwrap_or("<none available>")
    );
    if session == discovery::SessionType::Tty {
        println!("\nnote: no graphical session detected");
    }
    Ok(())
}

fn config_cmd(cmd: ConfigCommand, _cfg: &Config) -> Result<()> {
    match cmd {
        ConfigCommand::Init => {
            let path = config::config_path();
            Config::write_template(&path)?;
            println!("wrote {}", path.display());
            Ok(())
        }
        ConfigCommand::Path => {
            println!("{}", config::config_path().display());
            Ok(())
        }
        ConfigCommand::Print => {
            let cfg = Config::load()?;
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            Ok(())
        }
    }
}

fn modules_cmd(cmd: ModulesCommand, cfg: &Config) -> Result<i32> {
    let mm = ModuleManager::new(cfg.modules.clone());
    match cmd {
        ModulesCommand::List => {
            let mods = mm.list_installed()?;
            if mods.is_empty() {
                println!("(no modules installed)");
                println!("try: cliphistory modules install");
                return Ok(0);
            }
            for m in mods {
                println!(
                    "{:<18} v{:<10} caps=[{}] requires=[{}]",
                    m.manifest.id,
                    if m.version.is_empty() {
                        "local"
                    } else {
                        &m.version
                    },
                    m.manifest.capabilities.join(","),
                    m.manifest.requires.join(",")
                );
            }
            Ok(0)
        }
        ModulesCommand::Install { ids, force } => {
            let ids = resolve_target_ids(&mm, cfg, ids)?;
            if ids.is_empty() {
                bail!("nothing to install");
            }
            let mut failed = false;
            for res in mm.ensure_available(&ids, force, &|m| println!("{m}")) {
                if let Err(e) = res {
                    failed = true;
                    println!("error: {e:#}");
                }
            }
            if failed {
                Ok(1)
            } else {
                println!("\nrestart the daemon (`cliphistory stop && cliphistory serve`) to apply");
                Ok(0)
            }
        }
        ModulesCommand::Update => {
            let installed = mm.list_installed()?;
            if mm.cfg_uses_local_dir() {
                bail!("update is disabled while modules.local_dir is configured");
            }
            let rm = mm.fetch_remote_manifest(None)?;
            let ids: Vec<String> = installed
                .iter()
                .filter(|m| m.version != rm.release)
                .map(|m| m.manifest.id.clone())
                .collect();
            if ids.is_empty() {
                println!("all modules up to date ({})", rm.release);
                return Ok(0);
            }
            let mut failed = false;
            for res in mm.ensure_available(&ids, true, &|m| println!("{m}")) {
                if let Err(e) = res {
                    failed = true;
                    println!("error: {e:#}");
                }
            }
            Ok(if failed { 1 } else { 0 })
        }
        ModulesCommand::Remove { id } => {
            mm.uninstall(&id)?;
            println!("removed {id}");
            Ok(0)
        }
    }
}

fn resolve_target_ids(mm: &ModuleManager, cfg: &Config, ids: Vec<String>) -> Result<Vec<String>> {
    if !ids.is_empty() {
        return Ok(ids);
    }
    // Default: whatever discovery would pick for this machine.
    let installed = mm.list_installed()?;
    let infos = crate::engine::to_installed_infos(&installed);
    let report = discovery::discover(cfg, &infos)?;
    let mut targets: Vec<String> = Vec::new();
    if let Some(r) = report.readers.first() {
        targets.push(r.clone());
    }
    if let Some(f) = report.frontends.first() {
        targets.push(f.clone());
    }
    if targets.is_empty() {
        // Nothing installed yet -> offer every known candidate.
        let session = discovery::detect_session(&discovery::RealEnv);
        targets.extend(session.reader_candidates().iter().map(|s| s.to_string()));
        targets.extend(c::FRONTEND_CANDIDATES.iter().map(|s| s.to_string()));
    }
    Ok(targets)
}
