//! Keeping the dGPU's power state out of the boot.
//!
//! tspmod starts before the display manager (the compositor environment has to be in place before
//! KWin starts), but the NVIDIA driver is still being brought up then. On CachyOS a generator
//! makes `drm-module-load.target`, which waits for `nvidia_drm` and sends the machine to the
//! emergency target if it takes more than 60 s. Powering the dGPU off in that window (Eco on
//! battery) removed the device under the loading driver: "No NVIDIA GPU found", timeout,
//! emergency mode.
//!
//! So until the boot's display stack has settled the daemon applies profiles and the compositor
//! environment but leaves the GPU policy alone ([`allowed`]). It then waits for systemd's own
//! signals (no timer polling): the awaiter target if it exists, else the display manager, else
//! `graphical.target`, with an upper bound. After that the normal live switch runs.

use crate::dbus::Shared;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tspmo_sys::SysRoot;

/// The most the daemon waits for the boot to settle before it stops waiting for systemd (the
/// NVIDIA-driver rule in [`ready`] still applies afterwards).
pub const BOUND: Duration = Duration::from_secs(300);

static SETTLED: AtomicBool = AtomicBool::new(false);

/// Has the boot settled? Until then no GPU power state change is allowed.
pub fn settled() -> bool {
    SETTLED.load(Ordering::SeqCst)
}

