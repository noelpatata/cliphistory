//! Universal cliphistory frontend.
//!
//! One built-in picker: an embedded egui window that draws itself on X11
//! and Wayland. No dmenu-style launchers, no zenity/kdialog, nothing to
//! install. See [`gui`] for the window implementation and [`ipc`] for
//! request reading and daemon calls.

use anyhow::Result;
use cliphistory_module_common as mcommon;
use cliphistory_proto::{ModuleKind, ModuleManifest, PROTOCOL_VERSION};

mod gui;
mod ipc;

const MODULE_ID: &str = "frontend-generic";

fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: MODULE_ID.into(),
        kind: ModuleKind::Frontend,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![],
        features: vec![],
        requires: vec![],
        description: "Built-in egui picker window (X11 + Wayland, no external tools)".into(),
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--manifest") {
        return mcommon::manifest_main(manifest);
    }
    mcommon::init_logging();

    match run() {
        Ok(code) => code,
        Err(e) => {
            log::error!("frontend error: {e:#}");
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<std::process::ExitCode> {
    let req = ipc::read_request()?;
    log::debug!(
        "frontend spawned: {} entries, socket={}",
        req.entries.len(),
        ipc::socket_from_env().is_some()
    );
    let resp = gui::pick(&req, ipc::socket_from_env())?;
    log::info!("frontend finished: {resp:?}");
    println!("{}", serde_json::to_string(&resp)?);
    Ok(std::process::ExitCode::SUCCESS)
}
