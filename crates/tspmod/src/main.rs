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
mod polkit;
mod session;
mod state;
mod watch;

use std::sync::{Arc, Mutex};
use tspmo_sys::SysRoot;

fn main() {
    // `tspmod --restore-limits`, run by systemd when the daemon stops: Eco+'s power caps must
    // not outlive the daemon that set them.
    if std::env::args().nth(1).as_deref() == Some("--restore-limits") {
        state::restore_saved(&SysRoot::from_env());
        return;
    }
    if let Err(e) = run() {
        eprintln!("tspmod: {e}");
        std::process::exit(1);
    }
}

fn run() -> zbus::Result<()> {
    let root = SysRoot::from_env();
    let asus = asus::init(&root);
    let mut daemon = state::Daemon::new(root);
    // Tune the machine before anyone can ask: the profile, then the GPU policy.
    daemon.apply(true);
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

    // The display listens before the watchers can publish anything.
    display::start(conn.inner(), &screen);
    watch::all(&conn, &shared);
    // The GPU policy waits for the boot to settle (see boot.rs).
    boot::start(conn.inner(), &shared);
    // Entering or leaving Eco+ changes what the GPU policy wants (Eco+ is dGPU off). The event
    // fires under the power daemon's lock: hand off to a thread.
    {
        let (c, s) = (conn.inner().clone(), Arc::clone(&shared));
        let last = std::sync::Mutex::new(false);
        events::subscribe(move |e| {
            if let events::Event::ProfileApplied { eco_plus, .. } = e {
                let mut l = last.lock().unwrap_or_else(|p| p.into_inner());
                if *l != *eco_plus {
                    *l = *eco_plus;
                    let (c, s) = (c.clone(), Arc::clone(&s));
                    let _ = std::thread::Builder::new()
                        .name("eco-plus-gpu".into())
                        .spawn(move || gpu::enforce(&c, &s));
                }
            }
        });
    }

    // All work happens on the D-Bus executor and the watcher threads.
    loop {
        std::thread::park();
    }
}
