//! D-Bus interfaces.
//!
//! * `org.freedesktop.UPower.PowerProfiles` at `/org/freedesktop/UPower/PowerProfiles` and its
//!   legacy twin `net.hadess.PowerProfiles` at `/net/hadess/PowerProfiles`: the standard API that
//!   KDE's battery applet, GNOME, `powerprofilesctl` and games (via holds) use. tspmod *is* the
//!   power-profiles daemon, so there is exactly one profile state on the system.
//! * `org.tspmo.Daemon1` at `/org/tspmo`: everything else (per-source profiles, GPU modes, …).

use crate::gpu;
use crate::polkit;
use crate::state::Daemon;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tspmo_core::gpu::{Mode, Policy};
use tspmo_core::power::Profile;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

pub type Shared = Arc<Mutex<Daemon>>;

pub const PPD_PATH: &str = "/org/freedesktop/UPower/PowerProfiles";
pub const PPD_NAME: &str = "org.freedesktop.UPower.PowerProfiles";
pub const HADESS_PATH: &str = "/net/hadess/PowerProfiles";
pub const HADESS_NAME: &str = "net.hadess.PowerProfiles";
pub const TSPMO_PATH: &str = "/org/tspmo";
pub const TSPMO_NAME: &str = "org.tspmo.Daemon1";

/// The power-profiles-daemon API version we implement.
const PPD_VERSION: &str = "0.30";

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, Daemon> {
    // A panic while holding the lock must not take the whole daemon down.
    s.lock().unwrap_or_else(|p| p.into_inner())
}

fn ov(v: impl Into<Value<'static>>) -> OwnedValue {
    v.into().try_into().expect("plain values always convert")
}

fn cpu_driver(d: &Daemon) -> &'static str {
    match d
        .root
        .read_opt("/sys/devices/system/cpu/cpufreq/policy0/scaling_driver")
        .as_deref()
    {
        Some("intel_pstate") => "intel_pstate",
        Some("amd-pstate-epp") | Some("amd-pstate") => "amd_pstate",
        _ => "tspmo",
    }
}

fn profiles(d: &Daemon) -> Vec<HashMap<String, OwnedValue>> {
    let cpu = cpu_driver(d);
    let platform = d.root.exists("/sys/firmware/acpi/platform_profile");
    Profile::ALL
        .iter()
        .map(|p| {
            let mut m = HashMap::new();
            m.insert("Profile".into(), ov(p.as_str()));
            m.insert("Driver".into(), ov("multiple"));
            m.insert("CpuDriver".into(), ov(cpu));
            if platform {
                m.insert("PlatformDriver".into(), ov("platform_profile"));
            }
            m
        })
        .collect()
}

fn holds(d: &Daemon) -> Vec<HashMap<String, OwnedValue>> {
    d.holds
        .iter()
        .map(|h| {
            let mut m = HashMap::new();
            m.insert("ApplicationId".into(), ov(h.app_id.clone()));
            m.insert("Profile".into(), ov(h.profile.as_str()));
            m.insert("Reason".into(), ov(h.reason.clone()));
            m
        })
        .collect()
}

/// Emit `PropertiesChanged` for everything that may have changed, on all three interfaces.
/// Builds the values from `Daemon` directly so it never needs the object server's interface
/// locks (safe to call from inside a handler).
pub async fn emit_changed(conn: &zbus::Connection, shared: &Shared) {
    let (active, hold_list, tspmo) = {
        let d = lock(shared);
        (d.effective().ppd().as_str(), holds(&d), tspmo_props(&d))
    };
    for (path, iface) in [(PPD_PATH, PPD_NAME), (HADESS_PATH, HADESS_NAME)] {
        let Ok(emitter) = SignalEmitter::new(conn, path) else {
            continue;
        };
        let mut changed: HashMap<&str, Value<'_>> = HashMap::new();
        changed.insert("ActiveProfile", Value::from(active));
        changed.insert("ActiveProfileHolds", Value::from(hold_list.clone()));
        let _ = zbus::fdo::Properties::properties_changed(
            &emitter,
            zbus::names::InterfaceName::from_static_str_unchecked(iface),
            changed,
            Cow::Borrowed(&[]),
        )
        .await;
    }
    if let Ok(emitter) = SignalEmitter::new(conn, TSPMO_PATH) {
        let changed: HashMap<&str, Value<'_>> =
            tspmo.iter().map(|(k, v)| (*k, Value::from(v.clone()))).collect();
        let _ = zbus::fdo::Properties::properties_changed(
            &emitter,
            zbus::names::InterfaceName::from_static_str_unchecked(TSPMO_NAME),
            changed,
            Cow::Borrowed(&[]),
        )
        .await;
    }
}

