//! `org.tspmo.Lid1`: the Slash light bar (and, on models that have one, the AniMe Matrix).
//!
//! Like the keyboard, the device is opened per write and closed again. Settings the firmware
//! keeps itself (show on boot, sleep, shutdown, lid closed) are written once; "show on
//! battery" is also enforced here, because it follows the power source.

use super::thermal::lock;
use super::{PATH, authorize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tspmo_core::asus::slash::{self, Mode, Slash, When};
use tspmo_sys::{SysRoot, power_supply};
use zbus::message::Header;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SlashCfg {
    enabled: bool,
    brightness: u8,
    interval: u8,
    mode: Mode,
    boot: bool,
    sleep: bool,
    shutdown: bool,
    /// Lit while running on battery. Off by default: it costs battery.
    battery: bool,
    lid: bool,
    warning: bool,
}

impl Default for SlashCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            brightness: 255,
            interval: 0,
            mode: Mode::Bounce,
            boot: true,
            sleep: true,
            shutdown: true,
            battery: false,
            lid: true,
            warning: true,
        }
    }
}

const OPTIONS: [&str; 6] = ["boot", "sleep", "shutdown", "battery", "lid", "warning"];

impl SlashCfg {
    fn option(&self, name: &str) -> Option<bool> {
        Some(match name {
            "boot" => self.boot,
            "sleep" => self.sleep,
            "shutdown" => self.shutdown,
            "battery" => self.battery,
            "lid" => self.lid,
            "warning" => self.warning,
            _ => return None,
        })
    }

    fn set_option(&mut self, name: &str, v: bool) -> bool {
        let slot = match name {
            "boot" => &mut self.boot,
            "sleep" => &mut self.sleep,
            "shutdown" => &mut self.shutdown,
            "battery" => &mut self.battery,
            "lid" => &mut self.lid,
            "warning" => &mut self.warning,
            _ => return false,
        };
        *slot = v;
        true
    }
}

struct State {
    cfg: SlashCfg,
    on_battery: bool,
}

pub struct Lid {
    root: SysRoot,
    slash: Option<Slash>,
    file: PathBuf,
    st: Mutex<State>,
}

impl Lid {
    /// `None` if the machine has no lid display.
    pub fn new(root: SysRoot, dir: PathBuf) -> Option<Self> {
        let slash = slash::detect(&root);
        slash.as_ref()?;
        let lid = Self {
            file: dir.join("lid"),
            st: Mutex::new(State {
                cfg: SlashCfg::default(),
                on_battery: power_supply::on_ac(&root) == Some(false),
            }),
            slash,
            root,
        };
        lid.load();
        Some(lid)
    }

