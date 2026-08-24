//! uinput auto-paste module.
//!
//! Creates a kernel-level virtual keyboard via `/dev/uinput` and replays a
//! paste chord through the input subsystem. This bypasses the display server
//! entirely, so it works on Wayland, X11 and TTY alike — and is immune to
//! compositor-side virtual-keyboard quirks. Requires write access to
//! `/dev/uinput` (logind grants it to the active seat by default).

mod device;
mod inject;

use anyhow::Result;
use cliphistory_module_common as mcommon;
use cliphistory_module_common::paster::{PasteBackend, serve};
use cliphistory_proto::{
    ModuleKind, ModuleManifest, PROTOCOL_VERSION,
};

const MODULE_ID: &str = "paster-uinput";

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
            "Replays the paste chord through a kernel uinput keyboard (no compositor support needed)"
                .into(),
    }
}

/// Replays the chord through the uinput device created at startup.
struct UinputBackend {
    keyboard: device::Keyboard,
}

impl PasteBackend for UinputBackend {
    fn play(&mut self) -> Result<()> {
        inject::play(&mut self.keyboard)
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--manifest") {
        return mcommon::manifest_main(manifest);
    }
    mcommon::init_logging();

    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let backend = UinputBackend {
        keyboard: device::Keyboard::create()?,
    };
    serve("uinput", backend)
}
