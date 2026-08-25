//! Event-driven clipboard change detection over the Wayland data-control
//! protocols.
//!
//! The compositor pushes `selection` / `primary_selection` events whenever
//! clipboard ownership changes, so a thread blocked in `blocking_dispatch`
//! costs **zero** CPU while nothing happens — no polling. Two protocol
//! flavours are supported:
//!
//! * `zwlr-data-control-unstable-v1` — wlroots-family compositors (Hyprland,
//!   Sway, …)
//! * `ext-data-control-unstable-v1` — newer standard (Plasma 6+)
//!
//! This mirrors what `wl-clipboard-rs` does internally for reads/writes,
//! but its wrappers are private, so this minimal watcher talks to the
//! protocols directly via `wayland-client`.
//!
//! When neither manager exists the spawn reports failure and the module
//! falls back to timed sampling.

use std::sync::mpsc::Sender;

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{event_created_child, Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1, ext_data_control_device_v1::ExtDataControlDeviceV1,
    ext_data_control_manager_v1::ExtDataControlManagerV1, ext_data_control_offer_v1,
    ext_data_control_offer_v1::ExtDataControlOfferV1,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1, zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1, zwlr_data_control_offer_v1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
};

use crate::Wake;

/// Globals discovered from the registry.
#[derive(Default)]
struct WatchState {
    seat: Option<wl_seat::WlSeat>,
    wlr_manager: Option<ZwlrDataControlManagerV1>,
    ext_manager: Option<ExtDataControlManagerV1>,
    /// Set by device events signalling a clipboard/primary change.
    changed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for WatchState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" => {
                    if state.seat.is_none() {
                        state.seat =
                            Some(registry.bind::<wl_seat::WlSeat, (), Self>(name, 1, qh, ()));
                    }
                }
                "zwlr_data_control_manager_v1" => {
                    state.wlr_manager = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "ext_data_control_manager_v1" => {
                    state.ext_manager = Some(registry.bind(name, version.min(1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

/// Both device flavours just flip the changed flag; the sampling side
/// (unchanged code path) figures out what the new content is.
impl Dispatch<ZwlrDataControlDeviceV1, ()> for WatchState {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(
            event,
            zwlr_data_control_device_v1::Event::Selection { .. }
                | zwlr_data_control_device_v1::Event::PrimarySelection { .. }
        ) {
            state.changed = true;
        }
    }

    // `data_offer` events create child offer proxies; without this
    // specialization wayland-client panics on delivery (observed as a
    // non-unwinding abort in the module).
    event_created_child!(WatchState, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (
            ZwlrDataControlOfferV1,
            ()
        )
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for WatchState {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlOfferV1,
        _: zwlr_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Mime-type announcements; the change signal comes from the
        // device-level selection events.
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for WatchState {
    fn event(
        state: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The ext protocol has no primary-selection event; selection is
        // enough for our purposes.
        if matches!(event, ext_data_control_device_v1::Event::Selection { .. }) {
            state.changed = true;
        }
    }

    event_created_child!(WatchState, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (
            ExtDataControlOfferV1,
            ()
        )
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for WatchState {
    fn event(
        _: &mut Self,
        _: &ExtDataControlOfferV1,
        _: ext_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for WatchState {
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

impl Dispatch<ZwlrDataControlManagerV1, ()> for WatchState {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlManagerV1,
        _: <ZwlrDataControlManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlManagerV1, ()> for WatchState {
    fn event(
        _: &mut Self,
        _: &ExtDataControlManagerV1,
        _: <ExtDataControlManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Connect, bind the best available data-control manager, and watch.
///
/// Sends one unit per observed clipboard change until the connection dies
/// or the receiver goes away. Returns `false` when there is no usable
/// data-control manager or no Wayland connection (caller falls back to
/// polling).
pub(crate) fn spawn(tx: Sender<Wake>) -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };

    let mut queue = conn.new_event_queue::<WatchState>();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());

    let mut state = WatchState::default();
    // Roundtrip 1: announce globals; roundtrip 2: create the devices bound
    // during announcement.
    if queue.roundtrip(&mut state).is_err() || queue.roundtrip(&mut state).is_err() {
        return false;
    }

    let Some(seat) = state.seat.clone() else {
        return false;
    };
    // Prefer the standardised ext protocol; fall back to the wlr one.
    match (&state.ext_manager, &state.wlr_manager) {
        (Some(manager), _) => {
            manager.get_data_device(&seat, &qh, ());
        }
        (None, Some(manager)) => {
            manager.get_data_device(&seat, &qh, ());
        }
        (None, None) => {
            log::debug!("no data-control manager; clipboard watcher unavailable");
            return false;
        }
    }
    // Receive the initial selection event so `changed` reflects reality
    // before the loop hands control to dispatch.
    if queue.roundtrip(&mut state).is_err() {
        return false;
    }

    std::thread::Builder::new()
        .name("clipboard-watch".into())
        .spawn(move || loop {
            if state.changed {
                state.changed = false;
                if tx.send(Wake::Changed).is_err() {
                    break; // picker/reader gone; nothing left to wake.
                }
            }
            // Blocks until events arrive: zero CPU while idle.
            if queue.blocking_dispatch(&mut state).is_err() {
                break; // compositor connection lost.
            }
        })
        .is_ok()
}
