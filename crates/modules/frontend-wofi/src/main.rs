//! wofi (dmenu mode) frontend for cliphistory.

use anyhow::Result;
use cliphistory_frontend_common as fc;
use cliphistory_proto::{ModuleKind, ModuleManifest, PROTOCOL_VERSION};

const MODULE_ID: &str = "frontend-wofi";
const MENU_BIN: &str = "wofi";
const FIXED_ARGS: &[&str] = &["--dmenu", "--insensitive", "--prompt", "cliphistory"];

fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: MODULE_ID.into(),
        kind: ModuleKind::Frontend,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![],
        requires: vec![MENU_BIN.into()],
        description: "Renders history in wofi --dmenu".into(),
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
    let resp = fc::run_menu(MENU_BIN, FIXED_ARGS, passthrough)?;
    println!("{}", serde_json::to_string(&resp)?);
    Ok(std::process::ExitCode::SUCCESS)
}
