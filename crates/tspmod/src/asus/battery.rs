//! `org.tspmo.Battery1`: charge limit, "charge to 100 % once", health and live numbers.
//!
//! Event-driven: UPower tells us when the battery's state or percentage changes, the power
//! source event covers plug/unplug. Nothing polls.

use super::thermal::lock;
use super::{PATH, authorize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tspmo_core::asus::battery::{self, Info};
use tspmo_core::asus::quirks::{self, Quirks};
use tspmo_sys::{SysRoot, power_supply};
use zbus::message::Header;

#[derive(Debug, Default)]
struct State {
    /// The limit the user chose; `None` until they do (the firmware's own is left alone).
    user_limit: Option<u8>,
    once: bool,
    info: Info,
    on_battery: bool,
}

pub struct Battery {
    root: SysRoot,
    name: String,
    quirks: Quirks,
    file: PathBuf,
    st: Mutex<State>,
}

impl Battery {
    /// `None` on a machine without a system battery.
    pub fn new(root: SysRoot, dir: PathBuf) -> Option<Self> {
        let name = battery::find(&root)?;
        let on_battery = power_supply::on_ac(&root) == Some(false);
        let b = Self {
            quirks: quirks::detect(&root),
            file: dir.join("battery"),
            st: Mutex::new(State {
                info: battery::read(&root, &name),
                on_battery,
                ..State::default()
            }),
            name,
            root,
        };
        b.load();
        Some(b)
    }

    fn load(&self) {
        let Ok(text) = std::fs::read_to_string(&self.file) else {
            return;
        };
        let mut st = lock(&self.st);
        for l in text.lines() {
            match l.split_once('=') {
                Some(("limit", v)) => {
                    st.user_limit = v.trim().parse().ok().filter(|v| self.quirks.charge.allows(*v))
                }
                Some(("once", v)) => st.once = v.trim() == "true",
                _ => {}
            }
        }
    }

    fn save(&self, st: &State) {
        let mut t = String::from("# tspmod battery (managed by tspmod)\n");
        if let Some(l) = st.user_limit {
            t.push_str(&format!("limit={l}\n"));
        }
        t.push_str(&format!("once={}\n", st.once));
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.file.with_extension("tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }

    pub fn has_limit(&self) -> bool {
        lock(&self.st).info.limit.is_some()
    }

    /// At start-up: bring the firmware to the limit the user chose (if they chose one), or end
    /// a "charge once" that outlived its charger.
    pub fn startup(&self) {
        let (limit, once) = {
            let mut st = lock(&self.st);
            st.info = battery::read(&self.root, &self.name);
            if st.once && battery::once_finished(&st.info.status, st.info.percent, st.on_battery) {
                st.once = false;
                self.save(&st);
            }
            (st.user_limit, st.once)
        };
        let target = if once { Some(100) } else { limit };
        if let Some(v) = target {
            if let Err(e) = battery::set_limit(&self.root, &self.name, &self.quirks, v) {
                eprintln!("tspmod: charge limit: {e}");
            }
        }
        self.refresh();
    }

    /// Re-read the battery; ends "charge once" when it is done. Returns whether anything shown changed.
    pub fn refresh(&self) -> bool {
        let info = battery::read(&self.root, &self.name);
        let finish = {
            let st = lock(&self.st);
            st.once && battery::once_finished(&info.status, info.percent, st.on_battery)
        };
        if finish {
            let _ = self.charge_once(false);
        }
        let info = battery::read(&self.root, &self.name);
        let mut st = lock(&self.st);
        let changed = st.info != info;
        st.info = info;
        changed
    }

    pub fn on_power_source(&self, on_battery: bool) -> bool {
        lock(&self.st).on_battery = on_battery;
        self.refresh() || on_battery
    }

    pub fn set_limit(&self, value: u8) -> Result<(), String> {
        battery::set_limit(&self.root, &self.name, &self.quirks, value)?;
        let mut st = lock(&self.st);
        st.user_limit = Some(value);
        st.once = false;
        self.save(&st);
        st.info = battery::read(&self.root, &self.name);
        Ok(())
    }

    /// Charge to 100 % now (`true`), or go back to the chosen limit (`false`).
    pub fn charge_once(&self, enable: bool) -> Result<(), String> {
        let target = if enable {
            100
        } else {
            lock(&self.st).user_limit.unwrap_or(100)
        };
        battery::set_limit(&self.root, &self.name, &self.quirks, target)?;
        let mut st = lock(&self.st);
        st.once = enable;
        self.save(&st);
        st.info = battery::read(&self.root, &self.name);
        Ok(())
    }
}

pub struct Battery1(pub Arc<Battery>);

#[zbus::interface(name = "org.tspmo.Battery1")]
impl Battery1 {
    /// The limit in force, percent (0: this battery has no limit).
    #[zbus(property)]
    fn limit(&self) -> u8 {
        lock(&self.0.st).info.limit.unwrap_or(0)
    }

    /// The limit the user chose (0: never set). Differs from `Limit` during "charge once".
    #[zbus(property)]
    fn user_limit(&self) -> u8 {
        lock(&self.0.st).user_limit.unwrap_or(0)
    }

    /// Limits the firmware honours.
    #[zbus(property)]
    fn steps(&self) -> Vec<u8> {
        if self.0.has_limit() {
            self.0.quirks.charge.values()
        } else {
            Vec::new()
        }
    }

    #[zbus(property)]
    fn charge_once(&self) -> bool {
        lock(&self.0.st).once
    }

    /// `Charging`, `Discharging`, `Not charging`, `Full`.
    #[zbus(property)]
    fn status(&self) -> String {
        lock(&self.0.st).info.status.clone()
    }

    #[zbus(property)]
    fn percent(&self) -> u8 {
        lock(&self.0.st).info.percent.unwrap_or(0)
    }

    /// Full capacity over design capacity, percent (-1 unknown).
    #[zbus(property)]
    fn health(&self) -> f64 {
        lock(&self.0.st).info.health.unwrap_or(-1.0)
    }

    #[zbus(property)]
    fn cycles(&self) -> u32 {
        lock(&self.0.st).info.cycles.unwrap_or(0)
    }

    /// Watts in or out (-1 unknown).
    #[zbus(property)]
    fn power(&self) -> f64 {
        lock(&self.0.st).info.power_w.unwrap_or(-1.0)
    }

    /// Seconds until empty or full (-1 unknown).
    #[zbus(property)]
    fn time_left(&self) -> i64 {
        lock(&self.0.st).info.time_left.map_or(-1, |s| s as i64)
    }

    async fn set_limit(
        &self,
        percent: u8,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_BATTERY).await?;
        self.0.set_limit(percent).map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    /// Charge to 100 % once: back to the limit after unplugging or when full.
    async fn set_charge_once(
        &self,
        enable: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_BATTERY).await?;
        self.0
            .charge_once(enable)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }
}

pub async fn changed(conn: &zbus::Connection) {
    let Ok(r) = conn.object_server().interface::<_, Battery1>(PATH).await else {
        return;
    };
    let i = r.get().await;
    let e = r.signal_emitter();
    let _ = i.limit_changed(e).await;
    let _ = i.user_limit_changed(e).await;
    let _ = i.charge_once_changed(e).await;
    let _ = i.status_changed(e).await;
    let _ = i.percent_changed(e).await;
    let _ = i.health_changed(e).await;
    let _ = i.cycles_changed(e).await;
    let _ = i.power_changed(e).await;
    let _ = i.time_left_changed(e).await;
}

/// Follow UPower's battery summary: refresh when the state or percentage changes.
pub fn watch(conn: zbus::blocking::Connection, b: Arc<Battery>) {
    super::spawn("battery", move || {
        let Ok(builder) = zbus::blocking::fdo::PropertiesProxy::builder(&conn)
            .destination("org.freedesktop.UPower")
            .and_then(|b| b.path("/org/freedesktop/UPower/devices/DisplayDevice"))
        else {
            return;
        };
        let Ok(props) = builder.build() else { return };
        let Ok(signals) = props.receive_properties_changed() else {
            return eprintln!("tspmod: UPower unavailable; battery numbers won't update live");
        };
        for sig in signals {
            let Ok(args) = sig.args() else { continue };
            // Rate and estimate wobble constantly; state and percentage are what matter.
            if args
                .changed_properties()
                .keys()
                .any(|k| matches!(*k, "State" | "Percentage"))
                && b.refresh()
            {
                zbus::block_on(changed(conn.inner()));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tspmo_core::asus::testfix::Fix;

    const LIMIT: &str = "/sys/class/power_supply/BAT1/charge_control_end_threshold";

    fn bat(fx: &Fix) -> Battery {
        Battery::new(fx.root(), fx.dir().join("state")).unwrap()
    }

    #[test]
    fn default_leaves_the_firmware_limit_alone() {
        let fx = Fix::new("bat-default");
        fx.set(LIMIT, "75\n");
        let b = bat(&fx);
        b.startup();
        assert_eq!(fx.get(LIMIT), "75");
        assert_eq!(lock(&b.st).user_limit, None);
    }

    #[test]
    fn limit_is_validated_saved_and_reapplied() {
        let fx = Fix::new("bat-limit");
        let b = bat(&fx);
        assert!(b.set_limit(85).is_err());
        b.set_limit(80).unwrap();
        assert_eq!(fx.get(LIMIT), "80");
        // The EC forgot it (reboot): the next start puts it back.
        fx.set(LIMIT, "100\n");
        let b = bat(&fx);
        assert_eq!(lock(&b.st).user_limit, Some(80));
        b.startup();
        assert_eq!(fx.get(LIMIT), "80");
    }

    #[test]
    fn charge_once_ends_when_full_or_unplugged() {
        let fx = Fix::new("bat-once");
        let st = "/sys/class/power_supply/BAT1/status";
        fx.set(st, "Charging\n");
        let b = bat(&fx);
        b.set_limit(80).unwrap();
        b.charge_once(true).unwrap();
        assert_eq!(fx.get(LIMIT), "100");
        assert!(lock(&b.st).once);
        // Still charging on AC: nothing changes.
        b.on_power_source(false);
        assert_eq!(fx.get(LIMIT), "100");
        // Full: back to 80.
        fx.set(st, "Full\n");
        b.refresh();
        assert_eq!(fx.get(LIMIT), "80");
        assert!(!lock(&b.st).once);

        // Unplugged.
        fx.set(st, "Charging\n");
        b.charge_once(true).unwrap();
        fx.set("/sys/class/power_supply/ACAD/online", "0\n");
        b.on_power_source(true);
        assert_eq!(fx.get(LIMIT), "80");
    }

    #[test]
    fn charge_once_survives_restart_until_done() {
        let fx = Fix::new("bat-once-restart");
        fx.set("/sys/class/power_supply/BAT1/status", "Charging\n")
            .set("/sys/class/power_supply/ACAD/online", "1\n");
        let b = bat(&fx);
        b.charge_once(true).unwrap();
        let b = bat(&fx);
        b.startup();
        assert!(lock(&b.st).once);
        assert_eq!(fx.get(LIMIT), "100");
    }
}
