//! Chord playback through the virtual keyboard.

use crate::connection::{Keyboard, STEP_MS};
use anyhow::Result;
use cliphistory_module_common::chord::{self, State};

/// xkb modifier mask for the chord (`Shift = 1 << 0`).
const MOD_MASK: u32 = chord::MOD_MASK;

/// Replay the paste chord into whatever surface currently has keyboard
/// focus: Shift down (with a matching `modifiers` request), tap Insert,
/// Shift up. Every step is followed by a roundtrip so delivery is
/// synchronous and compositor-side rejections surface as errors.
pub fn play(kb: &mut Keyboard) -> Result<()> {
    let t = |n: u64| n as u32; // wrapping ms timestamps are fine per spec
    let base = kb.elapsed_ms();

    kb.keyboard().key(
        t(base),
        chord::key::LEFTSHIFT as u32,
        State::Press.wayland_value(),
    );
    kb.keyboard().modifiers(MOD_MASK, 0, 0, 0);
    kb.sync("modifier press")?;
    std::thread::sleep(std::time::Duration::from_millis(STEP_MS));

    kb.keyboard().key(
        t(base + STEP_MS),
        chord::key::INSERT as u32,
        State::Press.wayland_value(),
    );
    kb.sync("key press")?;
    std::thread::sleep(std::time::Duration::from_millis(STEP_MS));
    kb.keyboard().key(
        t(base + 2 * STEP_MS),
        chord::key::INSERT as u32,
        State::Release.wayland_value(),
    );
    kb.sync("key release")?;
    std::thread::sleep(std::time::Duration::from_millis(STEP_MS));

    kb.keyboard().key(
        t(base + 3 * STEP_MS),
        chord::key::LEFTSHIFT as u32,
        State::Release.wayland_value(),
    );
    kb.keyboard().modifiers(0, 0, 0, 0);
    kb.sync("modifier release")?;
    Ok(())
}
