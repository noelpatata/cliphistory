//! Graphics-session detection and the environment abstraction that makes it
//! testable.

use crate::constants as c;
use std::fmt;

/// Source of environment variables; production uses the real environment,
/// tests use in-memory maps.
pub trait EnvSource {
    fn get(&self, name: &str) -> Option<String>;
}

pub struct RealEnv;

impl EnvSource for RealEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionType {
    Wayland,
    X11,
    Tty,
}

impl fmt::Display for SessionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SessionType::Wayland => "wayland",
            SessionType::X11 => "x11",
            SessionType::Tty => "tty",
        })
    }
}

impl SessionType {
    /// Clipboard module ids to try for this kind of session, best first.
    pub fn clipboard_candidates(self) -> &'static [&'static str] {
        match self {
            SessionType::Wayland => c::CLIPBOARD_CANDIDATES_WAYLAND,
            SessionType::X11 => c::CLIPBOARD_CANDIDATES_X11,
            // A TTY can host either once a compositor appears; let tool
            // probing decide instead of hard-coding a preference.
            SessionType::Tty => &[],
        }
    }
}

/// `$XDG_SESSION_TYPE`, falling back to socket/display variable probes.
pub fn detect_session(env: &dyn EnvSource) -> SessionType {
    if let Some(t) = env.get(c::ENV_SESSION_TYPE) {
        match t.to_ascii_lowercase().as_str() {
            "wayland" => return SessionType::Wayland,
            "x11" => return SessionType::X11,
            "tty" => return SessionType::Tty,
            _ => {}
        }
    }
    if env.get(c::ENV_WAYLAND_DISPLAY).is_some() {
        return SessionType::Wayland;
    }
    if env.get(c::ENV_DISPLAY).is_some() {
        return SessionType::X11;
    }
    SessionType::Tty
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::MapEnv;

    #[test]
    fn session_from_env_vars() {
        let mk = |pairs: &[(&str, &str)]| {
            MapEnv(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        };
        assert_eq!(
            detect_session(&mk(&[(c::ENV_SESSION_TYPE, "wayland")])),
            SessionType::Wayland
        );
        assert_eq!(
            detect_session(&mk(&[
                (c::ENV_SESSION_TYPE, "unspecified"),
                (c::ENV_DISPLAY, ":0"),
            ])),
            SessionType::X11
        );
        assert_eq!(
            detect_session(&mk(&[
                (c::ENV_WAYLAND_DISPLAY, "wayland-1"),
                (c::ENV_DISPLAY, ":0"),
            ])),
            SessionType::Wayland
        );
        assert_eq!(detect_session(&MapEnv::default()), SessionType::Tty);
    }
}
