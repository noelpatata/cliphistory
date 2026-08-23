//! Wayland auto-paste module.
//!
//! Injects Ctrl+V into the focused surface through the compositor's
//! `zwp_virtual_keyboard_v1` interface, so auto-paste needs no external
//! binaries. Speaks the paster NDJSON protocol from [`cliphistory_proto`]:
//! reads [`HostToPaster`] frames on stdin and reports [`PasterToHost`]
//! frames on stdout.

use anyhow::{Context, Result};
use cliphistory_proto::{
    HostToPaster, ModuleKind, ModuleManifest, PasterToHost, PROTOCOL_VERSION,
};
use std::io::{BufRead, Write};
use std::os::fd::AsFd;
use std::time::Instant;

mod vk {
    // Generated code expects these names in scope.
    #[allow(clippy::single_component_path_imports)]
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        pub use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        #[allow(unused_imports)]
        use log;
        wayland_scanner::generate_interfaces!("protocols/wlr-virtual-keyboard-unstable-v1.xml");
    }
    use self::__interfaces::*;

    // Expands to the client-side bindings for our bundled protocol XML.
    wayland_scanner::generate_client_code!("protocols/wlr-virtual-keyboard-unstable-v1.xml");
}

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use vk::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use vk::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

const MODULE_ID: &str = "paster-wayland";

/// Linux evdev keycodes (the `key` request takes them un-offset).
const KEY_LEFTCTRL: u32 = 29;
const KEY_V: u32 = 47;
const XKB_FORMAT_V1: u32 = 1;
const MOD_CONTROL: u32 = 1 << 2;
const KEY_PRESS: u32 = 1;
const KEY_RELEASE: u32 = 0;
/// Settling time between injected events; compositors deliver serially but
/// some applications debounce rapid synthetic input.
const INJECT_STEP_MS: u64 = 20;

fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: MODULE_ID.into(),
        kind: ModuleKind::Paster,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![cliphistory_proto::CAP_WRITE.into()],
        features: vec![],
        requires: vec![],
        description:
            "Replays the paste shortcut via the Wayland virtual keyboard protocol".into(),
    }
}

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: vk::zwp_virtual_keyboard_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: vk::zwp_virtual_keyboard_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Minimal self-contained XKB keymap mapping evdev Ctrl/V onto the default
/// keycode positions (37 / 55). Compositors that ignore a client keymap fall
/// back to their stock layout, which maps those codes identically.
const KEYMAP: &str = r#"xkb_keymap {
	xkb_keycodes "cliphistory" {
		minimum = 8;
		maximum = 255;
		<LCTL> = 37;
		<V> = 55;
	};
	xkb_types "cliphistory" {
		type "ONE_LEVEL" {
			modifiers = none;
			level_name[Level1] = "Any";
			map[None] = Level1;
		};
	};
	xkb_compatibility "cliphistory" {
	};
	xkb_symbols "cliphistory" {
		key <LCTL> { [ Control_L ] };
		modifier_map Control { <LCTL> };
		key <V> { [ v, V ] };
	};
};
"#;

struct Keyboard {
    queue: wayland_client::EventQueue<State>,
    kb: ZwpVirtualKeyboardV1,
    started: Instant,
}

impl Keyboard {
    /// Connect to the compositor, create a virtual keyboard on the first
    /// seat, and upload our keymap.
    fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().context("connecting to Wayland compositor")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)
            .context("querying compositor globals")?;
        let qh = queue.handle();
        // State is a unit struct; dispatch impls are no-ops.
        queue.roundtrip(&mut State).context("initial compositor roundtrip")?;

        let manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .context("compositor does not advertise zwp_virtual_keyboard_manager_v1")?;
        // Cap the negotiated version like wtype does; newer wl_seat versions
        // gain nothing here.
        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 1..=7, ())
            .context("no wl_seat advertised by compositor")?;
        let kb = manager.create_virtual_keyboard(&seat, &qh, ());

        // The keymap travels as a NUL-terminated XKB v1 text blob; the
        // compositor parses it with xkb_keymap_new_from_string, so the
        // terminator is part of the payload.
        let mut keymap_bytes = KEYMAP.as_bytes().to_vec();
        keymap_bytes.push(0);
        let mut keymap_file =
            tempfile::NamedTempFile::new().context("creating keymap scratch file")?;
        keymap_file.write_all(&keymap_bytes)?;
        keymap_file.flush()?;
        let bytes = keymap_bytes.len() as u32;
        kb.keymap(XKB_FORMAT_V1, bytes, keymap_file.as_fd());
        // The file may be unlinked after the roundtrip; the compositor holds
        // its own descriptor.
        queue.roundtrip(&mut State).context("uploading keymap")?;
        Ok(Self {
            queue,
            kb,
            started: Instant::now(),
        })
    }

    /// Replay Ctrl+V into whatever surface currently has keyboard focus.
    /// Every step is followed by a roundtrip so delivery is synchronous and
    /// any compositor-side rejection surfaces as an error instead of being
    /// silently swallowed.
    fn paste(&mut self) -> Result<()> {
        let t = |n: u64| n as u32; // wrapping ms timestamps are fine per spec
        let base = self.elapsed_ms();
        let sleep_step = || std::thread::sleep(std::time::Duration::from_millis(INJECT_STEP_MS));

        self.kb.key(t(base), KEY_LEFTCTRL, KEY_PRESS);
        self.kb.modifiers(MOD_CONTROL, 0, 0, 0);
        self.sync("ctrl press")?;
        sleep_step();

        self.kb.key(t(base + INJECT_STEP_MS), KEY_V, KEY_PRESS);
        self.sync("v press")?;
        sleep_step();

        self.kb.key(t(base + 2 * INJECT_STEP_MS), KEY_V, KEY_RELEASE);
        self.sync("v release")?;
        sleep_step();

        self.kb.key(t(base + 3 * INJECT_STEP_MS), KEY_LEFTCTRL, KEY_RELEASE);
        self.kb.modifiers(0, 0, 0, 0);
        self.sync("ctrl release")?;
        Ok(())
    }

    /// Flush pending requests and wait for the compositor to acknowledge.
    fn sync(&mut self, what: &'static str) -> Result<()> {
        self.queue
            .roundtrip(&mut State)
            .with_context(|| format!("syncing {what}"))?;
        Ok(())
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--manifest") {
        return match serde_json::to_string(&manifest()) {
            Ok(json) => {
                println!("{json}");
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("serialize manifest: {e}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            let _ = emit(&PasterToHost::Error {
                message: format!("{e:#}"),
            });
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut keyboard = Keyboard::connect()?;
    emit(&PasterToHost::Ready {
        protocol_version: PROTOCOL_VERSION,
    })?;

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.context("reading host frame")?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<HostToPaster>(line.trim()) {
            Ok(HostToPaster::Ping) => emit(&PasterToHost::Pong)?,
            Ok(HostToPaster::Paste) => {
                if let Err(e) = keyboard.paste() {
                    log_frame_error(&format!("paste failed: {e:#}"));
                } else {
                    log::debug!("injected ctrl+v");
                }
            }
            Ok(HostToPaster::Stop) => return Ok(()),
            Err(e) => log_frame_error(&format!("unparsable frame: {e}")),
        }
    }
    // Host closed stdin: treat as shutdown.
    Ok(())
}

fn emit(frame: &PasterToHost) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, frame)?;
    lock.write_all(b"\n")?;
    lock.flush()?;
    Ok(())
}

fn log_frame_error(msg: &str) {
    let _ = emit(&PasterToHost::Error {
        message: msg.to_string(),
    });
}
