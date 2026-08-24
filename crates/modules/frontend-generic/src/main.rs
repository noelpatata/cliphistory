//! Universal cliphistory frontend.
//!
//! One built-in picker: an embedded egui window that draws itself on X11
//! and Wayland. No dmenu-style launchers, no zenity/kdialog, nothing to
//! install. See [`gui`] for the window implementation and
//! [`cliphistory_frontend_common`] for rendering/resolution.

use anyhow::Result;
use cliphistory_frontend_common as fc;
use cliphistory_module_common as mcommon;
use cliphistory_proto::{ModuleKind, ModuleManifest, PROTOCOL_VERSION};

mod gui;

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

    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<std::process::ExitCode> {
    let req = fc::read_request()?;
    let resp = gui::pick(&req)?;
    println!("{}", serde_json::to_string(&resp)?);
    Ok(std::process::ExitCode::SUCCESS)
}
