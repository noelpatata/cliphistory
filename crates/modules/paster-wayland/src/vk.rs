//! Generated client bindings for the bundled virtual-keyboard protocol XML.

// Generated code expects these names in scope.
#[allow(clippy::single_component_path_imports)]
use wayland_client;
use wayland_client::protocol::*;

pub mod __interfaces {
    pub use wayland_client::backend as wayland_backend;
    use wayland_client::protocol::__interfaces::*;
    #[allow(unused_imports)]
    use log;
    wayland_scanner::generate_interfaces!("protocols/wlr-virtual-keyboard-unstable-v1.xml");
}
use self::__interfaces::*;

// Expands to the client-side bindings for our bundled protocol XML.
wayland_scanner::generate_client_code!("protocols/wlr-virtual-keyboard-unstable-v1.xml");
