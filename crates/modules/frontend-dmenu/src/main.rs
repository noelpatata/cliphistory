//! dmenu (or bemenu on Wayland) frontend for cliphistory.
//!
//! The menu binary is taken from `CLIPWELL_DMENU_BIN`, falling back to
//! `dmenu`; bemenu users can point the variable at it.

use anyhow::Result;
use cliphistory_frontend_common as fc;
use cliphistory_proto::{ModuleKind, ModuleManifest, PROTOCOL_VERSION};

const MODULE_ID: &str = "frontend-dmenu";
const DEFAULT_MENU_BIN: &str = "dmenu";
const BIN_ENV_VAR: &str = "CLIPWELL_DMENU_BIN";
const FIXED_ARGS: &[&str] = &["-i", "-p", "cliphistory"];

fn menu_bin() -> String {
    std::env::var(BIN_ENV_VAR).unwrap_or_else(|_| DEFAULT_MENU_BIN.to_string())
}

fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: MODULE_ID.into(),
        kind: ModuleKind::Frontend,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![],
        requires: vec![DEFAULT_MENU_BIN.into()],
        description: format!(
            "Renders history via a dmenu-compatible binary ({BIN_ENV_VAR} overrides)"
        ),
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--manifest") {
        return match fc::print_manifest(&manifest()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e:#}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    match run_module(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run_module(passthrough: &[String]) -> Result<std::process::ExitCode> {
    let resp = fc::run_menu(&menu_bin(), FIXED_ARGS, passthrough)?;
    println!("{}", serde_json::to_string(&resp)?);
    Ok(std::process::ExitCode::SUCCESS)
}
