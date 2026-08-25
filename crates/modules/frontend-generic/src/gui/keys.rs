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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_returns_none() {
        assert!(parse("").is_none());
        assert!(parse("   ").is_none());
    }

    #[test]
    fn parse_single_letter() {
        let b = parse("p").unwrap();
        assert_eq!(b.key, Key::P);
        assert!(!b.ctrl);
    }

    #[test]
    fn parse_ctrl_combo() {
        let b = parse("ctrl+delete").unwrap();
        assert_eq!(b.key, Key::Delete);
        assert!(b.ctrl);
    }

    #[test]
    fn parse_named_keys() {
        assert_eq!(parse("enter").unwrap().key, Key::Enter);
        assert_eq!(parse("escape").unwrap().key, Key::Escape);
        assert_eq!(parse("arrow_up").unwrap().key, Key::ArrowUp);
        assert_eq!(parse("up").unwrap().key, Key::ArrowUp);
        assert_eq!(parse("arrow_down").unwrap().key, Key::ArrowDown);
        assert_eq!(parse("down").unwrap().key, Key::ArrowDown);
        assert_eq!(parse("arrow_left").unwrap().key, Key::ArrowLeft);
        assert_eq!(parse("arrow_right").unwrap().key, Key::ArrowRight);
        assert_eq!(parse("backspace").unwrap().key, Key::Backspace);
        assert_eq!(parse("home").unwrap().key, Key::Home);
        assert_eq!(parse("end").unwrap().key, Key::End);
        assert_eq!(parse("page_up").unwrap().key, Key::PageUp);
        assert_eq!(parse("page_down").unwrap().key, Key::PageDown);
        assert_eq!(parse("insert").unwrap().key, Key::Insert);
    }

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(parse("Enter").unwrap().key, Key::Enter);
        assert_eq!(parse("CTRL+Delete").unwrap().key, Key::Delete);
        assert!(parse("CTRL+Delete").unwrap().ctrl);
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert!(parse("banana").is_none());
    }

    #[test]
    fn resolve_uses_config_values() {
        let bindings = cliphistory_proto::KeyBindings {
            move_up: "down".into(),
            move_down: "up".into(),
            confirm: "escape".into(),
            dismiss: "enter".into(),
            delete_entry: "backspace".into(),
            clear_all: "p".into(),
            toggle_pin: "x".into(),
            action_next: "left".into(),
            action_prev: "right".into(),
        };
        let r = resolve(&bindings);
        assert_eq!(r.move_up.key, Key::ArrowDown);
        assert_eq!(r.move_down.key, Key::ArrowUp);
        assert_eq!(r.confirm.key, Key::Escape);
        assert_eq!(r.dismiss.key, Key::Enter);
        assert_eq!(r.delete_entry.key, Key::Backspace);
        assert_eq!(r.clear_all.key, Key::P);
        assert!(!r.clear_all.ctrl);
        assert_eq!(r.toggle_pin.key, Key::X);
        assert_eq!(r.action_next.key, Key::ArrowLeft);
        assert_eq!(r.action_prev.key, Key::ArrowRight);
    }

    #[test]
    fn resolve_falls_back_on_invalid() {
        let bindings = cliphistory_proto::KeyBindings {
            move_up: "banana".into(),
            ..cliphistory_proto::KeyBindings::default()
        };
        let r = resolve(&bindings);
        assert_eq!(r.move_up.key, Key::ArrowUp);
    }

    #[test]
    fn default_bindings_match_hardcoded_values() {
        let r = resolve(&cliphistory_proto::KeyBindings::default());
        assert_eq!(r.move_up.key, Key::ArrowUp);
        assert_eq!(r.move_down.key, Key::ArrowDown);
        assert_eq!(r.confirm.key, Key::Enter);
        assert_eq!(r.dismiss.key, Key::Escape);
        assert_eq!(r.delete_entry.key, Key::Delete);
        assert_eq!(r.clear_all.key, Key::Delete);
        assert!(r.clear_all.ctrl);
        assert_eq!(r.toggle_pin.key, Key::P);
        assert!(r.toggle_pin.ctrl);
        assert_eq!(r.action_next.key, Key::ArrowRight);
        assert_eq!(r.action_prev.key, Key::ArrowLeft);
    }
}