/// Blocking wrapper for watcher threads.
pub fn emit_changed_blocking(conn: &zbus::blocking::Connection, shared: &Shared) {
    zbus::block_on(emit_changed(conn.inner(), shared));
}

// ---------------------------------------------------------------------------------------------
// power-profiles-daemon API (two names, same behaviour)
// ---------------------------------------------------------------------------------------------

macro_rules! ppd_interface {
    ($ty:ident, $name:literal) => {
        pub struct $ty(pub Shared);

        #[zbus::interface(name = $name)]
        impl $ty {
            #[zbus(property)]
            fn active_profile(&self) -> String {
                lock(&self.0).effective().ppd().as_str().to_owned()
            }

            #[zbus(property)]
            async fn set_active_profile(
                &mut self,
                profile: String,
                #[zbus(header)] hdr: Option<Header<'_>>,
                #[zbus(connection)] conn: &zbus::Connection,
            ) -> zbus::fdo::Result<()> {
                let p = Profile::parse(&profile)
                    .filter(|p| !p.is_eco_plus())
                    .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("unknown profile '{profile}'")))?;
                let sender = hdr.as_ref().and_then(|h| h.sender()).map(|s| s.to_string());
                polkit::check(conn, sender.as_deref(), polkit::SWITCH_PROFILE).await?;
                lock(&self.0).choose_ppd(p);
                emit_changed(conn, &self.0).await;
                Ok(())
            }

            #[zbus(property)]
            fn performance_inhibited(&self) -> String {
                String::new()
            }

            #[zbus(property)]
            fn performance_degraded(&self) -> String {
                String::new()
            }

            #[zbus(property)]
            fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
                profiles(&lock(&self.0))
            }

            #[zbus(property)]
            fn actions(&self) -> Vec<String> {
                Vec::new()
            }

            #[zbus(property)]
            fn actions_info(&self) -> Vec<HashMap<String, OwnedValue>> {
                Vec::new()
            }

            #[zbus(property)]
            fn active_profile_holds(&self) -> Vec<HashMap<String, OwnedValue>> {
                holds(&lock(&self.0))
            }

            #[zbus(property)]
            fn version(&self) -> String {
                PPD_VERSION.to_owned()
            }

            /// Battery-aware tuning is always on in tspmod (profiles adapt to the power source).
            #[zbus(property)]
            fn battery_aware(&self) -> bool {
                true
            }

            #[zbus(property)]
            fn set_battery_aware(&mut self, _value: bool) {}

            async fn hold_profile(
                &self,
                profile: String,
                reason: String,
                application_id: String,
                #[zbus(header)] hdr: Header<'_>,
                #[zbus(connection)] conn: &zbus::Connection,
            ) -> zbus::fdo::Result<u32> {
                let p = match Profile::parse(&profile) {
                    Some(p @ (Profile::PowerSaver | Profile::Performance)) => p,
                    _ => {
                        return Err(zbus::fdo::Error::InvalidArgs(
                            "only power-saver and performance can be held".into(),
                        ));
                    }
                };
                let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
                polkit::check(conn, Some(&sender), polkit::HOLD_PROFILE).await?;
                let cookie = lock(&self.0).hold(p, reason, application_id, sender);
                emit_changed(conn, &self.0).await;
                Ok(cookie)
            }

            async fn release_profile(
                &self,
                cookie: u32,
                #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
                #[zbus(connection)] conn: &zbus::Connection,
            ) -> zbus::fdo::Result<()> {
                if !lock(&self.0).release(cookie) {
                    return Err(zbus::fdo::Error::InvalidArgs(format!(
                        "no hold with cookie {cookie}"
                    )));
                }
                let _ = Self::profile_released(&emitter, cookie).await;
                emit_changed(conn, &self.0).await;
                Ok(())
            }

            fn set_action_enabled(&self, _action: String, _enabled: bool) -> zbus::fdo::Result<()> {
                Err(zbus::fdo::Error::NotSupported(
                    "tspmod has no optional actions".into(),
                ))
            }

            #[zbus(signal)]
            pub async fn profile_released(emitter: &SignalEmitter<'_>, cookie: u32) -> zbus::Result<()>;
        }
    };
}

