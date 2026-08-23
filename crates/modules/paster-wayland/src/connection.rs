//! Compositor connection: virtual keyboard creation and keymap upload.

use crate::vk;
use vk::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use vk::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use anyhow::{Context, Result};
use std::io::Write;
use std::os::fd::AsFd;
use std::time::Instant;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};

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

/// Minimal self-contained XKB keymap covering every key any chord may tap.
/// Codes are evdev+8 per XKB convention. Compositors that ignore a client
/// keymap fall back to their stock layout, which maps these identically.
const KEYMAP: &str = r#"xkb_keymap {
	xkb_keycodes "cliphistory" {
		minimum = 8;
		maximum = 255;
		<LCTL> = 37;
		<LFSH> = 42;
		<V> = 55;
		<INS> = 118;
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
		key <LFSH> { [ Shift_L ] };
		modifier_map Shift { <LFSH> };
		key <V> { [ v, V ] };
		key <INS> { [ Insert ] };
	};
};
"#;

/// The chord's keys must exist in the uploaded keymap.
pub struct Keyboard {
    queue: wayland_client::EventQueue<State>,
    kb: ZwpVirtualKeyboardV1,
    started: Instant,
}

impl Keyboard {
    /// Connect to the compositor, create a virtual keyboard on the first
    /// seat, and upload our keymap.
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().context("connecting to Wayland compositor")?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).context("querying compositor globals")?;
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

    pub fn keyboard(&self) -> &ZwpVirtualKeyboardV1 {
        &self.kb
    }

    /// Flush pending requests and wait for the compositor to acknowledge so
    /// delivery is synchronous and rejections surface as errors.
    pub fn sync(&mut self, what: &'static str) -> Result<()> {
        self.queue
            .roundtrip(&mut State)
            .with_context(|| format!("syncing {what}"))?;
        Ok(())
    }

    /// Milliseconds since connect — timestamp base for `key` requests.
    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

/// Keymap format sent to `keymap`: 1 == XKB text v1.
pub const XKB_FORMAT_V1: u32 = 1;

/// Settling time between injected events; compositors deliver serially but
/// some applications debounce rapid synthetic input.
pub const STEP_MS: u64 = 20;
