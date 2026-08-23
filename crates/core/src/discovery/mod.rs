//! System discovery: which graphics session is running, which external tools
//! exist, which distro we're on — and how to rank candidate modules.
//!
//! Split by concern:
//! * [`session`]  — environment abstraction + session detection
//! * [`distro`]   — os-release parsing and install hints
//! * [`ranking`]  — tool probing and candidate ordering
//! * [`report`]   — the assembled [`DiscoveryReport`]

pub(crate) mod distro;
pub(crate) mod ranking;
pub(crate) mod report;
pub(crate) mod session;

pub use distro::{detect_distro, parse_os_release, DistroInfo};
pub use ranking::{probe_requirements, probe_tool, rank_candidates, InstalledInfo};
pub use report::{discover, DiscoveryReport};
pub use session::{detect_session, EnvSource, RealEnv, SessionType};

/// In-memory env for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MapEnv(pub std::collections::BTreeMap<String, String>);

#[cfg(test)]
impl super::discovery::EnvSource for MapEnv {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}
