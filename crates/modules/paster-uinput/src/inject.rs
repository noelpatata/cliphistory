//! Chord playback on top of the uinput [`Keyboard`].

use crate::device::{Keyboard, STEP};
use anyhow::Result;

/// Replay the paste chord into whatever surface currently has keyboard
/// focus. Each event is terminated by its own EV_SYN frame.
pub fn play(kb: &mut Keyboard) -> Result<()> {
    for event in cliphistory_module_common::chord::events() {
        kb.emit(event.code, event.state)?;
        std::thread::sleep(STEP);
    }
    Ok(())
}
