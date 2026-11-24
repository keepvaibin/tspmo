//! Event sources. Each runs on its own thread and blocks in the kernel until something
//! happens: no timers, no polling loops, zero wakeups while idle.

use crate::dbus::{Shared, emit_changed_blocking};
use crate::gpu;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use tspmo_core::power::PLATFORM_PROFILE;
use zbus::blocking::Connection;

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, crate::state::Daemon> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name(name.into())
        // Watchers only parse small messages; keep their stacks small.
        .stack_size(256 * 1024)
        .spawn(f);
}

/// AC plugged/unplugged, from UPower's `OnBattery`.
pub fn power_source(conn: Connection, shared: Shared) {
    spawn("upower", move || {
        let proxy = match zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.UPower",
            "/org/freedesktop/UPower",
            "org.freedesktop.UPower",
        ) {
            Ok(p) => p,
            Err(e) => {
                return eprintln!("tspmod: UPower unavailable ({e}); power-source changes won't be seen");
            }
        };
        if let Ok(on_battery) = proxy.get_property::<bool>("OnBattery") {
            on_change(&conn, &shared, on_battery);
        }
        for change in proxy.receive_property_changed::<bool>("OnBattery") {
            if let Ok(on_battery) = change.get() {
                on_change(&conn, &shared, on_battery);
            }
        }
    });

    fn on_change(conn: &Connection, shared: &Shared, on_battery: bool) {
        let changed = lock(shared).set_on_battery(on_battery);
        if changed {
            emit_changed_blocking(conn, shared);
            gpu::enforce(conn.inner(), shared);
        }
    }
}

/// The firmware changed the thermal mode itself (Fn+F5 on ASUS, Fn+Q on Lenovo).
/// sysfs attributes signal changes with POLLPRI; we sleep in poll(2) until then.
pub fn platform_profile(conn: Connection, shared: Shared) {
    let path = lock(&shared).root.path(PLATFORM_PROFILE);
    let Ok(mut file) = std::fs::File::open(&path) else {
        return;
    };
    spawn("platform-profile", move || {
        use rustix::event::{PollFd, PollFlags, poll};
        let mut buf = String::with_capacity(32);
        loop {
            buf.clear();
            if file.seek(SeekFrom::Start(0)).is_err() || file.read_to_string(&mut buf).is_err() {
                return;
            }
            let value = buf.trim().to_owned();
            let changed = lock(&shared).platform_changed(&value);
            if changed {
                emit_changed_blocking(&conn, &shared);
            }
            let mut fds = [PollFd::new(&file, PollFlags::PRI | PollFlags::ERR)];
            if poll(&mut fds, None).is_err() {
                return;
            }
        }
    });
}

/// Re-apply after resume: some firmware resets the thermal mode across sleep.
pub fn resume(conn: Connection, shared: Shared) {
    spawn("logind", move || {
        let Ok(proxy) = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        ) else {
            return;
        };
        let Ok(signals) = proxy.receive_signal("PrepareForSleep") else {
            return;
        };
        for msg in signals {
            let Ok(going_to_sleep) = msg.body().deserialize::<bool>() else {
                continue;
            };
            if !going_to_sleep {
                let mut d = lock(&shared);
                d.apply(true);
                tspmo_core::usbpm::apply(&d.root);
                drop(d);
                emit_changed_blocking(&conn, &shared);
            }
        }
    });
}

/// Drop profile holds when the app that made them exits or crashes.
pub fn hold_owners(conn: Connection, shared: Shared) {
    spawn("name-owners", move || {
        let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&conn) else {
            return;
        };
        let Ok(changes) = dbus.receive_name_owner_changed() else {
            return;
        };
        for sig in changes {
            let Ok(args) = sig.args() else { continue };
            // A unique name losing its owner means that client disconnected.
            if args.new_owner().is_some() || !args.name().starts_with(':') {
                continue;
            }
            let released = lock(&shared).owner_vanished(args.name());
            if released.is_empty() {
                continue;
            }
            for cookie in released {
                let _ = conn.emit_signal(
                    None::<&str>,
                    crate::dbus::PPD_PATH,
                    crate::dbus::PPD_NAME,
                    "ProfileReleased",
                    &cookie,
                );
            }
            emit_changed_blocking(&conn, &shared);
        }
    });
}

/// A deferred Eco switch happens once the desktop that held the dGPU has gone (logout).
pub fn sessions(conn: Connection, shared: Shared) {
    spawn("sessions", move || {
        let Ok(proxy) = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        ) else {
            return;
        };
        let Ok(signals) = proxy.receive_signal("SessionRemoved") else {
            return;
        };
        for _ in signals {
            // Give the compositor a moment to close its device files.
            std::thread::sleep(std::time::Duration::from_millis(500));
            gpu::enforce(conn.inner(), &shared);
        }
    });
}

/// While a policy switch is waiting for an app to release the dGPU, retry every 15 s. The
/// thread exists only while waiting, so an idle system has no timer at all.
pub fn gpu_retry(conn: &zbus::Connection, shared: &Shared) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if !lock(shared).gpu_waiting || RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let (conn, shared) = (conn.clone(), Arc::clone(shared));
    spawn("gpu-retry", move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(15));
            let (waiting, busy) = {
                let d = lock(&shared);
                (d.gpu_waiting, d.gpu_switching.is_some())
            };
            if !waiting {
                break;
            }
            if !busy {
                gpu::enforce(&conn, &shared);
                zbus::block_on(crate::dbus::emit_changed(&conn, &shared));
            }
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
}

pub fn all(conn: &Connection, shared: &Shared) {
    gpu_retry(conn.inner(), shared);
    sessions(conn.clone(), Arc::clone(shared));
    power_source(conn.clone(), Arc::clone(shared));
    platform_profile(conn.clone(), Arc::clone(shared));
    resume(conn.clone(), Arc::clone(shared));
    hold_owners(conn.clone(), Arc::clone(shared));
}
