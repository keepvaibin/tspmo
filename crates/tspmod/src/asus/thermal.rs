//! Fan curves and power limits per thermal mode, and how they are applied.
//!
//! Fans and power limits live together because ASUS firmware couples them: on some models the
//! kernel refuses power-limit writes unless a custom fan curve is active (see
//! `tspmo_core::asus::quirks`), and the EC drops both whenever the thermal mode changes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tspmo_core::asus::fans::{self, Curve, CurveError, Fans, Setting};
use tspmo_core::asus::limits::{self, Limit, LimitError};
use tspmo_core::asus::quirks::{self, Quirks};
use tspmo_core::power::{self, Profile};
use tspmo_sys::{SysRoot, dmi::Dmi, power_supply};

pub fn mode_index(p: Profile) -> usize {
    match p {
        // Eco+ is power-saver for the firmware's modes (and so for fan curves and limits).
        Profile::PowerSaver | Profile::EcoPlus => 0,
        Profile::Balanced => 1,
        Profile::Performance => 2,
    }
}

pub fn mode_from_index(i: usize) -> Profile {
    Profile::ALL[i]
}

pub fn source_name(on_battery: bool) -> &'static str {
    if on_battery { "battery" } else { "ac" }
}

#[derive(Debug)]
pub struct State {
    pub mode: Profile,
    /// Eco+ is active (the thermal mode is power-saver's): fans follow [`fans::eco_curve`].
    pub eco: bool,
    pub on_battery: bool,
    /// `(mode, fan)` → what the fan does there. Missing means Auto.
    pub settings: BTreeMap<(usize, u32), Setting>,
    /// Firmware curves, as read from the EC.
    pub factory: BTreeMap<(usize, u32), Curve>,
    /// `(mode, on_battery, attribute)` → watts / °C. Missing means firmware default.
    pub custom: BTreeMap<(usize, bool, String), i64>,
    /// The kernel's view of each limit, refreshed after every apply (never on a query).
    pub limits: Vec<Limit>,
    /// Custom limits were written since the EC last reloaded the thermal mode.
    dirty_power: bool,
}

pub struct Thermal {
    pub root: SysRoot,
    pub fans: Option<Fans>,
    pub quirks: Quirks,
    dir: PathBuf,
    bios: String,
    /// Serialises hardware writes; held for the whole of an apply.
    apply_lock: Mutex<()>,
    pub state: Mutex<State>,
}

pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Thermal {
    /// `None` when the machine has neither custom fan curves nor power limits.
    pub fn new(root: SysRoot, dir: PathBuf) -> Option<Self> {
        let fans = fans::discover(&root);
        let limits = limits::discover(&root);
        if fans.is_none() && limits.is_empty() {
            return None;
        }
        let mode = root
            .read_opt(power::PLATFORM_PROFILE)
            .and_then(|v| Profile::from_platform(&v))
            .unwrap_or(Profile::Balanced);
        let t = Self {
            quirks: quirks::detect(&root),
            bios: Dmi::read(&root).bios_version,
            fans,
            dir,
            apply_lock: Mutex::new(()),
            state: Mutex::new(State {
                mode,
                eco: false,
                on_battery: power_supply::on_ac(&root) == Some(false),
                settings: BTreeMap::new(),
                factory: BTreeMap::new(),
                custom: BTreeMap::new(),
                limits,
                dirty_power: false,
            }),
            root,
        };
        t.load();
        Some(t)
    }

    // --- persistence -------------------------------------------------------------------------

    fn fans_file(&self) -> PathBuf {
        self.dir.join("fans")
    }

    fn power_file(&self) -> PathBuf {
        self.dir.join("power")
    }

    fn load(&self) {
        let mut st = lock(&self.state);
        if let Ok(text) = std::fs::read_to_string(self.fans_file()) {
            let bios_ok = text
                .lines()
                .find_map(|l| l.strip_prefix("bios="))
                .is_none_or(|b| b == self.bios);
            for line in text.lines() {
                let w: Vec<&str> = line.split_whitespace().collect();
                let (Some(kind), Some(mode), Some(fan)) = (w.first(), w.get(1), w.get(2)) else {
                    continue;
                };
                let (Some(mode), Ok(fan)) = (Profile::parse(mode), fan.parse::<u32>()) else {
                    continue;
                };
                let key = (mode_index(mode), fan);
                match (*kind, w.get(3).copied()) {
                    ("set", Some("auto")) => {
                        st.settings.insert(key, Setting::Auto);
                    }
                    ("set", Some("full")) => {
                        st.settings.insert(key, Setting::Full);
                    }
                    ("set", Some("custom")) => {
                        if let Some(c) = w.get(4).and_then(|p| parse_points(p)) {
                            st.settings.insert(key, Setting::Custom(c));
                        }
                    }
                    ("factory", Some(p)) if bios_ok => {
                        if let Some(c) = parse_points(p) {
                            st.factory.insert(key, c);
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(self.power_file()) {
            for line in text.lines() {
                let w: Vec<&str> = line.split_whitespace().collect();
                if let ["limit", mode, src, name, value] = w[..] {
                    if let (Some(mode), Ok(v)) = (Profile::parse(mode), value.parse::<i64>()) {
                        if limits::is_limit(name) && matches!(src, "ac" | "battery") {
                            st.custom
                                .insert((mode_index(mode), src == "battery", name.to_owned()), v);
                        }
                    }
                }
            }
        }
    }

    fn save(&self, st: &State) {
        let mut f = format!("# tspmod fan curves (managed by tspmod)\nbios={}\n", self.bios);
        for ((m, fan), s) in &st.settings {
            let mode = mode_from_index(*m).as_str();
            match s {
                Setting::Auto => f.push_str(&format!("set {mode} {fan} auto\n")),
                Setting::Full => f.push_str(&format!("set {mode} {fan} full\n")),
                Setting::Custom(c) => f.push_str(&format!("set {mode} {fan} custom {}\n", fmt_points(c))),
            }
        }
        for ((m, fan), c) in &st.factory {
            f.push_str(&format!(
                "factory {} {fan} {}\n",
                mode_from_index(*m).as_str(),
                fmt_points(c)
            ));
        }
        let mut p = String::from("# tspmod power limits (managed by tspmod)\n");
        for ((m, bat, name), v) in &st.custom {
            p.push_str(&format!(
                "limit {} {} {name} {v}\n",
                mode_from_index(*m).as_str(),
                source_name(*bat)
            ));
        }
        let _ = std::fs::create_dir_all(&self.dir);
        write_atomic(&self.fans_file(), &f);
        write_atomic(&self.power_file(), &p);
    }

    // --- queries (never touch the firmware) ----------------------------------------------------

    #[cfg(test)]
    pub fn setting(&self, mode: Profile, fan: u32) -> Setting {
        lock(&self.state)
            .settings
            .get(&(mode_index(mode), fan))
            .cloned()
            .unwrap_or(Setting::Auto)
    }

    // --- changes -------------------------------------------------------------------------------

    fn fan_ids(&self, fan: u32) -> Result<Vec<u32>, String> {
        let fans = self.fans.as_ref().ok_or("no fan control")?;
        if fan == 0 {
            Ok(fans.fans.iter().map(|f| f.index).collect())
        } else if fans.has(fan) {
            Ok(vec![fan])
        } else {
            Err(format!("no fan {fan}"))
        }
    }

    /// Choose what fan `fan` (0 = all) does in `mode`. Applied now if `mode` is the active one.
    pub fn set_fan(&self, mode: Profile, fan: u32, setting: Setting) -> Result<(), String> {
        let ids = self.fan_ids(fan)?;
        {
            let mut st = lock(&self.state);
            if let Setting::Custom(c) = &setting {
                for id in &ids {
                    let f = st.factory.get(&(mode_index(mode), *id));
                    fans::validate(c, f).map_err(|e: CurveError| e.to_string())?;
                }
            }
            for id in &ids {
                st.settings.insert((mode_index(mode), *id), setting.clone());
            }
            self.save(&st);
        }
        self.apply_if_current(mode);
        Ok(())
    }

    /// Back to the firmware's curve and forget the one captured earlier (re-read when active).
    pub fn reset_fan(&self, mode: Profile, fan: u32) -> Result<(), String> {
        let ids = self.fan_ids(fan)?;
        {
            let mut st = lock(&self.state);
            let active = st.mode == mode;
            for id in &ids {
                st.settings.remove(&(mode_index(mode), *id));
                // Only the active mode's curve can be read again right now.
                if active {
                    st.factory.remove(&(mode_index(mode), *id));
                }
            }
            self.save(&st);
        }
        self.apply_if_current(mode);
        Ok(())
    }

    /// `value` of `None` means the firmware default.
    pub fn set_limit(
        &self,
        mode: Profile,
        on_battery: bool,
        name: &str,
        value: Option<i64>,
    ) -> Result<(), String> {
        if !limits::is_limit(name) {
            return Err(format!("unknown limit '{name}'"));
        }
        {
            let mut st = lock(&self.state);
            let Some(known) = st.limits.iter().find(|l| l.name == name).cloned() else {
                return Err(format!("no limit '{name}' on this machine"));
            };
            let key = (mode_index(mode), on_battery, name.to_owned());
            if value.is_some() && on_battery == st.on_battery && !known.adjustable() {
                return Err(format!("{}: fixed on {}", known.label, source_name(on_battery)));
            }
            match value {
                None => {
                    st.custom.remove(&key);
                }
                Some(v) => {
                    // The kernel reports bounds for the power source in use only.
                    if on_battery == st.on_battery && !(known.min..=known.max).contains(&v) {
                        return Err(format!(
                            "{}: must be {}-{} {}",
                            known.label, known.min, known.max, known.unit
                        ));
                    }
                    if v <= 0 {
                        return Err(format!("{}: must be positive", known.label));
                    }
                    st.custom.insert(key, v);
                }
            }
            self.save(&st);
        }
        self.apply_if_current(mode);
        Ok(())
    }

    pub fn reset_limits(&self, mode: Profile) {
        {
            let mut st = lock(&self.state);
            st.custom.retain(|(m, _, _), _| *m != mode_index(mode));
            self.save(&st);
        }
        self.apply_if_current(mode);
    }

    fn apply_if_current(&self, mode: Profile) {
        let current = lock(&self.state).mode;
        if current == mode {
            self.apply(false);
        } else {
            // Another mode's settings: nothing to write now, but the limits view may change.
            self.refresh_limits();
        }
    }

    // --- events --------------------------------------------------------------------------------

    /// Eco+ starts or ends; the next [`Thermal::on_profile`] writes what it needs.
    pub fn set_eco(&self, eco: bool) {
        lock(&self.state).eco = eco;
    }

    /// The thermal mode or power source changed: the EC dropped our settings; write them again.
    pub fn on_profile(&self, mode: Profile, on_battery: bool) {
        {
            let mut st = lock(&self.state);
            if st.mode != mode {
                // A mode change reloads the EC: nothing of ours is in force any more.
                st.dirty_power = false;
            }
            st.mode = mode;
            st.on_battery = on_battery;
        }
        self.apply(false);
    }

    /// Resume: the EC may have dropped our settings behind the driver's back.
    pub fn on_resume(&self) {
        self.apply(true);
    }

    pub fn refresh_limits(&self) {
        let mut l = limits::discover(&self.root);
        let mut st = lock(&self.state);
        let m = mode_index(st.mode);
        for x in &mut l {
            if !st.custom.contains_key(&(m, st.on_battery, x.name.to_owned())) {
                x.current = x.default;
            }
        }
        st.limits = l;
    }

    // --- applying ------------------------------------------------------------------------------

    /// Write what the active mode wants. `force` rewrites curves even if they look in force.
    pub fn apply(&self, force: bool) {
        let _serial = lock(&self.apply_lock);
        let (mode, on_battery, mut settings, mut factory, wanted, eco) = {
            let st = lock(&self.state);
            let m = mode_index(st.mode);
            let settings: BTreeMap<u32, Setting> = st
                .settings
                .iter()
                .filter(|((mi, _), _)| *mi == m)
                .map(|((_, f), s)| (*f, s.clone()))
                .collect();
            let factory: BTreeMap<u32, Curve> = st
                .factory
                .iter()
                .filter(|((mi, _), _)| *mi == m)
                .map(|((_, f), c)| (*f, c.clone()))
                .collect();
            let wanted: Vec<(String, i64)> = st
                .custom
                .iter()
                .filter(|((mi, bat, _), _)| *mi == m && *bat == st.on_battery)
                .map(|((_, _, n), v)| (n.clone(), *v))
                .collect();
            (st.mode, st.on_battery, settings, factory, wanted, st.eco)
        };
        let root = &self.root;

        // The firmware's own curves, once per mode: they are the baseline for validation and
        // for the "custom curve required" coupling. Reading them resets the fans to Auto.
        let mut captured = false;
        if let Some(fans) = &self.fans {
            for f in &fans.fans {
                if let std::collections::btree_map::Entry::Vacant(e) = factory.entry(f.index) {
                    match fans.factory(root, f.index) {
                        Ok(c) => {
                            e.insert(c);
                            captured = true;
                        }
                        Err(e) => eprintln!("tspmod: fan {} curve unreadable: {e}", f.label),
                    }
                }
            }
            // Eco+ owns the fans: stopped while cool, derived from the firmware's quiet curve.
            if eco {
                let tjmax = fans::tjmax(root);
                for f in &fans.fans {
                    let c = fans::eco_curve(factory.get(&f.index), tjmax);
                    settings.insert(f.index, Setting::Custom(c));
                }
            }
        }

        // Only limits this machine has, within the kernel's current bounds.
        let known = limits::discover(root);
        let wanted: Vec<(Limit, i64)> = wanted
            .into_iter()
            .filter_map(|(n, v)| {
                let l = known.iter().find(|l| l.name == n && l.adjustable())?.clone();
                let v = l.clamp(v);
                Some((l, v))
            })
            .collect();

        // Back to firmware limits: only a reload of the thermal mode does that.
        let dirty = lock(&self.state).dirty_power;
        if wanted.is_empty() && dirty {
            // The kernel only remembers what it last wrote: put its notion back to the
            // defaults too (a curve is still active, so the firmware takes the writes), then
            // let the firmware reload the mode.
            for l in known.iter().filter(|l| l.current != l.default && l.adjustable()) {
                let _ = limits::set(root, l.name, l.default);
            }
            if let Some(fans) = &self.fans {
                if let Err(e) = fans.reset_mode(root) {
                    eprintln!("tspmod: can't restore firmware limits: {e}");
                }
            }
            lock(&self.state).dirty_power = false;
        }

        // Fans.
        let all_auto = self.fans.as_ref().is_none_or(|fans| {
            fans.fans
                .iter()
                .all(|f| matches!(settings.get(&f.index), None | Some(Setting::Auto)))
        });
        let pin = self.quirks.power_needs_fan_curve && !wanted.is_empty() && all_auto;
        if let Some(fans) = &self.fans {
            for f in &fans.fans {
                let want = match settings.get(&f.index).unwrap_or(&Setting::Auto) {
                    Setting::Auto if pin => factory.get(&f.index).cloned(),
                    s => s.curve(),
                };
                let r = match &want {
                    Some(c) => fans.write(root, f.index, c, force),
                    None => fans.auto(root, f.index),
                };
                if let Err(e) = r {
                    eprintln!("tspmod: fan {} curve not applied: {e}", f.label);
                }
            }
        }

        // Power limits (after the fans: the kernel may need an active curve).
        let mut wrote = false;
        for (l, v) in &wanted {
            let mut r = limits::set(root, l.name, *v);
            if r == Err(LimitError::NeedsFanCurve) {
                self.pin_factory(&factory);
                r = limits::set(root, l.name, *v);
            }
            match r {
                Ok(()) => wrote = true,
                Err(e) => eprintln!("tspmod: {}: {e}", l.label),
            }
        }

        // The kernel only remembers what was last written; after a reload of the thermal
        // mode the firmware default is what runs.
        let mut limits_now = limits::discover(root);
        for l in &mut limits_now {
            if !wanted.iter().any(|(w, _)| w.name == l.name) {
                l.current = l.default;
            }
        }
        let mut st = lock(&self.state);
        for (i, c) in factory {
            st.factory.insert((mode_index(mode), i), c);
        }
        if wrote {
            st.dirty_power = true;
        }
        st.limits = limits_now;
        let _ = on_battery;
        if captured {
            self.save(&st);
        }
    }

    /// Activate the firmware's own curve as a "custom" one on every fan with a known curve.
    fn pin_factory(&self, factory: &BTreeMap<u32, Curve>) {
        let Some(fans) = &self.fans else { return };
        if fans.any_custom(&self.root) {
            return;
        }
        for f in &fans.fans {
            if let Some(c) = factory.get(&f.index) {
                if let Err(e) = fans.write(&self.root, f.index, c, false) {
                    eprintln!("tspmod: fan {} curve not applied: {e}", f.label);
                }
            }
        }
    }
}

pub fn parse_points(s: &str) -> Option<Curve> {
    let v: Option<Vec<(u8, u8)>> = s
        .split(',')
        .map(|p| {
            let (t, s) = p.split_once(':')?;
            Some((t.parse().ok()?, s.parse().ok()?))
        })
        .collect();
    Curve::from_points(&v?)
}

pub fn fmt_points(c: &Curve) -> String {
    c.0.iter()
        .map(|(t, s)| format!("{t}:{s}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn write_atomic(path: &std::path::Path, text: &str) {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tspmo_core::asus::testfix::Fix;

    fn thermal(fx: &Fix) -> Thermal {
        Thermal::new(fx.root(), fx.dir().join("state")).unwrap()
    }

    const HW: &str = "/sys/class/hwmon/hwmon8";
    const A: &str = "/sys/class/firmware-attributes/asus-armoury/attributes";

    /// The fixture was captured on battery; widen the bounds like the kernel does on AC.
    fn on_ac(fx: &Fix) {
        fx.set("/sys/class/power_supply/ACAD/online", "1\n");
        for (a, min, max) in [("ppt_pl1_spl", 28, 90), ("ppt_pl2_sppt", 28, 135)] {
            fx.set(&format!("{A}/{a}/min_value"), &format!("{min}\n"));
            fx.set(&format!("{A}/{a}/max_value"), &format!("{max}\n"));
            fx.set(&format!("{A}/{a}/default_value"), &format!("{max}\n"));
        }
    }

    #[test]
    fn points_round_trip() {
        let c = Curve::full();
        assert_eq!(parse_points(&fmt_points(&c)), Some(c));
        assert_eq!(parse_points("1:2,3:4"), None);
        assert_eq!(parse_points("x"), None);
    }

    #[test]
    fn custom_curve_is_applied_for_the_active_mode_only() {
        let fx = Fix::new("th-curve");
        fx.set(power::PLATFORM_PROFILE, "balanced\n");
        let t = thermal(&fx);
        t.on_profile(Profile::Balanced, false);
        let f = fx_factory(&t, 1);
        let mut c = f.clone();
        c.0[0] = (40, 0);
        t.set_fan(Profile::Balanced, 1, Setting::Custom(c.clone()))
            .unwrap();
        assert_eq!(fx.get(&format!("{HW}/pwm1_enable")), "1");
        assert_eq!(t.fans.as_ref().unwrap().read(&fx.root(), 1), Some(c));

        // Another mode: stored, not written.
        fx.set(&format!("{HW}/pwm2_enable"), "2\n");
        t.set_fan(Profile::Performance, 2, Setting::Full).unwrap();
        assert_eq!(fx.get(&format!("{HW}/pwm2_enable")), "2");
        // ... until that mode becomes active.
        t.on_profile(Profile::Performance, false);
        assert_eq!(fx.get(&format!("{HW}/pwm2_enable")), "1");
        assert_eq!(fx.get(&format!("{HW}/pwm2_auto_point1_pwm")), "255");
    }

    #[test]
    fn eco_plus_stops_the_fans_while_cool_and_leaving_it_gives_them_back() {
        let fx = Fix::new("th-eco");
        fx.set(power::PLATFORM_PROFILE, "low-power\n");
        let t = thermal(&fx);
        // A user curve for power-saver does not survive Eco+ ...
        t.set_fan(Profile::PowerSaver, 0, Setting::Full).unwrap();
        t.set_eco(true);
        t.on_profile(Profile::PowerSaver, false);
        for fan in 1..=3 {
            assert_eq!(fx.get(&format!("{HW}/pwm{fan}_enable")), "1");
            assert_eq!(
                fx.get(&format!("{HW}/pwm{fan}_auto_point1_pwm")),
                "0",
                "fan {fan}"
            );
            assert_eq!(
                fx.get(&format!("{HW}/pwm{fan}_auto_point2_pwm")),
                "0",
                "fan {fan}"
            );
        }
        // ... and is back, not Eco+'s curve, when Eco+ ends.
        t.set_eco(false);
        t.on_profile(Profile::PowerSaver, false);
        assert_eq!(fx.get(&format!("{HW}/pwm1_auto_point1_pwm")), "255");
        t.set_fan(Profile::PowerSaver, 0, Setting::Auto).unwrap();
        assert_eq!(fx.get(&format!("{HW}/pwm1_enable")), "2");
    }

    fn fx_factory(t: &Thermal, fan: u32) -> Curve {
        lock(&t.state)
            .factory
            .get(&(mode_index(Profile::Balanced), fan))
            .cloned()
            .unwrap()
    }

    #[test]
    fn bad_curves_are_refused() {
        let fx = Fix::new("th-bad");
        let t = thermal(&fx);
        t.on_profile(Profile::Balanced, false);
        let mut c = fx_factory(&t, 1);
        c.0[3].0 = 10;
        assert!(t.set_fan(Profile::Balanced, 1, Setting::Custom(c)).is_err());
        assert!(t.set_fan(Profile::Balanced, 9, Setting::Full).is_err());
        // Nothing was stored.
        assert_eq!(t.setting(Profile::Balanced, 1), Setting::Auto);
    }

    #[test]
    fn settings_survive_a_restart() {
        let fx = Fix::new("th-persist");
        on_ac(&fx);
        {
            let t = thermal(&fx);
            t.set_fan(Profile::PowerSaver, 0, Setting::Full).unwrap();
            t.set_limit(Profile::Balanced, false, "ppt_pl1_spl", Some(50))
                .unwrap();
            t.set_limit(Profile::Balanced, true, "ppt_pl1_spl", Some(30))
                .unwrap();
        }
        let t = thermal(&fx);
        assert_eq!(t.setting(Profile::PowerSaver, 3), Setting::Full);
        let st = lock(&t.state);
        assert_eq!(st.custom.get(&(1, false, "ppt_pl1_spl".into())), Some(&50));
        assert_eq!(st.custom.get(&(1, true, "ppt_pl1_spl".into())), Some(&30));
    }

    #[test]
    fn power_limits_follow_mode_and_source() {
        let fx = Fix::new("th-power");
        on_ac(&fx);
        let t = thermal(&fx);
        t.on_profile(Profile::Balanced, false);
        t.set_limit(Profile::Balanced, false, "ppt_pl1_spl", Some(45))
            .unwrap();
        t.set_limit(Profile::Balanced, true, "ppt_pl1_spl", Some(30))
            .unwrap();
        assert_eq!(fx.get(&format!("{A}/ppt_pl1_spl/current_value")), "45");
        // Unplugged: the battery value applies.
        t.on_profile(Profile::Balanced, true);
        assert_eq!(fx.get(&format!("{A}/ppt_pl1_spl/current_value")), "30");
        // Values outside the kernel's bounds are refused for the source in use.
        assert!(
            t.set_limit(Profile::Balanced, true, "ppt_pl1_spl", Some(500))
                .is_err()
        );
        assert!(t.set_limit(Profile::Balanced, true, "nope", Some(5)).is_err());
        // Another mode without custom values leaves the limits alone.
        t.on_profile(Profile::Performance, true);
        assert_eq!(fx.get(&format!("{A}/ppt_pl1_spl/current_value")), "30");
    }

    #[test]
    fn custom_power_pins_a_fan_curve_on_models_that_need_one() {
        let fx = Fix::new("th-pin");
        on_ac(&fx);
        assert!(thermal(&fx).quirks.power_needs_fan_curve);
        let t = thermal(&fx);
        t.on_profile(Profile::Balanced, false);
        assert!(!t.fans.as_ref().unwrap().any_custom(&fx.root()));
        t.set_limit(Profile::Balanced, false, "ppt_pl1_spl", Some(60))
            .unwrap();
        // Every fan now runs the firmware's own curve as an explicit one.
        for n in 1..=3 {
            assert_eq!(fx.get(&format!("{HW}/pwm{n}_enable")), "1", "fan {n}");
        }
        // Back to default: the thermal mode is reloaded, fans return to Auto.
        t.set_limit(Profile::Balanced, false, "ppt_pl1_spl", None)
            .unwrap();
        assert_eq!(fx.get(&format!("{HW}/pwm1_enable")), "2");
        // The kernel's own notion of the limit is back at the default too.
        assert_eq!(fx.get(&format!("{A}/ppt_pl1_spl/current_value")), "90");
    }

    #[test]
    fn reset_forgets_the_custom_curve() {
        let fx = Fix::new("th-reset");
        let t = thermal(&fx);
        t.on_profile(Profile::Balanced, false);
        t.set_fan(Profile::Balanced, 1, Setting::Full).unwrap();
        assert_eq!(fx.get(&format!("{HW}/pwm1_enable")), "1");
        t.reset_fan(Profile::Balanced, 1).unwrap();
        assert_eq!(t.setting(Profile::Balanced, 1), Setting::Auto);
        // The firmware curve is back (the fake can't mimic the kernel's 3 -> 2).
        assert_ne!(fx.get(&format!("{HW}/pwm1_enable")), "1");
    }
}
