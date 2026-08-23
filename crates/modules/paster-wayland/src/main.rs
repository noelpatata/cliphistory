//! Wayland auto-paste module.
//!
//! Injects the paste chord into the focused surface through the compositor's
//! `zwp_virtual_keyboard_v1` interface, so auto-paste needs no external
//! binaries. Speaks the paster NDJSON protocol from [`cliphistory_proto`].

mod connection;
mod inject;
mod vk;

use anyhow::Result;
use cliphistory_module_common as mcommon;
use cliphistory_module_common::paster::{PasteBackend, serve};
use cliphistory_proto::{ModuleKind, ModuleManifest, PROTOCOL_VERSION};

const MODULE_ID: &str = "paster-wayland";

fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: MODULE_ID.into(),
        kind: ModuleKind::Paster,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![cliphistory_proto::CAP_WRITE.into()],
        features: vec![],
        requires: vec![],
        description:
            "Replays the paste chord via the Wayland virtual keyboard protocol".into(),
    }
}

/// Replays the chord through the virtual keyboard created at startup.
struct VkBackend {
    keyboard: connection::Keyboard,
}

impl PasteBackend for VkBackend {
    fn play(&mut self) -> Result<()> {
        inject::play(&mut self.keyboard)
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--manifest") {
        return mcommon::manifest_main(manifest);
    }

    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let backend = VkBackend {
        keyboard: connection::Keyboard::connect()?,
    };
    serve("virtual keyboard", backend)
}
