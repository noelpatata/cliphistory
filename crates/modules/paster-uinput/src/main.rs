//! uinput auto-paste module.
//!
//! Creates a kernel-level virtual keyboard via `/dev/uinput` and replays
//! Ctrl+V through the input subsystem. This bypasses the display server
//! entirely, so it works on Wayland, X11 and TTY alike — and is immune to
//! compositor-side virtual-keyboard quirks. Requires write access to
//! `/dev/uinput` (logind grants it to the active seat by default).
//!
//! Speaks the paster NDJSON protocol from [`cliphistory_proto`]: reads
//! [`HostToPaster`] frames on stdin and reports [`PasterToHost`] frames on
//! stdout.

use anyhow::{Context, Result};
use cliphistory_proto::{
    HostToPaster, ModuleKind, ModuleManifest, PasterToHost, PROTOCOL_VERSION,
};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::mem::size_of_val;
use std::time::{Duration, Instant};
use std::{io::BufRead, path::Path};

/// True when any process currently has `target` open.
fn reader_exists(target: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.bytes().all(|b| b.is_ascii_digit()) || name == std::process::id().to_string() {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(dest) = std::fs::read_link(fd.path()) {
                if dest.as_os_str() == Path::new(target).as_os_str() {
                    return true;
                }
            }
        }
    }
    false
}

/// Linux event codes.
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const SYN_REPORT: u16 = 0;
const KEY_LEFTCTRL: u16 = 29;
const KEY_V: u16 = 47;
/// Extra chord keys some debug/diagnostic paths need.
const KEY_LEFTMETA: u16 = 125;
const KEY_PRESS: i32 = 1;
const KEY_RELEASE: i32 = 0;

// ioctl request numbers per /usr/include/linux/uinput.h
// (_IOC(dir=3bits, type='U', nr, size=4bytes for int args)).
const UI_SET_EVBIT: u64 = _ioc(1, b'U', 100, 4);
const UI_SET_KEYBIT: u64 = _ioc(1, b'U', 101, 4);
const UI_DEV_SETUP: u64 = _ioc(1, b'U', 3, UINPUT_SETUP_LEN);
const UI_DEV_CREATE: u64 = _ioc(0, b'U', 1, 0);
const UI_DEV_DESTROY: u64 = _ioc(0, b'U', 2, 0);
const UINPUT_SETUP_LEN: u64 = 92; // input_id(8) + name[80] + ff_effects_max(4)

const fn _ioc(dir: u64, ty: u8, nr: u8, size: u64) -> u64 {
    (dir << 30) | (size << 16) | ((ty as u64) << 8) | nr as u64
}

/// `ioctl`'s request parameter is `unsigned long` on glibc but plain `int`
/// on musl; all our encoded requests fit in either.
fn ioctl_req(req: u64) -> IoctlReq {
    req as IoctlReq
}

#[cfg(target_env = "musl")]
type IoctlReq = libc::c_int;
#[cfg(not(target_env = "musl"))]
type IoctlReq = libc::c_ulong;

/// Fixed-kernel-ABI `struct input_event` (x86_64/aarch64 layout).
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct InputEvent {
    sec: u64,
    usec: u64,
    type_: u16,
    code: u16,
    value: i32,
}

/// Mirrors `struct uinput_setup`.
#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

/// Settling time between injected events; applications debounce rapid
/// synthetic input, so keep the chord human-paced.
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
            "Replays the paste shortcut through a kernel uinput keyboard (no compositor support needed)"
                .into(),
    }
}

const MODULE_ID: &str = "paster-uinput";

struct Injector {
    dev: File,
}

impl Drop for Injector {
    fn drop(&mut self) {
        // SAFETY: no argument; destroys this process's uinput device.
        unsafe { libc::ioctl(self.dev.as_raw_fd(), ioctl_req(UI_DEV_DESTROY)) };
    }
}

impl Injector {
    /// Create the virtual keyboard device exposing only Ctrl and V.
    fn create() -> Result<Self> {
        let dev = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("opening /dev/uinput (is this an active logind session?)")?;
        let fd = dev.as_raw_fd();

        let set_bit = |req: u64, bit: u16| {
            // SAFETY: UI_SET_*BIT takes an int by value; fd is a live uinput device.
            unsafe { libc::ioctl(fd, ioctl_req(req), bit as libc::c_int) }
        };
        if set_bit(UI_SET_EVBIT, EV_SYN) < 0 || set_bit(UI_SET_EVBIT, EV_KEY) < 0 {
            anyhow::bail!("UI_SET_EVBIT failed");
        }
        for key in [KEY_LEFTCTRL, KEY_V, KEY_LEFTMETA] {
            if set_bit(UI_SET_KEYBIT, key) < 0 {
                anyhow::bail!("UI_SET_KEYBIT({key}) failed");
            }
        }

        let mut setup = UinputSetup {
            bustype: 0x06, // BUS_VIRTUAL
            vendor: 0x1a52,
            product: 0x0001,
            version: 1,
            name: [0u8; 80],
            ff_effects_max: 0,
        };
        let name = b"cliphistory-paste-keyboard";
        setup.name[..name.len()].copy_from_slice(name);

        // SAFETY: setup is a fully initialised uinput_setup matching the ABI.
        if unsafe { libc::ioctl(fd, ioctl_req(UI_DEV_SETUP), &setup) } < 0 {
            anyhow::bail!("UI_DEV_SETUP failed");
        }
        // SAFETY: no argument; creates the device.
        if unsafe { libc::ioctl(fd, ioctl_req(UI_DEV_CREATE)) } < 0 {
            anyhow::bail!("UI_DEV_CREATE failed");
        }
        let injector = Self { dev };
        injector.wait_for_reader()?;
        Ok(injector)
    }