ppd_interface!(PowerProfiles, "org.freedesktop.UPower.PowerProfiles");
ppd_interface!(HadessPowerProfiles, "net.hadess.PowerProfiles");

// ---------------------------------------------------------------------------------------------
// org.tspmo.Daemon1
// ---------------------------------------------------------------------------------------------

fn tspmo_props(d: &Daemon) -> Vec<(&'static str, OwnedValue)> {
    let g = d.gpu_status();
    vec![
        ("Profile", ov(d.effective().as_str())),
        ("ProfileAc", ov(d.profile_ac.as_str())),
        ("ProfileBattery", ov(d.profile_battery.as_str())),
        ("OnBattery", ov(d.on_battery)),
        ("GpuMode", ov(g.mode.as_str())),
        ("GpuPolicy", ov(d.gpu_policy.as_str())),
        ("GpuPending", ov(g.pending.map_or("", Mode::as_str))),
        ("DgpuAsleep", ov(g.dgpu_asleep)),
        ("GpuMessage", ov(d.gpu_message.clone())),
        ("GpuSwitching", ov(d.gpu_switching.map_or("", Mode::as_str))),
    ]
}

/// Show an on-screen note for the changes KDE's own power-profile OSD can't show: it sees
/// Eco+ as power-saver, so moving between those two changes nothing it knows about.
fn announce(before: Profile, after: Profile) {
    let text = match (before, after) {
        (b, Profile::EcoPlus) if b != Profile::EcoPlus => "Eco+",
        (Profile::EcoPlus, Profile::PowerSaver) => "Power saver",
        _ => return,
    };
    crate::session::osd(
        if text == "Eco+" {
            "eco-plus"
        } else {
            "battery-profile-powersave"
        },
        text,
        None,
    );
}

/// The on-screen note for a mode chosen with a key.
pub fn osd_mode(p: Profile) {
    let (icon, text) = match p {
        Profile::EcoPlus => ("eco-plus", "Eco+"),
        Profile::PowerSaver => ("battery-profile-powersave", "Power saver"),
        Profile::Balanced => ("battery-profile-balanced", "Balanced"),
        Profile::Performance => ("battery-profile-performance", "Performance"),
    };
    crate::session::osd(icon, text, None);
}

pub struct Tspmo(pub Shared);

#[zbus::interface(name = "org.tspmo.Daemon1")]
impl Tspmo {
    /// Everything the GUI needs for its first frame, in one round trip.
    fn get_state(&self) -> HashMap<String, OwnedValue> {
        tspmo_props(&lock(&self.0))
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect()
    }

    #[zbus(property)]
    fn profile(&self) -> String {
        lock(&self.0).effective().as_str().to_owned()
    }
    #[zbus(property)]
    fn profile_ac(&self) -> String {
        lock(&self.0).profile_ac.as_str().to_owned()
    }
    #[zbus(property)]
    fn profile_battery(&self) -> String {
        lock(&self.0).profile_battery.as_str().to_owned()
    }
    #[zbus(property)]
    fn on_battery(&self) -> bool {
        lock(&self.0).on_battery
    }
    #[zbus(property)]
    fn gpu_mode(&self) -> String {
        lock(&self.0).gpu_status().mode.as_str().to_owned()
    }
    #[zbus(property)]
    fn gpu_policy(&self) -> String {
        lock(&self.0).gpu_policy.as_str().to_owned()
    }
    #[zbus(property)]
    fn gpu_pending(&self) -> String {
        lock(&self.0)
            .gpu_status()
            .pending
            .map_or("", Mode::as_str)
            .to_owned()
    }
    #[zbus(property)]
    fn dgpu_asleep(&self) -> bool {
        lock(&self.0).gpu_status().dgpu_asleep
    }
    #[zbus(property)]
    fn gpu_message(&self) -> String {
        lock(&self.0).gpu_message.clone()
    }

