//! Every key binding of the picker, in one place.
//!
//! Bindings are constants so remapping is a one-line change and the rest
//! of the UI never hardcodes key literals. Modifier combos are expressed
//! as [`Binding`]s; plain keys are matched exactly (Ctrl+key never fires a
//! binding declared without the modifier).

use egui::{Context, Key};

/// A key plus whether Ctrl must be held for it to count.
#[derive(Clone, Copy)]
pub struct Binding {
    pub key: Key,
    pub ctrl: bool,
}

const fn plain(key: Key) -> Binding {
    Binding { key, ctrl: false }
}

const fn ctrl(key: Key) -> Binding {
    Binding { key, ctrl: true }
}

/// Move the row selection up.
pub const MOVE_UP: Binding = plain(Key::ArrowUp);
/// Move the row selection down.
pub const MOVE_DOWN: Binding = plain(Key::ArrowDown);
/// Activate whatever is focused: paste the entry, or run the focused
/// per-row action.
pub const CONFIRM: Binding = plain(Key::Enter);
/// Close the picker (backs out of per-row action focus first).
pub const DISMISS: Binding = plain(Key::Escape);
/// Delete the selected entry.
pub const DELETE_ENTRY: Binding = plain(Key::Delete);
/// Clear every unpinned entry (a second press within the confirm window
/// executes).
pub const CLEAR_ALL: Binding = ctrl(Key::Delete);
/// Toggle the pin on the selected entry. Ctrl is required because plain
/// keystrokes always land in the filter box.
pub const TOGGLE_PIN: Binding = ctrl(Key::P);
/// Move focus from the row onto its per-row actions (pin first).
pub const ACTION_NEXT: Binding = plain(Key::ArrowRight);
/// Move focus back from the per-row actions towards the row.
pub const ACTION_PREV: Binding = plain(Key::ArrowLeft);

/// True when `binding` was pressed this frame.
pub fn pressed(ctx: &Context, binding: &Binding) -> bool {
    ctx.input(|i| i.key_pressed(binding.key) && i.modifiers.ctrl == binding.ctrl)
}
