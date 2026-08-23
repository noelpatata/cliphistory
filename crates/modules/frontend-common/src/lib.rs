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
    let (lines, selections) = render::SelectionMap::build(&request.entries, render_images);

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
        for line in &lines {
            writeln!(stdin, "{}", line.0)?;
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

    match selections.resolve(&selected) {
        Some(id) => Ok(ShowResponse::Selected { id }),
        // Unknown echo (menu mangled the line): dismiss rather than risk
        // acting on the wrong entry.
        None => Ok(ShowResponse::Dismissed),
    }
}

fn read_show_request() -> Result<ShowRequest> {
    let mut buf = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buf)
        .context("reading show request")?;
    serde_json::from_slice(&buf).context("parsing show request")
}

// ---------------------------------------------------------------------------
// Thin-frontend scaffolding
// ---------------------------------------------------------------------------

/// Static description of a dmenu-style frontend; everything a thin
/// frontend binary needs besides [`menu_main`].
pub struct MenuSpec {
    /// Module id, e.g. `frontend-rofi`.
    pub id: &'static str,
    /// Resolves the menu binary at run time (allows env overrides).
    pub bin: fn() -> String,
    /// Declared dependency in the manifest.
    pub default_bin: &'static str,
    /// Arguments always passed to the menu binary.
    pub fixed_args: &'static [&'static str],
    /// Whether thumbnails are rendered (declares the `images` feature).
    pub images: bool,
    /// One-line manifest description.
    pub description: &'static str,
}

/// Entry point for dmenu-style frontends: handles `--manifest`, otherwise
/// runs the picker and prints the answer as JSON.
pub fn menu_main(spec: MenuSpec) -> std::process::ExitCode {
    use cliphistory_proto::{ModuleKind, ModuleManifest, PROTOCOL_VERSION};

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--manifest") {
        let manifest = ModuleManifest {
            id: spec.id.into(),
            kind: ModuleKind::Frontend,
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec![],
            features: if spec.images {
                vec![cliphistory_proto::FEATURE_IMAGES.to_string()]
            } else {
                vec![]
            },
            requires: vec![spec.default_bin.into()],
            description: spec.description.into(),
        };
        return match print_manifest(&manifest) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e:#}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    match run_menu(
        &(spec.bin)(),
        spec.fixed_args,
        &args,
        spec.images,
    ) {
        Ok(resp) => match serde_json::to_string(&resp) {
            Ok(json) => {
                println!("{json}");
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: serialize response: {e}");
                std::process::ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
