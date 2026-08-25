//! Key binding resolution for the picker.
//!
//! Bindings come from the daemon's `ViewOptions.keys` (configured in
//! `[frontend.keys]` in config.toml). Each value is a key name
//! (lowercase), optionally prefixed with `ctrl+` — e.g. `"enter"`,
//! `"ctrl+delete"`, `"p"`. Unknown names fall back to the built-in
//! default so a typo never disables an action.

use cliphistory_proto::KeyBindings;
use egui::{Context, Key};

/// A parsed key plus whether Ctrl must be held for it to count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub key: Key,
    pub ctrl: bool,
}

/// All resolved bindings, ready to use in the render loop.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Resolved {
    pub move_up: Binding,
    pub move_down: Binding,
    pub confirm: Binding,
    pub dismiss: Binding,
    pub delete_entry: Binding,
    pub clear_all: Binding,
    pub toggle_pin: Binding,
    pub action_next: Binding,
    pub action_prev: Binding,
}

/// Resolve configured key names into concrete bindings. Unparsable names
/// silently fall back to the defaults (logged by the caller if needed).
pub(crate) fn resolve(config: &KeyBindings) -> Resolved {
    Resolved {
        move_up: parse_or_default(&config.move_up, Key::ArrowUp, false),
        move_down: parse_or_default(&config.move_down, Key::ArrowDown, false),
        confirm: parse_or_default(&config.confirm, Key::Enter, false),
        dismiss: parse_or_default(&config.dismiss, Key::Escape, false),
        delete_entry: parse_or_default(&config.delete_entry, Key::Delete, false),
        clear_all: parse_or_default(&config.clear_all, Key::Delete, true),
        toggle_pin: parse_or_default(&config.toggle_pin, Key::P, true),
        action_next: parse_or_default(&config.action_next, Key::ArrowRight, false),
        action_prev: parse_or_default(&config.action_prev, Key::ArrowLeft, false),
    }
}

/// True when `binding` was pressed this frame.
pub(crate) fn pressed(ctx: &Context, binding: &Binding) -> bool {
    ctx.input(|i| i.key_pressed(binding.key) && i.modifiers.ctrl == binding.ctrl)
}

/// Parse a binding string; falls back to `(fallback_key, fallback_ctrl)`
/// when the name is not recognised.
fn parse_or_default(raw: &str, fallback_key: Key, fallback_ctrl: bool) -> Binding {
    parse(raw).unwrap_or(Binding {
        key: fallback_key,
        ctrl: fallback_ctrl,
    })
}

/// Parse a key name like `"ctrl+delete"` or `"p"` into a [`Binding`].
fn parse(raw: &str) -> Option<Binding> {
    let raw = raw.trim().to_lowercase();
    if raw.is_empty() {
        return None;
    }
    let (ctrl, key_part) = match raw.strip_prefix("ctrl+") {
        Some(rest) => (true, rest),
        None => (false, raw.as_str()),
    };
    let key = match key_part {
        "enter" => Key::Enter,
        "escape" => Key::Escape,
        "arrow_up" | "up" => Key::ArrowUp,
        "arrow_down" | "down" => Key::ArrowDown,
        "arrow_right" | "right" => Key::ArrowRight,
        "arrow_left" | "left" => Key::ArrowLeft,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "backspace" => Key::Backspace,
        "home" => Key::Home,
        "end" => Key::End,
        "page_up" => Key::PageUp,
        "page_down" => Key::PageDown,
        // Fall through to egui's own name parser for single letters
        // ("a" → Key::A) and any other recognised names.
        other => Key::from_name(other)?,
    };
    Some(Binding { key, ctrl })
}
