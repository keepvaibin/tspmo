//! ASUS hardware control: fans, power limits, battery, firmware toggles, lighting, lid
//! displays and hotkeys. Each part is a D-Bus interface at `/org/tspmo`, present only if the
//! machine has the hardware. Parts talk to the rest of the daemon through `events` and
//! `session::osd`, and never block on anything but the kernel.

mod battery;
mod fans;
mod fnlock;
mod hardware;
mod hotkeys;
mod lid;
mod lighting;
mod power;
mod thermal;

use crate::{events, polkit, session};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, mpsc};
use tspmo_core::power::Profile;
use tspmo_sys::SysRoot;
use zbus::blocking::connection::Builder;
use zbus::message::Header;

pub const PATH: &str = "/org/tspmo";

pub const SET_FANS: &str = "org.tspmo.set-fans";
pub const SET_POWER: &str = "org.tspmo.set-power";
pub const SET_BATTERY: &str = "org.tspmo.set-battery";
pub const SET_HARDWARE: &str = "org.tspmo.set-hardware";
pub const SET_LIGHTING: &str = "org.tspmo.set-lighting";
pub const SET_LID: &str = "org.tspmo.set-lid";

pub(crate) async fn authorize(
    conn: &zbus::Connection,
    hdr: &Header<'_>,
    action: &str,
) -> zbus::fdo::Result<()> {
    let sender = hdr.sender().map(|s| s.to_string());
    polkit::check(conn, sender.as_deref(), action).await
}

pub(crate) fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name(name.into())
        .stack_size(256 * 1024)
        .spawn(f);
}

fn state_dir() -> PathBuf {
    std::env::var_os("STATE_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/tspmo"))
}

/// What this machine has. Every part is optional.
pub struct Asus {
    root: SysRoot,
    lighting: Option<Arc<lighting::Lighting>>,
    lid: Option<Arc<lid::Lid>>,
    thermal: Option<Arc<thermal::Thermal>>,
    battery: Option<Arc<battery::Battery>>,
    hardware: Option<Arc<hardware::Hardware>>,
    conn: OnceLock<zbus::blocking::Connection>,
}

impl Asus {
    fn emit(&self, f: impl AsyncFn(&zbus::Connection)) {
        if let Some(c) = self.conn.get() {
            zbus::block_on(f(c.inner()));
        }
    }

    /// The Aura key: next (or previous) effect, or the next brightness on keyboards without effects.
    fn aura_key(&self, forward: bool) {
        let Some(l) = &self.lighting else { return };
        match l.cycle_effect(forward) {
            Ok(name) => session::osd("input-keyboard", name, None),
            Err(_) => {
                let level = l.cycle_brightness();
                let pct = (u32::from(level) * 100 / u32::from(l.max().max(1))) as u8;
                session::osd("input-keyboard", "Keyboard", Some(pct));
            }
        }
        self.emit(lighting::changed);
    }

    fn handle(&self, e: &events::Event) {
        use events::Event::*;
        match e {
            ProfileApplied {
                profile,
                on_battery,
                eco_plus,
            } => {
                if let Some(l) = &self.lighting {
                    l.set_eco(*eco_plus);
                    hotkeys::rearm();
                }
                if let Some(l) = &self.lid {
                    l.set_eco(*eco_plus);
                }
                if let Some(t) = &self.thermal {
                    t.set_eco(*eco_plus);
                    t.on_profile(*profile, *on_battery);
                    self.emit(async |c| {
                        fans::changed(c).await;
                        power::changed(c).await;
                    });
                }
            }
            PowerSource { on_battery } => {
                if let Some(l) = &self.lid {
                    l.on_power_source(*on_battery);
                }
                if let Some(l) = &self.lighting {
                    l.on_power_source(*on_battery);
                    self.emit(lighting::changed);
                }
                if let Some(b) = &self.battery {
                    b.on_power_source(*on_battery);
                    self.emit(battery::changed);
                }
            }
            Resumed => {
                if let Some(t) = &self.thermal {
                    t.on_resume();
                    self.emit(async |c| {
                        fans::changed(c).await;
                        power::changed(c).await;
                    });
                }
                if let Some(h) = &self.hardware {
                    h.reapply();
                    self.emit(hardware::changed);
                }
                if let Some(b) = &self.battery {
                    b.refresh();
                    self.emit(battery::changed);
                }
                if let Some(l) = &self.lighting {
                    l.apply();
                    self.emit(lighting::changed);
                }
                if let Some(l) = &self.lid {
                    l.apply();
                }
                // The USB keyboard may have re-enumerated while asleep.
                hotkeys::rearm();
            }
            Key(_) => {}
        }
    }
}

