//! Wayland paster: replays Ctrl+V into the focused surface.
//!
//! Uses `zwp_virtual_keyboard_v1` directly — no external tools. The
//! compositor forwards the synthesized keystrokes to whatever surface has
//! keyboard focus, exactly as if the user had pressed them.

use anyhow::{Context, Result};
use cliphistory_proto::{HostToPaster, ModuleKind, ModuleManifest, PasterToHost, PROTOCOL_VERSION};
use std::io::Write as _;
use std::os::unix::io::AsFd as _;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard::KeyState, wl_keyboard::KeymapFormat, wl_registry, wl_seat},
    Connection, Dispatch, QueueHandle,
};

// ---------------------------------------------------------------------------
// Generated protocol bindings (vendored XML, built with wayland-scanner)
// ---------------------------------------------------------------------------

pub mod vk {
    use wayland_client;
    #[allow(unused_imports)]
    use wayland_client::protocol::*;

    #[allow(non_snake_case)]
    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("./virtual-keyboard-unstable-v1.xml");
    }
    #[allow(unused_imports)]
    pub use __interfaces::*;

    wayland_scanner::generate_client_code!("./virtual-keyboard-unstable-v1.xml");
}

use vk::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use vk::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

// ---------------------------------------------------------------------------
// Module identity + tuning constants
// ---------------------------------------------------------------------------

const MODULE_ID: &str = "paster-wayland";

/// Minimal XKB keymap covering Ctrl and V. Includes resolve on the
/// compositor side against xkeyboard-config data files.
const KEYMAP: &str = r#"xkb_keymap {
	xkb_keycodes "cliphistory" { include "evdev" };
	xkb_types    "cliphistory" { include "complete" };
	xkb_compat   "cliphistory" { include "complete" };
	xkb_symbols  "cliphistory" { include "pc+us" };
}"#;

/// evdev keycodes; the protocol expects the XKB convention (evdev + 8).
const KEY_LEFTCTRL: i32 = 29 + 8;
const KEY_V: i32 = 47 + 8;

/// XKB "Control" modifier bitmask for the `modifiers` request.
const MOD_CONTROL: u32 = 1 << 2;

/// Dwell between synthetic transitions so every client observes each edge.
const KEY_DWELL_MS: u64 = 20;

fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: MODULE_ID.into(),
        kind: ModuleKind::Paster,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![],
        requires: vec![],
        features: vec![],
        description: "Injects ctrl+v into the focused Wayland surface (virtual keyboard)".into(),
    }
}

enum Incoming {
    Control(HostToPaster),
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--manifest") {
        return match cliphistory_clipboard_common::print_manifest(&manifest()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e:#}");
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

fn emit(frame: &PasterToHost) -> Result<()> {
    cliphistory_clipboard_common::emit_json(frame)
}

fn log_err(msg: &str) {
    let _ = emit(&PasterToHost::Error {
        message: msg.to_string(),
    });
}

// ---------------------------------------------------------------------------
// Event loop
// ---------------------------------------------------------------------------

/// Long-lived session: connect once, inject on demand.
struct KeyboardSession {
    conn: Connection,
    keyboard: ZwpVirtualKeyboardV1,
}

/// Marker state for all our dispatch impls (no events are acted upon).
struct State;

impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &wayland_client::globals::GlobalListContents,
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
        _: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event,
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
        _: <ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl KeyboardSession {
    fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().context("connecting to compositor")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).context("registry init")?;

        let seat: wl_seat::WlSeat = globals
            .bind(&queue.handle(), 1..=9, ())
            .context("binding wl_seat")?;
        let manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&queue.handle(), 1..=1, ())
            .context("compositor lacks zwp_virtual_keyboard_manager_v1")?;
        queue.roundtrip(&mut State).context("initial roundtrip")?;

        let qh = queue.handle();
        let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
        install_keymap(&keyboard, &conn)?;

        Ok(Self { conn, keyboard })
    }

    /// Synthesize ctrl↓ → v↓ → v↑ → ctrl↑ with modifier updates while
    /// control is held, flushing between edges.
    fn paste_ctrl_v(&self) -> Result<()> {
        self.keyboard.modifiers(MOD_CONTROL, 0, 0, 0);
        self.keyboard.key(millis(), KEY_LEFTCTRL, KEY_STATE_PRESS);
        self.flush()?;
        dwell();

        self.keyboard.key(millis(), KEY_V, KEY_STATE_PRESS);
        self.flush()?;
        dwell();

        self.keyboard.key(millis(), KEY_V, KEY_STATE_RELEASE);
        self.keyboard.modifiers(0, 0, 0, 0);
        self.keyboard.key(millis(), KEY_LEFTCTRL, KEY_STATE_RELEASE);
        self.flush()
    }

    fn flush(&self) -> Result<()> {
        self.conn.flush().context("flushing wayland connection")
    }
}

const KEY_STATE_PRESS: KeyState = KeyState::Pressed;
const KEY_STATE_RELEASE: KeyState = KeyState::Released;

fn run() -> Result<()> {
    let (tx, rx) = mpsc::channel::<Incoming>();

    // stdin -> control frames
    {
        use std::io::BufRead;
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("stdin".into())
            .spawn(move || {
                let mut reader = std::io::BufReader::new(std::io::stdin().lock());
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let Ok(frame) = serde_json::from_str::<HostToPaster>(line.trim()) else {
                        continue;
                    };
                    if tx.send(Incoming::Control(frame)).is_err() {
                        break;
                    }
                }
            })
            .context("spawning stdin thread")?;
    }

    // Fail fast when no compositor / no virtual-keyboard support exists.
    let session = KeyboardSession::connect().context("wayland paster unavailable")?;

    emit(&PasterToHost::Ready {
        protocol_version: PROTOCOL_VERSION,
    })?;

    loop {
        match rx.recv() {
            Ok(Incoming::Control(HostToPaster::Ping)) => {
                emit(&PasterToHost::Pong)?;
            }
            Ok(Incoming::Control(HostToPaster::Paste)) => {
                if let Err(e) = session.paste_ctrl_v() {
                    log_err(&format!("paste injection failed: {e:#}"));
                }
            }
            Ok(Incoming::Control(HostToPaster::Stop)) | Err(mpsc::RecvError) => {
                return Ok(());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Keymap upload
// ---------------------------------------------------------------------------

/// Upload [`KEYMAP`] over a pipe fd (format 1 = XKB text map). The file is
/// removed only after the compositor acknowledged the roundtrip.
fn install_keymap(keyboard: &ZwpVirtualKeyboardV1, conn: &Connection) -> Result<()> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let path = dir.join(format!("cliphistory-keymap-{}", std::process::id()));

    let mut file =
        std::fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    file.write_all(KEYMAP.as_bytes())?;
    file.flush()?;
    file.sync_all().ok();

    keyboard.keymap(KeymapFormat::XkbV1, file.as_fd(), KEYMAP.len() as u32);
    conn.flush().context("flushing keymap")?;
    // Give the compositor a chance to consume the fd before we unlink it.
    std::thread::sleep(Duration::from_millis(50));
    let _ = std::fs::remove_file(path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn dwell() {
    std::thread::sleep(Duration::from_millis(KEY_DWELL_MS));
}

fn millis() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(0)
}