    /// Choose a mode for the current power source: `eco-plus`, `power-saver`, `balanced` or
    /// `performance`. (`ActiveProfile` of the power-profiles API can't name `eco-plus`.)
    async fn set_profile(
        &self,
        profile: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let p = Profile::parse(&profile)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("unknown profile '{profile}'")))?;
        let sender = hdr.sender().map(|s| s.to_string());
        polkit::check(conn, sender.as_deref(), polkit::SWITCH_PROFILE).await?;
        {
            let mut d = lock(&self.0);
            let before = d.effective();
            d.choose(p);
            announce(before, d.effective());
        }
        emit_changed(conn, &self.0).await;
        Ok(())
    }

    /// eco-plus → power-saver → balanced → performance → eco-plus.
    async fn cycle_profile(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<String> {
        let sender = hdr.sender().map(|s| s.to_string());
        polkit::check(conn, sender.as_deref(), polkit::SWITCH_PROFILE).await?;
        let next = {
            let mut d = lock(&self.0);
            let before = d.effective();
            let next = d.chosen().next();
            d.choose(next);
            announce(before, d.effective());
            next
        };
        emit_changed(conn, &self.0).await;
        Ok(next.as_str().to_owned())
    }

    /// What `tspmo doctor` prints: `(severity, topic, text)` for every finding, severity
    /// `error`, `warn` or `info`. The text is the title and detail; a fix is appended as
    /// `Fix: <command>` lines. Runs the checks on each call (a few milliseconds, no hardware
    /// access beyond sysfs and `/proc`); never touches the NVIDIA GPU.
    fn doctor(&self) -> Vec<(String, String, String)> {
        let root = lock(&self.0).root.clone();
        let m = tspmo_core::machine::Machine::probe(&root);
        tspmo_core::doctor::run(&root, &m)
            .into_iter()
            .map(|f| {
                let sev = match f.severity {
                    tspmo_core::doctor::Severity::Error => "error",
                    tspmo_core::doctor::Severity::Warn => "warn",
                    tspmo_core::doctor::Severity::Info => "info",
                };
                let mut text = format!("{}. {}", f.title, f.detail);
                if let Some(fix) = f.fix {
                    for c in fix.commands {
                        text.push_str(&format!("\nFix: {c}"));
                    }
                }
                (sev.to_owned(), f.id.to_owned(), text)
            })
            .collect()
    }

    /// Set the GPU policy: `eco`, `hybrid`, `ultimate` or `optimized`. Returns a message for the user.
    async fn set_gpu_mode(
        &self,
        mode: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<String> {
        let policy = Policy::parse(&mode)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("unknown GPU mode '{mode}'")))?;
        let sender = hdr.sender().map(|s| s.to_string());
        polkit::check(conn, sender.as_deref(), polkit::SET_GPU).await?;
        let (target, current) = {
            let d = lock(&self.0);
            let st = d.gpu_status();
            (policy.target(d.on_battery), st)
        };
        // Same hardware mode (e.g. choosing Optimized while on AC in Hybrid): just save the policy.
        if target == current.mode && current.pending.is_none() {
            let msg = {
                let mut d = lock(&self.0);
                d.gpu_policy = policy;
                d.gpu_waiting = false;
                d.save();
                d.gpu_message = format!("GPU mode: {}", policy.as_str());
                d.gpu_message.clone()
            };
            emit_changed(conn, &self.0).await;
            return Ok(msg);
        }
        gpu::begin(conn.clone(), self.0.clone(), target, Some(policy), false)
            .map_err(zbus::fdo::Error::Failed)?;
        Ok(format!("Switching to {target}…"))
    }

    /// The mode a switch in progress is heading to, or "".
    #[zbus(property)]
    fn gpu_switching(&self) -> String {
        lock(&self.0).gpu_switching.map_or("", Mode::as_str).to_owned()
    }

    /// Processes holding the NVIDIA GPU: `(pid, name, kind)`. `render`: an app using it (blocks
    /// Eco). `display`: the desktop watching its outputs (released automatically on switch).
    fn gpu_users(&self) -> Vec<(u32, String, String)> {
        let d = lock(&self.0);
        gpu::blocking_users(&d.root)
            .into_iter()
            .map(|u| {
                let kind = match u.kind {
                    tspmo_core::gpu::UseKind::Render => "render",
                    tspmo_core::gpu::UseKind::Display => "display",
                };
                (u.pid, u.name, kind.to_owned())
            })
            .collect()
    }
}
