//! rofi (dmenu mode) frontend for cliphistory.

use cliphistory_frontend_common as fc;

fn main() -> std::process::ExitCode {
    fc::menu_main(fc::MenuSpec {
        id: "frontend-rofi",
        bin: || "rofi".into(),
        default_bin: "rofi",
        fixed_args: &["-dmenu", "-i", "-p", "cliphistory"],
        images: false,
        description: "Renders history in rofi -dmenu",
    })
}
