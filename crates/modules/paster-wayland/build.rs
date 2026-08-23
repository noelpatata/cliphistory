//! Rebuild when the bundled virtual-keyboard protocol XML changes.
//!
//! Codegen itself happens inline via `wayland_scanner::generate_client_code!`
//! in src/main.rs.

fn main() {
    println!("cargo:rerun-if-changed=protocols/wlr-virtual-keyboard-unstable-v1.xml");
}
