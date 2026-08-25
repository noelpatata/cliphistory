//! The paste chord replayed by every paster backend.
//!
//! One fixed chord: **Shift+Insert**. Honored natively by GTK (hardcoded as
//! `paste-clipboard`), Chromium's editor (Google Docs included),
//! VS Code/Electron, and terminal emulators (where it traditionally means
//! "paste selection"; cliphistory therefore claims the PRIMARY selection
//! alongside the clipboard).
//!
//! Apps that need something else are covered by the daemon's
//! `general.paste_command` override — this crate stays single-purpose.

/// Linux evdev key codes used by the chord.
pub mod key {
    pub const LEFTSHIFT: u16 = 42;
    pub const INSERT: u16 = 110;
}

/// Key event state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum State {
    Press,
    Release,
}

impl State {
    /// Value used by uinput's `EV_KEY` events.
    pub fn uinput_value(self) -> i32 {
        match self {
            State::Press => 1,
            State::Release => 0,
        }
    }
}

/// One evdev key event of the chord.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: u16,
    pub state: State,
}

fn press(code: u16) -> KeyEvent {
    KeyEvent {
        code,
        state: State::Press,
    }
}

fn release(code: u16) -> KeyEvent {
    KeyEvent {
        code,
        state: State::Release,
    }
}

/// Ordered events realizing the chord: modifiers down, key tap, modifiers up.
pub fn events() -> Vec<KeyEvent> {
    use key::*;
    vec![
        press(LEFTSHIFT),
        press(INSERT),
        release(INSERT),
        release(LEFTSHIFT),
    ]
}

/// Every distinct evdev code appearing in the chord — backends use this to
/// declare capabilities up front (uinput keybits, XKB keymaps).
pub fn all_codes() -> &'static [u16] {
    &[key::LEFTSHIFT, key::INSERT]
}

/// xkb modifier mask held while the tapped key is down (`Shift = 1 << 0`),
/// for Wayland `modifiers` requests.
pub const MOD_MASK: u32 = 1 << 0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_sequence_is_shift_tap_insert_shift() {
        let evs = events();
        assert_eq!(evs.len(), 4);
        assert_eq!(evs[0], press(key::LEFTSHIFT));
        assert_eq!(evs[1], press(key::INSERT));
        assert_eq!(evs[2], release(key::INSERT));
        assert_eq!(evs[3], release(key::LEFTSHIFT));
    }

    #[test]
    fn all_codes_covers_the_chord() {
        for ev in events() {
            assert!(all_codes().contains(&ev.code));
        }
    }
}