impl Asus {
    fn build(root: &SysRoot, dir: PathBuf) -> Self {
        Self {
            root: root.clone(),
            lighting: lighting::Lighting::new(root.clone(), dir.clone()).map(Arc::new),
            lid: lid::Lid::new(root.clone(), dir.clone()).map(Arc::new),
            thermal: thermal::Thermal::new(root.clone(), dir.clone()).map(Arc::new),
            battery: battery::Battery::new(root.clone(), dir.clone()).map(Arc::new),
            hardware: hardware::Hardware::new(root.clone(), dir).map(Arc::new),
            conn: OnceLock::new(),
        }
    }
}

/// Discover the hardware and subscribe to events. Call before anything publishes events.
pub fn init(root: &SysRoot) -> Arc<Asus> {
    let asus = Arc::new(Asus::build(root, state_dir()));
    // One worker thread blocks on the channel; subscribers (which run on the publisher's
    // thread, often under the daemon lock) only enqueue.
    let (tx, rx) = mpsc::channel::<events::Event>();
    events::subscribe(move |e| {
        let _ = tx.send(e.clone());
    });
    let a = Arc::clone(&asus);
    spawn("asus-events", move || {
        for e in rx {
            a.handle(&e);
        }
    });
    asus
}

/// Add the interfaces this machine supports to the bus connection.
pub fn register<'a>(mut b: Builder<'a>, asus: &Asus) -> zbus::Result<Builder<'a>> {
    if let Some(t) = &asus.thermal {
        if t.fans.is_some() {
            b = b.serve_at(PATH, fans::Fans1(Arc::clone(t)))?;
        }
        if !lock_limits_empty(t) {
            b = b.serve_at(PATH, power::Power1(Arc::clone(t)))?;
        }
    }
    if let Some(bat) = &asus.battery {
        b = b.serve_at(PATH, battery::Battery1(Arc::clone(bat)))?;
    }
    if let Some(h) = &asus.hardware {
        b = b.serve_at(PATH, hardware::Hardware1(Arc::clone(h)))?;
    }
    if let Some(l) = &asus.lighting {
        b = b.serve_at(PATH, lighting::Lighting1(Arc::clone(l)))?;
    }
    if let Some(l) = &asus.lid {
        b = b.serve_at(PATH, lid::Lid1(Arc::clone(l)))?;
    }
    Ok(b)
}

fn lock_limits_empty(t: &thermal::Thermal) -> bool {
    thermal::lock(&t.state).limits.is_empty()
}

/// The bus is up: start the event sources and apply saved settings.
pub fn start(conn: &zbus::blocking::Connection, asus: &Arc<Asus>) {
    let _ = asus.conn.set(conn.clone());
    if let Some(b) = &asus.battery {
        b.startup();
        battery::watch(conn.clone(), Arc::clone(b));
    }
    if let Some(h) = &asus.hardware {
        h.reapply();
        spawn("asus-fnlock-refresh", fnlock::refresh_if_stale);
    }
    if let Some(l) = &asus.lighting {
        l.apply();
    }
    if let Some(l) = &asus.lid {
        l.apply();
    }
    hotkeys::start(Arc::clone(asus));
}
