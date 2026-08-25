//! Event-driven clipboard change detection on X11 via **XFixes**.
//!
//! Core X11 has no change notification, but the XFixes extension (v2+,
//! present on every realistic server) does: registering a window with
//! [`select_selection_input`] makes the server deliver an XFixes
//! `SelectionNotify` whenever *any* client takes ownership of the watched
//! selection. A thread blocked in `wait_for_event()` therefore costs zero
//! CPU while nothing happens — no polling, no subprocess churn.
//!
//! Only detection lives here; reading contents stays with the existing
//! `xclip` plumbing (it handles TARGETS negotiation and INCR transfers).
//! When XFixes is unavailable (ancient/embedded servers) the spawn reports
//! failure and the module falls back to timed sampling.

use std::sync::mpsc::Sender;

use x11rb::atom_manager;
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _, WindowClass};
use x11rb::protocol::Event;

atom_manager! {
    /// Atoms the watcher needs; resolved once per connection.
    WatchAtoms:
    WatchAtomsInit {
        CLIPBOARD,
    }
}

/// Connect to the X server, register for CLIPBOARD ownership changes, and
/// watch. Sends one unit per observed change until the receiver goes away
/// or the connection dies. Returns `false` when there is no X connection
/// or no XFixes ≥ 2 (caller falls back to polling).
pub(crate) fn spawn(tx: Sender<crate::Wake>) -> bool {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return false;
    };
    let root = conn.setup().roots[screen_num].root;

    // SelectSelectionInput needs XFixes 2.0.
    let version = match conn.xfixes_query_version(2, 0) {
        Ok(cookie) => cookie,
        Err(_) => return false,
    }
    .reply();
    let Ok(version) = version else {
        return false;
    };
    if version.major_version < 2 {
        return false;
    }

    let atoms = match WatchAtoms::new(&conn)
        .map_err(|_| ())
        .and_then(|cookie| cookie.reply().map_err(|_| ()))
    {
        Ok(atoms) => atoms,
        Err(_) => return false,
    };

    // An input-only child of the root: invisible, exists purely to receive
    // the XFixes notifications.
    let window: xproto::Window = match conn.generate_id() {
        Ok(id) => id,
        Err(_) => return false,
    };
    if conn
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &Default::default(),
        )
        .is_err()
    {
        return false;
    }
    if conn
        .xfixes_select_selection_input(
            window,
            atoms.CLIPBOARD,
            xfixes::SelectionEventMask::SET_SELECTION_OWNER,
        )
        .is_err()
    {
        return false;
    }
    if conn.flush().is_err() {
        return false;
    }
    std::thread::Builder::new()
        .name("clipboard-watch".into())
        .spawn(move || loop {
            // Blocks until the X server sends anything: zero CPU while
            // idle. Ownership changes arrive as XFixes SelectionNotify.
            match conn.wait_for_event() {
                Ok(Event::XfixesSelectionNotify(_)) => {
                    if tx.send(crate::Wake::Changed).is_err() {
                        break; // reader gone; nothing left to wake.
                    }
                }
                Ok(_) => {}
                Err(_) => break, // X connection lost.
            }
        })
        .is_ok()
}
