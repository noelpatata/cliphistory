//! Human-readable reports: `cliphistory doctor`.

use super::Shared;
use crate::discovery;

/// Full diagnostic text: session, discovery, daemon state, auto-paste
/// mechanism, storage and installed modules.
pub(crate) fn doctor_text(st: &Shared) -> String {
    let installed = st.mm.list_installed().unwrap_or_default();
    let infos = super::bootstrap::to_installed_infos(&installed);
    let mut lines = Vec::new();

    match discovery::discover(&st.cfg, &infos) {
        Ok(report) => lines.push(report.to_string()),
        Err(e) => lines.push(format!("discovery failed: {e:#}")),
    }

    lines.push(format!(
        "daemon:     pid {}, up since unix {}",
        std::process::id(),
        st.started_at
    ));
    lines.push(format!(
        "active:     clipboard={} frontend={}",
        st.clipboard_id.read().unwrap(),
        st.frontend_id.read().unwrap()
    ));
    lines.push(auto_paste_line(st));
    match st.storage.count() {
        Ok(n) => {
            let budget = st.cfg.storage.max_total_bytes;
            let used = st.storage.db_size_bytes();
            let tail = if budget > 0 {
                format!(" | payload budget {} MiB", budget / (1024 * 1024))
            } else {
                String::new()
            };
            lines.push(format!(
                "storage:    {n} entries at {} ({} bytes on disk{tail})",
                st.storage.db_path().display(),
                used
            ));
        }
        Err(e) => lines.push(format!("storage:    ERROR {e:#}")),
    }
    if let Ok(items) = super::actions::list_modules(st) {
        lines.push("modules:".into());
        for m in items {
            lines.push(format!(
                "  {:<20} {} [{:?}] {}",
                m.id, m.version, m.kind, m.description
            ));
        }
    }
    lines.join("\n")
}

/// The `auto-paste: on/off [mechanism]` doctor line.
fn auto_paste_line(st: &Shared) -> String {
    let paster_running =
        !st.paster_id.read().unwrap().is_empty() && st.paster_tx.read().unwrap().is_some();

    let mechanism = if st.paste_command_override().is_some() {
        "custom command".into()
    } else if paster_running {
        format!("native module ({})", st.paster_id.read().unwrap())
    } else {
        crate::paste::find_tool(st.session)
            .map(|t| format!("external tool ({t})"))
            .unwrap_or_else(|| "none available".into())
    };

    let enabled = st.cfg.general.auto_paste && mechanism != "none available";
    format!(
        "auto-paste: {}{}",
        if enabled { "on" } else { "off" },
        if st.cfg.general.auto_paste {
            format!(" [{mechanism}]")
        } else {
            String::new()
        },
    )
}