    fn load(&self) {
        let Ok(text) = std::fs::read_to_string(&self.file) else {
            return;
        };
        let mut st = lock(&self.st);
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let (k, v) = (k.trim(), v.trim());
            let c = &mut st.cfg;
            match k {
                "enabled" => c.enabled = v == "1",
                "brightness" => c.brightness = v.parse().unwrap_or(c.brightness),
                "interval" => c.interval = v.parse().unwrap_or(c.interval),
                "mode" => c.mode = Mode::parse(v).unwrap_or(c.mode),
                other => {
                    c.set_option(other, v == "1");
                }
            }
        }
    }

    fn save(&self, st: &State) {
        let c = &st.cfg;
        let mut t = String::from("# tspmod lid display (managed by tspmod)\n");
        t.push_str(&format!(
            "enabled={}\nbrightness={}\ninterval={}\nmode={}\n",
            u8::from(c.enabled),
            c.brightness,
            c.interval,
            c.mode.name()
        ));
        for o in OPTIONS {
            t.push_str(&format!("{o}={}\n", u8::from(c.option(o).unwrap_or(false))));
        }
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.file.with_extension("tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }

    fn send(&self, packets: &[Vec<u8>]) {
        if let Some(s) = &self.slash {
            if let Err(e) = s.send(&self.root, packets) {
                eprintln!("tspmod: slash: {e}");
            }
        }
    }

    /// Lit now: enabled, and either on AC or allowed on battery.
    fn lit(c: &SlashCfg, on_battery: bool) -> bool {
        c.enabled && (!on_battery || c.battery)
    }

    fn display_packets(&self, c: &SlashCfg, on_battery: bool) -> [Vec<u8>; 2] {
        let id = self.report();
        let lit = Self::lit(c, on_battery);
        [
            slash::enable(id, lit),
            slash::options(id, lit, c.brightness, c.interval),
        ]
    }

    fn report(&self) -> u8 {
        self.slash.as_ref().map_or(0, |s| s.report)
    }

    /// Everything, in the order the firmware wants it (start-up, resume).
    pub fn apply(&self) {
        let (c, on_battery) = {
            let st = lock(&self.st);
            (st.cfg.clone(), st.on_battery)
        };
        let id = self.report();
        let [i0, i1] = slash::init(id);
        let [en, op] = self.display_packets(&c, on_battery);
        self.send(&[
            i0,
            i1,
            en,
            op,
            slash::mode(id, c.mode),
            slash::save(id),
            slash::show_when(id, When::Boot, c.boot),
            slash::show_when(id, When::Sleep, c.sleep),
            slash::show_when(id, When::Shutdown, c.shutdown),
            slash::show_when(id, When::LowBattery, c.warning),
            slash::show_on_battery(id, c.battery),
            slash::show_on_lid_closed(id, c.lid),
            slash::save(id),
        ]);
        // Remember the defaults on first start.
        self.save(&lock(&self.st));
    }

    pub fn on_power_source(&self, on_battery: bool) {
        let c = {
            let mut st = lock(&self.st);
            if st.on_battery == on_battery {
                return;
            }
            st.on_battery = on_battery;
            st.cfg.clone()
        };
        self.send(&self.display_packets(&c, on_battery));
    }

    fn change(&self, f: impl FnOnce(&mut SlashCfg) -> Result<Vec<Vec<u8>>, String>) -> Result<(), String> {
        let mut st = lock(&self.st);
        let on_battery = st.on_battery;
        let packets = f(&mut st.cfg)?;
        let _ = on_battery;
        self.save(&st);
        drop(st);
        self.send(&packets);
        Ok(())
    }

    pub fn set_enabled(&self, on: bool) -> Result<(), String> {
        let on_battery = lock(&self.st).on_battery;
        self.change(|c| {
            c.enabled = on;
            Ok(self.display_packets(c, on_battery).to_vec())
        })
    }

    pub fn set_brightness(&self, v: u8) -> Result<(), String> {
        let on_battery = lock(&self.st).on_battery;
        self.change(|c| {
            c.brightness = v;
            Ok(self.display_packets(c, on_battery).to_vec())
        })
    }

    pub fn set_interval(&self, v: u8) -> Result<(), String> {
        let on_battery = lock(&self.st).on_battery;
        self.change(|c| {
            c.interval = v;
            Ok(self.display_packets(c, on_battery).to_vec())
        })
    }

    pub fn set_mode(&self, name: &str) -> Result<(), String> {
        let m = Mode::parse(name).ok_or_else(|| {
            format!(
                "unknown mode '{name}' ({})",
                Mode::ALL.iter().map(|m| m.name()).collect::<Vec<_>>().join(", ")
            )
        })?;
        let id = self.report();
        self.change(|c| {
            c.mode = m;
            Ok(vec![slash::mode(id, m), slash::save(id)])
        })
    }

    pub fn set_show(&self, what: &str, on: bool) -> Result<(), String> {
        let id = self.report();
        let on_battery = lock(&self.st).on_battery;
        self.change(|c| {
            if !c.set_option(what, on) {
                return Err(format!("unknown option '{what}' ({})", OPTIONS.join(", ")));
            }
            Ok(match what {
                "boot" => vec![slash::show_when(id, When::Boot, on)],
                "sleep" => vec![slash::show_when(id, When::Sleep, on)],
                "shutdown" => vec![slash::show_when(id, When::Shutdown, on)],
                "warning" => vec![slash::show_when(id, When::LowBattery, on)],
                "battery" => {
                    let mut v = vec![slash::show_on_battery(id, on)];
                    v.extend(self.display_packets(c, on_battery));
                    v
                }
                _ => vec![slash::show_on_lid_closed(id, on), slash::save(id)],
            })
        })
    }
}

pub struct Lid1(pub Arc<Lid>);

#[zbus::interface(name = "org.tspmo.Lid1")]
impl Lid1 {
    /// Lid displays this laptop has: `slash`.
    #[zbus(property)]
    fn devices(&self) -> Vec<String> {
        vec!["slash".to_owned()]
    }

    #[zbus(property)]
    fn slash_enabled(&self) -> bool {
        lock(&self.0.st).cfg.enabled
    }

    /// 0-255.
    #[zbus(property)]
    fn slash_brightness(&self) -> u8 {
        lock(&self.0.st).cfg.brightness
    }

    /// Pause between animations, 0-255.
    #[zbus(property)]
    fn slash_interval(&self) -> u8 {
        lock(&self.0.st).cfg.interval
    }

    #[zbus(property)]
    fn slash_mode(&self) -> String {
        lock(&self.0.st).cfg.mode.name().to_owned()
    }

    #[zbus(property)]
    fn slash_modes(&self) -> Vec<String> {
        Mode::ALL.iter().map(|m| m.name().to_owned()).collect()
    }

    /// `(name, on)`: boot, sleep, shutdown, battery, lid, warning.
    #[zbus(property)]
    fn slash_show(&self) -> Vec<(String, bool)> {
        let st = lock(&self.0.st);
        OPTIONS
            .iter()
            .map(|o| ((*o).to_owned(), st.cfg.option(o).unwrap_or(false)))
            .collect()
    }

    async fn set_slash_enabled(
        &self,
        on: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0.set_enabled(on).map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_slash_brightness(
        &self,
        level: u8,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_brightness(level)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_slash_interval(
        &self,
        seconds: u8,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_interval(seconds)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_slash_mode(
        &self,
        mode: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0.set_mode(&mode).map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_slash_show(
        &self,
        what: String,
        on: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_show(&what, on)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }
}

pub async fn changed(conn: &zbus::Connection) {
    let Ok(r) = conn.object_server().interface::<_, Lid1>(PATH).await else {
        return;
    };
    let i = r.get().await;
    let e = r.signal_emitter();
    let _ = i.slash_enabled_changed(e).await;
    let _ = i.slash_brightness_changed(e).await;
    let _ = i.slash_interval_changed(e).await;
    let _ = i.slash_mode_changed(e).await;
    let _ = i.slash_show_changed(e).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tspmo_core::asus::testfix::Fix;

    fn lid(fx: &Fix) -> Lid {
        let root = fx.root();
        std::fs::create_dir_all(root.path("/dev")).unwrap();
        std::fs::write(root.path("/dev/hidraw1"), b"").unwrap();
        Lid::new(root, fx.dir().join("state")).unwrap()
    }

    fn sent(fx: &Fix) -> Vec<u8> {
        std::fs::read(fx.root().path("/dev/hidraw1")).unwrap()
    }

    fn clear(fx: &Fix) {
        std::fs::write(fx.root().path("/dev/hidraw1"), b"").unwrap();
    }

    #[test]
    fn dark_on_battery_by_default() {
        let fx = Fix::new("lid-battery");
        fx.set("/sys/class/power_supply/ACAD/online", "1\n");
        let l = lid(&fx);
        clear(&fx);
        l.on_power_source(true);
        // enable off + options off
        let b = sent(&fx);
        assert_eq!(&b[..6], &[0x5e, 0xd8, 2, 0, 1, 0x80]);
        assert_eq!(b[6 + 8], 0, "options: disabled");
        clear(&fx);
        l.on_power_source(false);
        let b = sent(&fx);
        assert_eq!(b[5], 0x00);
        assert_eq!(b[6 + 8], 1);
        // Brightness is carried.
        assert_eq!(b[6 + 10], 255);
    }

    #[test]
    fn battery_option_keeps_it_lit() {
        let fx = Fix::new("lid-show-battery");
        let l = lid(&fx);
        l.on_power_source(true);
        clear(&fx);
        l.set_show("battery", true).unwrap();
        let b = sent(&fx);
        // show_on_battery packet first (6 bytes), then enable + options.
        assert_eq!(&b[..6], &[0x5e, 0xd8, 1, 0, 1, 0x00]);
        assert_eq!(b[6 + 5], 0x00);
    }

    #[test]
    fn settings_persist_and_validate() {
        let fx = Fix::new("lid-persist");
        {
            let l = lid(&fx);
            l.set_mode("flux").unwrap();
            l.set_brightness(100).unwrap();
            l.set_show("lid", false).unwrap();
            assert!(l.set_mode("nope").is_err());
            assert!(l.set_show("nope", true).is_err());
        }
        let l = lid(&fx);
        let st = lock(&l.st);
        assert_eq!(st.cfg.mode, Mode::Flux);
        assert_eq!(st.cfg.brightness, 100);
        assert!(!st.cfg.lid);
        assert!(!st.cfg.battery);
    }

    #[test]
    fn nothing_without_the_device() {
        let fx = Fix::new("lid-none");
        fx.set("/sys/class/dmi/id/board_name", "FX507ZC4\n");
        assert!(Lid::new(fx.root(), fx.dir().join("state")).is_none());
    }

    #[test]
    fn full_apply_order() {
        let fx = Fix::new("lid-apply");
        fx.set("/sys/class/power_supply/ACAD/online", "1\n");
        let l = lid(&fx);
        clear(&fx);
        l.apply();
        let b = sent(&fx);
        assert_eq!(&b[..6], &[0x5e, 0xd7, 0, 0, 1, 0xac]); // init
        assert!(b.windows(6).any(|w| w == [0x5e, 0xd4, 0, 0, 1, 0xab])); // save
        assert!(b.ends_with(&[0x5e, 0xd4, 0, 0, 1, 0xab]));
    }
}