    /// Block until a display-server process has opened our event node.
    ///
    /// evdev does not replay history: events written before the compositor
    /// opens the node are silently lost, so the very first paste after
    /// startup would vanish without this.
    fn wait_for_reader(&self) -> Result<()> {
        let evdev_name = self.query_event_node()?;
        let target = format!("/dev/input/{evdev_name}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if reader_exists(&target) {
                log::debug!("{target} opened by compositor");
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Compositor may legitimately never open it (headless); don't fail
        // hard, paste attempts will just be no-ops.
        log::warn!("no reader appeared for {target} within 5s");
        Ok(())
    }

    /// Ask uinput which `eventN` node this device got via UI_GET_SYSNAME.
    fn query_event_node(&self) -> Result<String> {
        let mut buf = [0u8; 32];
        // UI_GET_SYSNAME(len) = _IOC(_IOC_READ=2, 'U', 44, len).
        const UI_GET_SYSNAME: u64 = _ioc(2, b'U', 44, 32);
        // SAFETY: buf is writable and matches the encoded length.
        let n = unsafe {
            libc::ioctl(
                self.dev.as_raw_fd(),
                ioctl_req(UI_GET_SYSNAME),
                buf.as_mut_ptr(),
            )
        };
        if n < 0 {
            anyhow::bail!("UI_GET_SYSNAME failed");
        }
        Ok(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    }

    fn emit(&mut self, type_: u16, code: u16, value: i32) -> Result<()> {
        let ev = InputEvent {
            type_,
            code,
            value,
            ..Default::default()
        };
        self.write_event(&ev)
    }

    /// Replay Ctrl+V into the focused window of the current session.
    fn paste(&mut self) -> Result<()> {
        // Debug hook: PASTER_KEY=<evdev code> injects a single plain key.
        if let Some(code) = std::env::var("PASTER_KEY").ok().and_then(|s| s.parse().ok()) {
            self.emit(EV_KEY, code, KEY_PRESS)?;
            self.sync()?;
            std::thread::sleep(Duration::from_millis(INJECT_STEP_MS));
            self.emit(EV_KEY, code, KEY_RELEASE)?;
            self.sync()?;
            return Ok(());
        }
        // Debug hook: PASTER_CHORD=1 injects Super+V (fires the user's
        // cliphistory bind if synthetic input works at all).
        if std::env::var_os("PASTER_CHORD").is_some() {
            for (code, state) in [
                (KEY_LEFTMETA, KEY_PRESS),
                (KEY_V, KEY_PRESS),
                (KEY_V, KEY_RELEASE),
                (KEY_LEFTMETA, KEY_RELEASE),
            ] {
                self.emit(EV_KEY, code, state)?;
                self.sync()?;
                std::thread::sleep(Duration::from_millis(INJECT_STEP_MS));
            }
            return Ok(());
        }
        let step = Duration::from_millis(INJECT_STEP_MS);
        self.emit(EV_KEY, KEY_LEFTCTRL, KEY_PRESS)
            .context("ctrl press")?;
        self.sync()?;
        std::thread::sleep(step);
        self.emit(EV_KEY, KEY_V, KEY_PRESS).context("v press")?;
        self.sync()?;
        std::thread::sleep(step);
        self.emit(EV_KEY, KEY_V, KEY_RELEASE).context("v release")?;
        self.sync()?;
        std::thread::sleep(step);
        self.emit(EV_KEY, KEY_LEFTCTRL, KEY_RELEASE)
            .context("ctrl release")?;
        self.sync()?;
        Ok(())
    }

    /// Terminate the event frame with SYN_REPORT.
    fn sync(&mut self) -> Result<()> {
        let ev = InputEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
            ..Default::default()
        };
        self.write_event(&ev)?;
        self.dev.flush()?;
        Ok(())
    }

    fn write_event(&mut self, ev: &InputEvent) -> Result<()> {
        // SAFETY: reading size_of::<InputEvent>() bytes from a repr(C) struct.
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts((ev as *const InputEvent).cast::<u8>(), size_of_val(ev))
        };
        self.dev.write_all(bytes)?;
        Ok(())
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
    let mut injector = Injector::create()?;
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
                if let Err(e) = injector.paste() {
                    log_frame_error(&format!("paste failed: {e:#}"));
                } else {
                    { log::debug!("injected ctrl+v via uinput"); }
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
