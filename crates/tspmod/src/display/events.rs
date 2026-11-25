//! When the display acts: on power events, resume and when the desktop appears. Nothing here
//! runs while nothing happens.

use super::{Shared, apply_refresh, dbus, flicker, lock, read_output};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Serialises everything that talks to the compositor, so two events arriving together can't
/// interleave their mode switches.
pub(super) static COMPOSITOR: Mutex<()> = Mutex::new(());

/// Run `f` with the compositor to itself.
pub fn exclusive<T>(f: impl FnOnce() -> T) -> T {
    let _g = COMPOSITOR.lock().unwrap_or_else(|p| p.into_inner());
    f()
}

/// Bring the screen in line with the settings. Safe to repeat.
pub fn apply_all(conn: &zbus::Connection, shared: &Shared) {
    let _g = COMPOSITOR.lock().unwrap_or_else(|p| p.into_inner());
    lock(shared).message.clear();
    lock(shared).color.forget_source();
    let note = |what: &str, r: Result<(), String>| {
        if let Err(e) = r {
            eprintln!("tspmod: display {what}: {e}");
            lock(shared).message = e;
        }
    };
    note("refresh", apply_refresh(shared));
    note("brightness", flicker::reapply(shared));
    dbus::emit_changed_blocking(conn, shared);
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(f);
}

/// Wire the display into the daemon: react to power events and to the desktop appearing.
pub fn start(conn: &zbus::Connection, shared: &Shared) {
    {
        let (conn, shared) = (conn.clone(), Arc::clone(shared));
        crate::events::subscribe(move |e| {
            use crate::events::Event;
            // Handlers run under the power daemon's lock: only copy values and hand off.
            let (battery, eco) = match e {
                Event::ProfileApplied {
                    on_battery, eco_plus, ..
                } => (Some(*on_battery), Some(*eco_plus)),
                Event::PowerSource { on_battery } => (Some(*on_battery), None),
                Event::Resumed => (None, None),
                Event::Key(_) | Event::FanKey(_) | Event::ThermalModeWritten { .. } => return,
            };
            let (conn, shared) = (conn.clone(), Arc::clone(&shared));
            spawn("display-event", move || {
                {
                    let mut s = lock(&shared);
                    if let Some(b) = battery {
                        s.on_battery = b;
                    }
                    if let Some(e) = eco {
                        s.eco_plus = e;
                    }
                }
                apply_all(&conn, &shared);
            });
        });
    }
    flicker::watch(conn.clone(), Arc::clone(shared));
    // A desktop that is already running (daemon restart) or that starts later.
    let (conn, shared) = (conn.clone(), Arc::clone(shared));
    spawn("display-session", move || {
        wait_for_desktop(&shared);
        apply_all(&conn, &shared);
        for () in new_sessions() {
            wait_for_desktop(&shared);
            apply_all(&conn, &shared);
        }
    });
}

/// logind's `SessionNew` signals, until the connection ends.
fn new_sessions() -> impl Iterator<Item = ()> {
    let it = (|| {
        let conn = zbus::blocking::Connection::system().ok()?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )
        .ok()?;
        let sig = proxy.receive_signal("SessionNew").ok()?;
        Some((conn, sig))
    })();
    it.into_iter().flat_map(|(_conn, sig)| sig.map(|_| ()))
}

/// Wait (at most a minute, only right after login) until the compositor answers.
fn wait_for_desktop(shared: &Shared) {
    let root = lock(shared).root.clone();
    for _ in 0..30 {
        if read_output(&root, None).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
