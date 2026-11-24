//! tspmod — the tspmo system daemon.
//!
//! Runs as root under systemd. Owns the power-profiles D-Bus API (replacing
//! power-profiles-daemon) and `org.tspmo.Daemon1`.

mod dbus;
mod gpu;
mod polkit;
mod state;
mod watch;

use std::sync::{Arc, Mutex};
use tspmo_sys::SysRoot;

fn main() {
    if let Err(e) = run() {
        eprintln!("tspmod: {e}");
        std::process::exit(1);
    }
}

fn run() -> zbus::Result<()> {
    let root = SysRoot::from_env();
    let mut daemon = state::Daemon::new(root);
    // Tune the machine before anyone can ask: the profile, then the GPU policy.
    daemon.apply(true);
    gpu::enforce_policy(&mut daemon);
    let shared: dbus::Shared = Arc::new(Mutex::new(daemon));

    // TSPMOD_BUS=session runs against the session bus (development, with TSPMO_SYSROOT).
    let builder = match std::env::var("TSPMOD_BUS").as_deref() {
        Ok("session") => zbus::blocking::connection::Builder::session()?,
        _ => zbus::blocking::connection::Builder::system()?,
    };
    let conn = builder
        .serve_at(dbus::PPD_PATH, dbus::PowerProfiles(Arc::clone(&shared)))?
        .serve_at(dbus::HADESS_PATH, dbus::HadessPowerProfiles(Arc::clone(&shared)))?
        .serve_at(dbus::TSPMO_PATH, dbus::Tspmo(Arc::clone(&shared)))?
        .name(dbus::PPD_NAME)?
        .name(dbus::HADESS_NAME)?
        .name(dbus::TSPMO_NAME)?
        .build()
        .map_err(|e| {
            zbus::Error::Failure(format!(
                "{e} (is power-profiles-daemon or tuned-ppd still running? tspmod replaces them)"
            ))
        })?;

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

    watch::all(&conn, &shared);

    // All work happens on the D-Bus executor and the watcher threads.
    loop {
        std::thread::park();
    }
}
