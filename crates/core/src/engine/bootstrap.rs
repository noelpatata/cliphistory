//! Module selection and installation for this machine.
//!
//! Answers two questions at daemon startup:
//! * which installed modules should be active,
//! * which missing ones are worth downloading (using the release
//!   manifest's own metadata, keeping core decoupled from internals).

use super::Shared;
use crate::config::Config;
use crate::constants as c;
use crate::discovery::{self, probe_requirements, probe_tool, InstalledInfo};
use anyhow::Result;
use cliphistory_proto::{ModuleKind, ModuleManifest, PROTOCOL_VERSION};

/// What the daemon wants to end up running, one id per module kind.
#[derive(Debug, Default, Clone)]
pub(crate) struct DesiredModules {
    pub clipboard: Option<String>,
    pub frontend: Option<String>,
    pub paster: Option<String>,
}

/// One module kind's discovery inputs: candidate ids and configured
/// preference.
struct CandidateSource<'a> {
    kind: ModuleKind,
    candidates: &'static [&'static str],
    preferred: Option<&'a str>,
    /// Slot in [`DesiredModules`] the winner is recorded into.
    slot: fn(&mut DesiredModules, String),
}

impl<'a> CandidateSource<'a> {
    fn all(cfg: &'a Config, session: discovery::SessionType) -> [CandidateSource<'a>; 3] {
        [
            Self {
                kind: ModuleKind::Clipboard,
                candidates: session.clipboard_candidates(),
                preferred: cfg.discovery.preferred_clipboard.as_deref(),
                slot: |d, id| d.clipboard = Some(id),
            },
            Self {
                kind: ModuleKind::Frontend,
                candidates: c::FRONTEND_CANDIDATES,
                preferred: cfg.discovery.preferred_frontend.as_deref(),
                slot: |d, id| d.frontend = Some(id),
            },
            Self {
                kind: ModuleKind::Paster,
                // Kernel-level uinput serves every session type.
                candidates: c::PASTER_CANDIDATES,
                preferred: cfg.discovery.preferred_paster.as_deref(),
                slot: |d, id| d.paster = Some(id),
            },
        ]
    }
}

/// Decide which modules this machine should run, downloading anything
/// missing. Returns the validated picks per kind.
pub fn resolve_desired(shared: &mut Shared) -> Result<DesiredModules> {
    let installed = shared.mm.list_installed()?;
    let infos = to_installed_infos(&installed);

    let report = discovery::discover(&shared.cfg, &infos)?;
    log::info!("discovery:\n{report}");

    let mut desired = DesiredModules::default();
    let mut wanted: Vec<String> = Vec::new();

    for source in CandidateSource::all(&shared.cfg, shared.session) {
        // Rank installed modules first…
        let pick =
            discovery::rank_candidates(source.candidates, &infos, source.preferred, source.kind)
                .first()
                .cloned();

        // …and fall back to remote metadata when nothing is installed yet
        // (fresh machine): rank by published `requires` probed on PATH.
        let want = match pick {
            Some(id) => Some(id),
            None => remote_rank(shared, source.candidates, source.preferred, source.kind),
        };

        if let Some(id) = want {
            if !wanted.contains(&id) {
                wanted.push(id.clone());
            }
            (source.slot)(&mut desired, id);
        }
    }

    if !shared.cfg.modules.uses_local_dir() {
        let missing: Vec<String> = wanted
            .iter()
            .filter(|id| shared.mm.resolve(id).is_none())
            .cloned()
            .collect();
        if !missing.is_empty() {
            let root = shared.mm.install_root();
            let _ = std::fs::create_dir_all(root);
            log::info!("installing missing modules: {missing:?}");
            for res in shared
                .mm
                .ensure_available(&missing, false, &|msg| log::info!("{msg}"))
            {
                if let Err(e) = res {
                    log::warn!("install failed: {e:#}");
                }
            }
        }
    }
    Ok(desired)
}

/// Rank not-yet-installed candidates using release-manifest metadata.
/// `None` when the source is unreachable.
fn remote_rank(
    shared: &Shared,
    candidates: &[&str],
    preferred: Option<&str>,
    kind: ModuleKind,
) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    // Explicit config preference short-circuits remote ranking.
    if let Some(p) = preferred {
        return Some(p.to_string());
    }
    let rm = shared.mm.fetch_remote_manifest(None).ok()?;
    let assets = shared.mm.select_target(&rm).ok()?;

    let pseudo: Vec<InstalledInfo> = assets
        .modules
        .iter()
        .map(|m| InstalledInfo {
            manifest: ModuleManifest {
                id: m.id.clone(),
                kind: m.kind,
                version: rm.release.clone(),
                protocol_version: PROTOCOL_VERSION,
                capabilities: m.capabilities.clone(),
                requires: m.requires.clone(),
                features: m.features.clone(),
                description: m.description.clone(),
            },
            requirements_met: m.requires.iter().all(|t| probe_tool(t)),
        })
        .collect();

    discovery::rank_candidates(candidates, &pseudo, None, kind)
        .into_iter()
        .next()
}

pub(crate) fn to_installed_infos(
    installed: &[crate::plugins::InstalledModule],
) -> Vec<InstalledInfo> {
    installed
        .iter()
        .map(|m| {
            let (met, _) = probe_requirements(&m.manifest.requires);
            InstalledInfo {
                manifest: m.manifest.clone(),
                requirements_met: met,
            }
        })
        .collect()
}
