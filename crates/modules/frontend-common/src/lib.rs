//! Shared plumbing for cliphistory frontend modules.
//!
//! A frontend reads one [`ShowRequest`] JSON document from stdin, renders a
//! picker using its menu program of choice and prints a single
//! [`ShowResponse`] JSON document on stdout.

use anyhow::{Context, Result};
use cliphistory_proto::{HistoryItem, ModuleManifest, ShowRequest, ShowResponse};
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
/// `render_images`: emit wofi-style `img:` escape segments for entries that
/// carry a thumbnail (only pass `true` for menus that support it).
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
        for line in render_lines(&request.entries, render_images) {
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

    let selected = selected.trim_end_matches(['\n', '\r']).to_string();
    if selected.is_empty() || !status.success() {
        // Esc / dismissed / killed: never an error.
        return Ok(ShowResponse::Dismissed);
    }

    match parse_selection(&selected)? {
        Some(id) => Ok(ShowResponse::Selected { id }),
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

/// `"<id>\t<label>"` lines; control characters flattened so menus show one
/// line per entry. With `images` on, thumbnails are emitted as wofi image
/// escapes (`img:<path> text:<label>`); selection output still starts with
/// the id prefix, so parsing is unaffected.
fn render_lines(entries: &[HistoryItem], images: bool) -> Vec<String> {
    entries
        .iter()
        .map(|e| {
            let flat = e
                .preview
                .replace('\n', "\\n")
                .replace('\r', "")
                .replace('\t', "  ");
            let label = match (images, &e.thumbnail) {
                (true, Some(path)) => format!("img:{path} text:{flat}"),
                (true, None) => format!("text:{flat}"),
                (false, _) => flat,
            };
            format!("{}\t{label}", e.id)
        })
        .collect()
}

fn parse_selection(selected: &str) -> Result<Option<i64>> {
    match selected
        .split_once('\t')
        .or_else(|| selected.split_once(' '))
    {
        Some((id, _rest)) => id
            .parse::<i64>()
            .map(Some)
            .with_context(|| format!("cannot parse entry id from selection {selected:.80}")),
        None => selected
            .parse::<i64>()
            .map(Some)
            .with_context(|| format!("cannot parse selection {selected:.80}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_flattened() {
        let items = vec![HistoryItem {
            id: 7,
            kind: "text".into(),
            mime: "text/plain".into(),
            preview: "two\nlines\there".into(),
            size_bytes: 3,
            created_at: 0,
            use_count: 0,
            pinned: false,
            thumbnail: None,
        }];
        assert_eq!(
            render_lines(&items, false),
            vec!["7\ttwo\\nlines  here".to_string()]
        );
    }

    #[test]
    fn image_entries_emit_wofi_escapes_when_enabled() {
        let mk = |id: i64, preview: &str, thumb: Option<&str>| HistoryItem {
            id,
            kind: if thumb.is_some() { "image" } else { "text" }.into(),
            mime: "application/octet-stream".into(),
            preview: preview.into(),
            size_bytes: 1,
            created_at: 0,
            use_count: 0,
            pinned: false,
            thumbnail: thumb.map(str::to_string),
        };
        let items = vec![
            mk(3, "[image image/png 12.0KB]", Some("/thumbs/abc.png")),
            mk(4, "plain", None),
        ];
        assert_eq!(
            render_lines(&items, true),
            vec![
                "3\timg:/thumbs/abc.png text:[image image/png 12.0KB]".to_string(),
                "4\ttext:plain".to_string(),
            ]
        );
        // Without the images feature, output stays plain regardless.
        assert_eq!(
            render_lines(&items, false),
            vec![
                "3\t[image image/png 12.0KB]".to_string(),
                "4\tplain".to_string(),
            ]
        );
    }

    #[test]
    fn parses_id_prefixed_selections() {
        assert_eq!(parse_selection("42\thello world").unwrap(), Some(42));
        assert_eq!(parse_selection("42 hello").unwrap(), Some(42));
        assert_eq!(parse_selection("42").unwrap(), Some(42));
        assert!("x\ty".parse::<i64>().is_err());
    }
}
