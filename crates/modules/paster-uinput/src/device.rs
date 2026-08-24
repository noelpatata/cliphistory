//! Safe wrapper around a kernel uinput keyboard device.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

/// Linux event codes / constants (linux/input-event-codes.h, linux/uinput.h).
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const SYN_REPORT: u16 = 0;

// ioctl request numbers, encoded per _IOC(dir=3bits, type='U', nr, size).
const UI_SET_EVBIT: u64 = ioc(1, b'U', 100, 4);
const UI_SET_KEYBIT: u64 = ioc(1, b'U', 101, 4);
const UI_DEV_SETUP: u64 = ioc(1, b'U', 3, UINPUT_SETUP_LEN);
const UI_DEV_CREATE: u64 = ioc(0, b'U', 1, 0);
const UI_DEV_DESTROY: u64 = ioc(0, b'U', 2, 0);
const UI_GET_SYSNAME: u64 = ioc(2, b'U', 44, 32);
const UINPUT_SETUP_LEN: u64 = 92; // input_id(8) + name[80] + ff_effects_max(4)

/// `ioctl`'s request parameter is `unsigned long` on glibc but plain `int`
/// on musl; all our encoded requests fit in either.
fn ioctl_req(req: u64) -> IoctlReq {
    req as IoctlReq
}

#[cfg(target_env = "musl")]
type IoctlReq = libc::c_int;
#[cfg(not(target_env = "musl"))]
type IoctlReq = libc::c_ulong;

const fn ioc(dir: u64, ty: u8, nr: u8, size: u64) -> u64 {
    (dir << 30) | (size << 16) | ((ty as u64) << 8) | nr as u64
}

/// Fixed-kernel-ABI `struct input_event` (x86_64/aarch64 layout).
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RawEvent {
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

/// How long to wait between the events of one chord.
///
/// Measured cost: this is slept once per event, so a 4-event chord spends
/// `4 × STEP` inside playback. 20 ms made every paste carry an 80 ms floor;
/// 5 ms keeps a comfortable margin for apps to register the modifier while
/// cutting that to ~20 ms.
pub const STEP: Duration = Duration::from_millis(5);

/// A virtual keyboard exposing exactly the keys cliphistory needs.
pub struct Keyboard {
    dev: File,
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        // SAFETY: no argument; destroys this process's uinput device.
        unsafe { libc::ioctl(self.dev.as_raw_fd(), ioctl_req(UI_DEV_DESTROY)) };
    }
}

impl Keyboard {
    /// Create the virtual keyboard capable of every chord in
    /// [`cliphistory_module_common::chord::Chord`].
    pub fn create() -> Result<Self> {
        let dev = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("opening /dev/uinput (is this an active logind session?)")?;
        let fd = dev.as_raw_fd();

        let set_bit = |req: u64, bit: u16| {
            // SAFETY: UI_SET_*BIT takes an int by value; fd is a live device.
            unsafe { libc::ioctl(fd, ioctl_req(req), bit as libc::c_int) }
        };
        if set_bit(UI_SET_EVBIT, EV_SYN) < 0 || set_bit(UI_SET_EVBIT, EV_KEY) < 0 {
            anyhow::bail!("UI_SET_EVBIT failed");
        }
        for &code in cliphistory_module_common::chord::all_codes() {
            if set_bit(UI_SET_KEYBIT, code) < 0 {
                anyhow::bail!("UI_SET_KEYBIT({code}) failed");
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

        let kb = Self { dev };
        kb.wait_for_reader()?;
        Ok(kb)
    }

    /// Block until a display-server process has opened our event node.
    ///
    /// evdev does not replay history: events written before the compositor
    /// opens the node are silently lost, so the very first paste after
    /// startup would vanish without this.
    fn wait_for_reader(&self) -> Result<()> {
        let target = format!("/dev/input/{}", self.query_event_node()?);
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
        // SAFETY: buf is writable and matches the encoded length.
        let n = unsafe {
            libc::ioctl(self.dev.as_raw_fd(), ioctl_req(UI_GET_SYSNAME), buf.as_mut_ptr())
        };
        if n < 0 {
            anyhow::bail!("UI_GET_SYSNAME failed");
        }
        Ok(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    }

    /// Send one key event followed by an EV_SYN frame separator.
    pub fn emit(&mut self, code: u16, state: State) -> Result<()> {
        self.write_event(&RawEvent {
            type_: EV_KEY,
            code,
            value: state.uinput_value(),
            ..Default::default()
        })?;
        self.write_event(&RawEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            ..Default::default()
        })?;
        self.dev.flush()?;
        Ok(())
    }

    fn write_event(&mut self, ev: &RawEvent) -> Result<()> {
        // SAFETY: reading size_of::<RawEvent>() bytes from a repr(C) struct.
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts((ev as *const RawEvent).cast::<u8>(), size_of_val(ev))
        };
        self.dev.write_all(bytes)?;
        Ok(())
    }
}

use cliphistory_module_common::chord::State;
use std::mem::size_of_val;

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
                if dest.as_os_str() == std::path::Path::new(target).as_os_str() {
                    return true;
                }
            }
        }
    }
    false
}
