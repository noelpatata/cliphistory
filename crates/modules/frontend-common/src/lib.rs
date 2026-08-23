//! Shared plumbing for cliphistory frontend modules.
//!
//! A frontend reads one [`ShowRequest`] JSON document from stdin, renders a
//! picker using its menu program of choice and prints a single
//! [`ShowResponse`] JSON document on stdout.
//!
//! Line formatting and selection parsing live in [`render`]; this module
//! only does process plumbing.

pub mod render;

use anyhow::{Context, Result};
use cliphistory_proto::{ModuleManifest, ShowRequest, ShowResponse};
use render::Selection;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

/// Print the module's self-description (invoked as `<module> --manifest`).
pub fn print_manifest(manifest: &ModuleManifest) -> Result<()> {
    let json = serde_json::to_string(manifest).context("serialize manifest")?;
    println!("{json}");
    Ok(())
}

/// Run the menu binary and translate its answer into a [`ShowResponse`].
///
/// `render_images` must mirror the frontend's declared `images` feature;
/// line formatting and selection parsing live in [`render`].
pub fn run_menu(
    bin: &str,
    fixed_args: &[&str],
    passthrough: &[String],
    render_images: bool,
) -> Result<ShowResponse> {
    let request = read_show_request()?;
    if request.entries.is_empty() {
        return Ok(ShowResponse::Dismissed);
    }

    let mut child = Command::new(bin)
        .args(fixed_args)
        .args(passthrough.iter())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning {bin}"))?;

    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("frontend stdin");
        for entry in &request.entries {
            let line = render::display_line(entry, render_images).0;
            writeln!(stdin, "{line}")?;
        }
        stdin.flush()?;
        drop(stdin); // EOF lets the menu render
    }

    let mut selected = String::new();
    if let Some(stdout) = child.stdout.as_mut() {
        BufReader::new(stdout).read_line(&mut selected)?;
    }
    let status = child.wait()?;

    if selected.trim().is_empty() || !status.success() {
        // Esc / dismissed / killed: never an error.
        return Ok(ShowResponse::Dismissed);
    }

    match render::parse_selection(&selected) {
        Some(Selection::Id(id)) => Ok(ShowResponse::Selected { id }),
        // Frontends do not emit delete/clear yet; treat unknown as dismissed
        // so a malformed echo can never destroy history.
        _ => Ok(ShowResponse::Dismissed),
    }
}

fn read_show_request() -> Result<ShowRequest> {
    let mut buf = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buf)
        .context("reading show request")?;
    serde_json::from_slice(&buf).context("parsing show request")
}