/// `ActiveState` of a unit, or `None` when systemd has no such unit loaded.
pub type State = Option<String>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Units {
    /// `drm-module-load.target`, created by CachyOS's awaiter generator when an NVIDIA GPU exists.
    pub awaiter: State,
    pub display_manager: State,
    pub graphical: State,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Nvidia {
    /// An NVIDIA GPU is on the PCI bus.
    pub present: bool,
    /// `nvidia` is loaded.
    pub module_loaded: bool,
    /// `nvidia_drm` is loaded.
    pub drm_loaded: bool,
}

pub fn nvidia(root: &SysRoot) -> Nvidia {
    Nvidia {
        present: !tspmo_sys::pci::nvidia_addresses(root).is_empty(),
        module_loaded: root.exists("/sys/module/nvidia"),
        drm_loaded: root.exists("/sys/module/nvidia_drm"),
    }
}

impl Nvidia {
    /// The driver is on its way and `nvidia_drm` has not appeared yet: changing the GPU's power
    /// state now would pull the device out from under it.
    fn still_loading(self, awaiter_exists: bool) -> bool {
        self.present && !self.drm_loaded && (awaiter_exists || self.module_loaded)
    }
}

fn is(state: &State, wanted: &[&str]) -> bool {
    state.as_deref().is_some_and(|s| wanted.contains(&s))
}

/// Can the GPU policy be applied? Pure, so the whole decision is tested without systemd.
pub fn ready(units: &Units, nv: Nvidia, timed_out: bool) -> bool {
    if nv.still_loading(units.awaiter.is_some()) {
        return false; // never, not even after the bound
    }
    if timed_out {
        return true;
    }
    if units.awaiter.is_some() {
        // `failed` means the machine is already heading for the emergency target: not our job.
        return is(&units.awaiter, &["active", "failed"]);
    }
    if units.display_manager.is_some() {
        return is(&units.display_manager, &["active"]);
    }
    if units.graphical.is_some() {
        return is(&units.graphical, &["active"]);
    }
    true
}

/// May the GPU's power state change now? Only once the boot has settled ([`ready`] holds that
/// back while the NVIDIA driver is still loading).
pub fn allowed() -> bool {
    #[cfg(test)]
    if TEST_ALLOWED.with(std::cell::Cell::get) {
        return true;
    }
    settled()
}

#[cfg(test)]
thread_local! {
    /// A test that exercises a settled system sets this on its own thread.
    pub static TEST_ALLOWED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

const SYSTEMD: &str = "org.freedesktop.systemd1";
const PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
const WATCHED: [&str; 3] = [
    "drm-module-load.target",
    "display-manager.service",
    "graphical.target",
];

/// Ask systemd for the three units' states (one call; `not-found` units are `None`).
pub fn read_units(conn: &zbus::blocking::Connection) -> Option<Units> {
    let proxy = zbus::blocking::Proxy::new(conn, SYSTEMD, PATH, MANAGER).ok()?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        String,
        zbus::zvariant::OwnedObjectPath,
        u32,
        String,
        zbus::zvariant::OwnedObjectPath,
    )> = proxy.call("ListUnitsByNames", &(WATCHED.to_vec(),)).ok()?;
    let state = |name: &str| -> State {
        rows.iter()
            .find(|r| r.0 == name && r.2 != "not-found")
            .map(|r| r.3.clone())
    };
    Some(Units {
        awaiter: state(WATCHED[0]),
        display_manager: state(WATCHED[1]),
        graphical: state(WATCHED[2]),
    })
}

struct Watch {
    bus: zbus::blocking::Connection,
    root: SysRoot,
    shared: Shared,
    timed_out: AtomicBool,
}

impl Watch {
    /// Settle if the boot has. Returns whether it is settled.
    fn try_finish(&self) -> bool {
        if settled() {
            return true;
        }
        // No systemd to ask (a development bus): nothing to wait for.
        let units = read_units(&self.bus).unwrap_or_default();
        if !ready(&units, nvidia(&self.root), self.timed_out.load(Ordering::SeqCst)) {
            return false;
        }
        if !SETTLED.swap(true, Ordering::SeqCst) {
            eprintln!("tspmod: boot settled; applying the GPU policy");
            crate::gpu::enforce(self.bus.inner(), &self.shared);
        }
        true
    }
}

/// Wait, on systemd's signals, until [`ready`]; then allow GPU changes and apply the policy once.
/// Runs on its own threads and ends for good as soon as the boot has settled.
pub fn start(conn: &zbus::Connection, shared: &Shared) {
    let root = shared.lock().unwrap_or_else(|p| p.into_inner()).root.clone();
    let w = std::sync::Arc::new(Watch {
        bus: zbus::blocking::Connection::from(conn.clone()),
        root,
        shared: shared.clone(),
        timed_out: AtomicBool::new(false),
    });
    let w1 = w.clone();
    let _ = std::thread::Builder::new()
        .name("boot-settle".into())
        .spawn(move || {
            let proxy = zbus::blocking::Proxy::new(&w1.bus, SYSTEMD, PATH, MANAGER).ok();
            if let Some(p) = &proxy {
                let _ = p.call_method("Subscribe", &());
            }
            let signals = proxy.as_ref().and_then(|p| p.receive_signal("JobRemoved").ok());
            if !w1.try_finish() {
                for _ in signals.into_iter().flatten() {
                    if w1.try_finish() {
                        break;
                    }
                }
            }
            if let Some(p) = &proxy {
                let _ = p.call_method("Unsubscribe", &());
            }
        });
    // The upper bound: one sleep, not a poll.
    if !settled() {
        let _ = std::thread::Builder::new()
            .name("boot-bound".into())
            .spawn(move || {
                std::thread::sleep(BOUND);
                w.timed_out.store(true, Ordering::SeqCst);
                w.try_finish();
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> State {
        Some(v.to_owned())
    }

    const NO_NV: Nvidia = Nvidia {
        present: false,
        module_loaded: false,
        drm_loaded: false,
    };
    const LOADED: Nvidia = Nvidia {
        present: true,
        module_loaded: true,
        drm_loaded: true,
    };

    #[test]
    fn waits_for_the_awaiter_target_when_there_is_one() {
        let mut u = Units {
            awaiter: s("activating"),
            display_manager: s("inactive"),
            graphical: s("inactive"),
        };
        assert!(!ready(&u, LOADED, false));
        u.awaiter = s("active");
        assert!(ready(&u, LOADED, false));
        // Failed: the machine is going to emergency anyway; the daemon doesn't make it worse.
        u.awaiter = s("failed");
        assert!(ready(&u, LOADED, false));
    }

    #[test]
    fn without_the_target_the_display_manager_decides_then_graphical() {
        let mut u = Units {
            awaiter: None,
            display_manager: s("inactive"),
            graphical: s("active"),
        };
        assert!(!ready(&u, NO_NV, false)); // display manager present and not up yet
        u.display_manager = s("active");
        assert!(ready(&u, NO_NV, false));
        let u = Units {
            awaiter: None,
            display_manager: None,
            graphical: s("activating"),
        };
        assert!(!ready(&u, NO_NV, false));
        assert!(ready(&Units::default(), NO_NV, false)); // nothing to wait for
    }

    #[test]
    fn the_bound_ends_the_wait_for_systemd_but_not_for_the_driver() {
        let early = Units {
            awaiter: s("activating"),
            display_manager: s("inactive"),
            graphical: s("inactive"),
        };
        assert!(ready(&early, NO_NV, true));
        // The driver is loading (nvidia is in, nvidia_drm is not): not even after the bound.
        let loading = Nvidia {
            present: true,
            module_loaded: true,
            drm_loaded: false,
        };
        assert!(!ready(&early, loading, true));
        let u = Units {
            awaiter: s("active"),
            ..Units::default()
        };
        assert!(!ready(&u, loading, false));
    }

    #[test]
    fn an_awaiter_target_with_the_driver_not_yet_in_means_still_loading() {
        // The target exists because an NVIDIA GPU was there at generator time; nothing loaded yet.
        let early_boot = Nvidia {
            present: true,
            module_loaded: false,
            drm_loaded: false,
        };
        let u = Units {
            awaiter: s("active"),
            ..Units::default()
        };
        assert!(!ready(&u, early_boot, false));
        // No target and no module: a machine where the driver isn't used.
        assert!(ready(&Units::default(), early_boot, false));
        // The GPU is already off the bus (Eco): nothing to protect.
        assert!(ready(&u, NO_NV, false));
    }

    #[test]
    fn gpu_changes_are_refused_until_the_boot_has_settled() {
        // (No test marks the boot settled: the flag is process-wide.)
        assert!(!allowed());
        let st = tspmo_core::gpu::Status {
            switcher: tspmo_core::gpu::Switcher::None,
            mode: tspmo_core::gpu::Mode::Hybrid,
            pending: None,
            dgpu: Some("0000:01:00.0".into()),
            dgpu_asleep: false,
        };
        let root = SysRoot::new("/nonexistent");
        for target in [tspmo_core::gpu::Mode::Eco, tspmo_core::gpu::Mode::Hybrid] {
            assert_eq!(
                crate::gpu::precheck(&root, &st, target),
                Err(crate::gpu::STILL_STARTING.to_owned())
            );
        }
    }
}
