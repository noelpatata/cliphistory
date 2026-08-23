//! Discovery report assembly: ties session, distro, tool probing and module
//! ranking together into one snapshot.

use super::distro::{detect_distro, DistroInfo};
use super::ranking::{probe_requirements, rank_candidates, InstalledInfo};
use super::session::{detect_session, SessionType, RealEnv};
use crate::config::Config;
use crate::constants as c;
use anyhow::Result;
use cliphistory_proto::ModuleKind;
use std::fmt;

#[derive(Clone, Debug)]
pub struct DiscoveryReport {
    pub session: SessionType,
    pub distro: Option<DistroInfo>,
    pub tools: Vec<(String, bool)>,
    pub readers: Vec<String>,
    pub frontends: Vec<String>,
    pub pasters: Vec<String>,
}

impl fmt::Display for DiscoveryReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "session:   {}", self.session)?;
        if let Some(d) = &self.distro {
            writeln!(f, "distro:    {d}")?;
        } else {
            writeln!(f, "distro:    <unknown>")?;
        }
        writeln!(f, "tools:")?;
        for (name, found) in &self.tools {
            writeln!(
                f,
                "  {:<12} {}",
                name,
                if *found { "found" } else { "missing" }
            )?;
        }
        writeln!(f, "clipboards: {}", self.readers.join(" > "))?;
        writeln!(f, "frontends:  {}", self.frontends.join(" > "))?;
        write!(f, "pasters:    {}", self.pasters.join(" > "))
    }
}

/// Run full discovery against the real system.
pub fn discover(cfg: &Config, installed: &[InstalledInfo]) -> Result<DiscoveryReport> {
    let session = detect_session(&RealEnv);
    let distro = detect_distro();

    let relevant = installed.to_vec();
    let mut tools: Vec<(String, bool)> = Vec::new();
    let mut push_tools = |reqs: &[(String, bool)]| {
        for (name, ok) in reqs {
            if !tools.iter().any(|(n, _)| n == name) {
                tools.push((name.clone(), *ok));
            }
        }
    };

    let reader_cands = session.clipboard_candidates().to_vec();
    let readers = rank_candidates(
        &reader_cands,
        &relevant,
        cfg.discovery.preferred_clipboard.as_deref(),
        ModuleKind::Clipboard,
    );
    let frontends = rank_candidates(
        c::FRONTEND_CANDIDATES,
        &relevant,
        cfg.discovery.preferred_frontend.as_deref(),
        ModuleKind::Frontend,
    );
    let pasters = rank_candidates(
        session.paster_candidates(),
        &relevant,
        cfg.discovery.preferred_paster.as_deref(),
        ModuleKind::Paster,
    );

    for m in &relevant {
        let (_, statuses) = probe_requirements(&m.manifest.requires);
        push_tools(&statuses);
    }

    Ok(DiscoveryReport {
        session,
        distro,
        tools,
        readers,
        frontends,
        pasters,
    })
}
