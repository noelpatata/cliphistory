//! dmenu (or bemenu on Wayland) frontend for cliphistory.
//!
//! The menu binary is taken from `CLIPWELL_DMENU_BIN`, falling back to
//! `dmenu`; bemenu users can point the variable at it.

use cliphistory_frontend_common as fc;

fn main() -> std::process::ExitCode {
    fc::menu_main(fc::MenuSpec {
        id: "frontend-dmenu",
        bin: || std::env::var("CLIPWELL_DMENU_BIN").unwrap_or_else(|_| "dmenu".into()),
        default_bin: "dmenu",
        fixed_args: &["-i", "-p", "cliphistory"],
        images: false,
        description:
            "Renders history via a dmenu-compatible binary (CLIPWELL_DMENU_BIN overrides)",
    })
}
