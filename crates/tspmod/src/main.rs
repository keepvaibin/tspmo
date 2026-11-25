//! tspmod — the tspmo system daemon.
//!
//! Runs as root under systemd. Owns the power-profiles D-Bus API (replacing
//! power-profiles-daemon) and `org.tspmo.Daemon1`.

mod asus;
mod boot;
mod dbus;
mod display;
mod events;
mod gpu;
mod guard;
mod polkit;
mod quiet;
mod session;
mod state;
mod watch;

use std::sync::{Arc, Mutex};
use tspmo_sys::SysRoot;

fn main() {
    // `tspmod --restore-limits`, run by systemd when the daemon stops: Eco+'s power caps must
    // not outlive the daemon that set them.
    if std::env::args().nth(1).as_deref() == Some("--restore-limits") {
        let root = SysRoot::from_env();
        let eco = root.exists(state::ECO_SAVED);
        state::restore_saved(&root);
        guard::shutdown(&root);
        asus::restore_on_stop(&root, eco);
        if eco {
            display::restore_on_stop(&root);
        }
        return;
    }
    if let Err(e) = run() {
        eprintln!("tspmod: {e}");
        std::process::exit(1);
    }
}

fn lock_eco(shared: &dbus::Shared) -> bool {
    shared
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .effective()
        .is_eco_plus()
}

fn run() -> zbus::Result<()> {
    let root = SysRoot::from_env();
    let asus = asus::init(&root);
    let mut daemon = state::Daemon::new(root);
    // Tune the machine before anyone can ask: the profile, then the GPU policy.
    daemon.apply(true);
    // Background jobs a previous daemon moved aside: back, unless Eco+ keeps them there.
    if daemon.effective().is_eco_plus() {
        guard::set_active(&daemon.root, true);
    } else {
        guard::shutdown(&daemon.root);
    }
    let n = tspmo_core::usbpm::apply(&daemon.root);
    if n > 0 {
        eprintln!("tspmod: internal USB devices allowed to sleep: {n}");
    }
    daemon.write_compositor_env();
    // The screen starts from the power state the daemon just applied (its event fired before
    // the display could listen).
    let mut screen_state = display::State::new(daemon.root.clone(), daemon.on_battery);
    screen_state.eco_plus = daemon.effective().is_eco_plus();
    let shared: dbus::Shared = Arc::new(Mutex::new(daemon));
    let screen: display::Shared = Arc::new(Mutex::new(screen_state));

    // TSPMOD_BUS=session runs against the session bus (development, with TSPMO_SYSROOT).
    let builder = match std::env::var("TSPMOD_BUS").as_deref() {
        Ok("session") => zbus::blocking::connection::Builder::session()?,
        _ => zbus::blocking::connection::Builder::system()?,
    };
    let builder = builder
        .serve_at(dbus::PPD_PATH, dbus::PowerProfiles(Arc::clone(&shared)))?
        .serve_at(dbus::HADESS_PATH, dbus::HadessPowerProfiles(Arc::clone(&shared)))?
        .serve_at(dbus::TSPMO_PATH, dbus::Tspmo(Arc::clone(&shared)))?
        .serve_at(display::dbus::PATH, display::dbus::Display1(Arc::clone(&screen)))?;
    let conn = asus::register(builder, &asus)?
        .name(dbus::PPD_NAME)?
        .name(dbus::HADESS_NAME)?
        .name(dbus::TSPMO_NAME)?
        .build()
        .map_err(|e| {
            zbus::Error::Failure(format!(
                "{e} (is power-profiles-daemon or tuned-ppd still running? tspmod replaces them)"
            ))
        })?;

    asus::start(&conn, &asus);

    {
        let d = shared.lock().unwrap_or_else(|p| p.into_inner());
        eprintln!(
            "tspmod {} ready: {} on {}, GPU {}",
            env!("CARGO_PKG_VERSION"),
            d.effective().as_str(),
            if d.on_battery { "battery" } else { "AC" },
            d.gpu_status().mode
        );
    }

    // Clients that outlived the previous daemon drop its stale `GpuSwitching`/`GpuMessage`.
    zbus::block_on(dbus::emit_changed(conn.inner(), &shared));

    // Every listener is in place before a watcher or the display can publish anything.
    // Entering or leaving Eco+ changes what the GPU policy wants (Eco+ is dGPU off). The event
    // fires under the power daemon's lock: hand off to a thread.
    {
        let (c, s) = (conn.inner().clone(), Arc::clone(&shared));
        // Starts as the mode the daemon is in: a restart during Eco+ must still see Eco+ end.
        let last = std::sync::Mutex::new(lock_eco(&shared));
        events::subscribe(move |e| {
            if let events::Event::ProfileApplied { eco_plus, .. } = e {
                let mut l = last.lock().unwrap_or_else(|p| p.into_inner());
                if *l != *eco_plus {
                    *l = *eco_plus;
                    guard::set_active(&SysRoot::from_env(), *eco_plus);
                    let (c, s) = (c.clone(), Arc::clone(&s));
                    let _ = std::thread::Builder::new()
                        .name("eco-plus-gpu".into())
                        .spawn(move || gpu::enforce(&c, &s));
                }
            }
        });
    }

    // Fn+F5 with Fn-lock on reaches the keyboard driver as a plain key that nothing cycles:
    // if the kernel did not change the thermal mode, the daemon does.
    {
        let (c, s) = (conn.inner().clone(), Arc::clone(&shared));
        events::subscribe(move |e| {
            if let events::Event::FanKey = e {
                let (c, s) = (c.clone(), Arc::clone(&s));
                let before = SysRoot::from_env().read_opt(tspmo_core::power::PLATFORM_PROFILE);
                let _ = std::thread::Builder::new().name("fan-key".into()).spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let root = SysRoot::from_env();
                    if root.read_opt(tspmo_core::power::PLATFORM_PROFILE) != before {
                        return; // the kernel cycled it; the watcher follows
                    }
                    let next = {
                        let mut d = s.lock().unwrap_or_else(|p| p.into_inner());
                        let next = d.chosen().next();
                        d.choose(next);
                        next
                    };
                    dbus::osd_mode(next);
                    zbus::block_on(dbus::emit_changed(&c, &s));
                });
            }
        });
    }

    // The display listens before the watchers can publish anything.
    display::start(conn.inner(), &screen);
    watch::all(&conn, &shared);
    // The GPU policy waits for the boot to settle (see boot.rs).
    boot::start(conn.inner(), &shared);
    // All work happens on the D-Bus executor and the watcher threads.
    loop {
        std::thread::park();
    }
}
