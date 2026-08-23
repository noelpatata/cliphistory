//! wofi (dmenu mode) frontend for cliphistory.

use cliphistory_frontend_common as fc;

fn main() -> std::process::ExitCode {
    fc::menu_main(fc::MenuSpec {
        id: "frontend-wofi",
        bin: || "wofi".into(),
        default_bin: "wofi",
        // `-Dimage_size` controls thumbnail height in px (wofi default 32).
        fixed_args: &[
            "--dmenu",
            "--allow-images",
            "-Dimage_size=96",
            "--insensitive",
            "--prompt",
            "cliphistory",
        ],
        images: true,
        description: "Renders history in wofi --dmenu (with image previews)",
    })
}
