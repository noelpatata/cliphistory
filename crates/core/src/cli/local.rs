//! Daemon-less commands: discovery preview, config management and module
//! installation against the local filesystem / configured release source.

use super::{ConfigCommand, ModulesCommand};
use crate::config::{self, Config};
use crate::constants as c;
use crate::discovery;
use crate::plugins::ModuleManager;
use anyhow::{bail, Result};

pub(crate) fn discover(cfg: &Config) -> Result<()> {
    let mm = ModuleManager::new(cfg.modules.clone());
    let installed = mm.list_installed()?;
    println!("modules installed at: {}", mm.install_root().display());
    let infos = crate::engine::bootstrap::to_installed_infos(&installed);
    let report = discovery::discover(cfg, &infos)?;
    println!("{report}");

    println!("\nwould use:");
    for (label, ids) in [
        ("reader", &report.readers),
        ("frontend", &report.frontends),
        ("paster", &report.pasters),
    ] {
        println!(
            "  {label:<9} {}",
            ids.first().map(String::as_str).unwrap_or("<none available>")
        );
    }
    if report.session == discovery::SessionType::Tty {
        println!("\nnote: no graphical session detected");
    }
    Ok(())
}

pub(crate) fn config_cmd(cmd: ConfigCommand) -> Result<()> {
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

pub(crate) fn modules_cmd(cmd: ModulesCommand, cfg: &Config) -> Result<i32> {
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
                    "{:<20} {:<10} caps=[{}] requires=[{}]",
                    m.manifest.id,
                    if m.version.is_empty() {
                        "local".to_string()
                    } else {
                        m.version.trim_start_matches('v').to_string()
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

/// Default install targets: whatever discovery would pick for this machine.
fn resolve_target_ids(mm: &ModuleManager, cfg: &Config, ids: Vec<String>) -> Result<Vec<String>> {
    if !ids.is_empty() {
        return Ok(ids);
    }
    let installed = mm.list_installed()?;
    let infos = crate::engine::bootstrap::to_installed_infos(&installed);
    let report = discovery::discover(cfg, &infos)?;
    let mut targets: Vec<String> = Vec::new();
    if let Some(r) = report.readers.first() {
        targets.push(r.clone());
    }
    if let Some(f) = report.frontends.first() {
        targets.push(f.clone());
    }
    if let Some(p) = report.pasters.first() {
        targets.push(p.clone());
    }
    if targets.is_empty() {
        // Nothing installed yet -> offer every known candidate.
        let session = discovery::detect_session(&discovery::RealEnv);
        targets.extend(session.clipboard_candidates().iter().map(|s| s.to_string()));
        targets.extend(c::FRONTEND_CANDIDATES.iter().map(|s| s.to_string()));
        targets.extend(c::PASTER_CANDIDATES.iter().map(|s| s.to_string()));
    }
    Ok(targets)
}
