//! `org.tspmo.Lighting1`: keyboard brightness, effects, per-power-source profiles, power-state
//! lighting and an idle timeout.
//!
//! The Aura controller is written to per change (open, write, close): holding the hidraw node
//! open would keep the USB device awake.

use super::thermal::lock;
use super::{PATH, authorize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tspmo_core::asus::lighting::{self, Caps, Direction, Effect, Mode, PowerStates, Rgb, Speed};
use tspmo_sys::{SysRoot, power_supply};
use zbus::message::Header;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Profile {
    brightness: Option<u8>,
    effect: Option<Effect>,
}

#[derive(Debug)]
struct State {
    /// Index 0 is AC, 1 is battery.
    profiles: [Profile; 2],
    on_battery: bool,
    /// What the keyboard shows now.
    brightness: u8,
    effect: Option<Effect>,
    power: Option<PowerStates>,
    /// Seconds without input before the backlight goes dark (0: never).
    idle: u32,
    /// The level to restore when input resumes, while the idle timeout has darkened the keyboard.
    dimmed_from: Option<u8>,
}

pub struct Lighting {
    root: SysRoot,
    caps: Caps,
    file: PathBuf,
    st: Mutex<State>,
}

pub type Row = (String, (u8, u8, u8), (u8, u8, u8), String, String);

fn src_index(on_battery: bool) -> usize {
    usize::from(on_battery)
}

fn src_arg(s: &str, current: bool) -> Result<usize, String> {
    match s {
        "" => Ok(src_index(current)),
        "ac" => Ok(0),
        "battery" => Ok(1),
        other => Err(format!("unknown power source '{other}' (ac, battery)")),
    }
}

fn hex(c: Rgb) -> String {
    format!("{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn unhex(s: &str) -> Option<Rgb> {
    let v = u32::from_str_radix(s, 16).ok().filter(|_| s.len() == 6)?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

impl Lighting {
    /// `None` without a keyboard backlight.
    pub fn new(root: SysRoot, dir: PathBuf) -> Option<Self> {
        let caps = lighting::detect(&root);
        let bl = caps.backlight.clone()?;
        let brightness = bl.get(&root).unwrap_or(0);
        let l = Self {
            file: dir.join("lighting"),
            st: Mutex::new(State {
                profiles: [Profile::default(); 2],
                on_battery: power_supply::on_ac(&root) == Some(false),
                brightness,
                effect: None,
                power: None,
                idle: 0,
                dimmed_from: None,
            }),
            caps,
            root,
        };
        l.load();
        Some(l)
    }

    pub fn has_effects(&self) -> bool {
        self.caps.aura.is_some()
    }

    fn load(&self) {
        let Ok(text) = std::fs::read_to_string(&self.file) else {
            return;
        };
        let mut st = lock(&self.st);
        for line in text.lines() {
            let w: Vec<&str> = line.split_whitespace().collect();
            match w[..] {
                ["brightness", src, v] => {
                    if let (Ok(i), Ok(v)) = (src_arg(src, false), v.parse::<u8>()) {
                        st.profiles[i].brightness = Some(v.min(self.max()));
                    }
                }
                ["effect", src, mode, c1, c2, speed, dir] => {
                    let e = (|| {
                        Some(Effect {
                            mode: Mode::parse(mode).filter(|m| self.caps.modes.contains(m))?,
                            colour: unhex(c1)?,
                            colour2: unhex(c2)?,
                            speed: Speed::parse(speed)?,
                            direction: Direction::parse(dir)?,
                        })
                    })();
                    if let (Ok(i), Some(e)) = (src_arg(src, false), e) {
                        st.profiles[i].effect = Some(e);
                    }
                }
                ["power", b, a, s, d] => {
                    st.power = Some(PowerStates {
                        boot: b == "1",
                        awake: a == "1",
                        sleep: s == "1",
                        shutdown: d == "1",
                    });
                }
                ["idle", n] => st.idle = n.parse().unwrap_or(0),
                _ => {}
            }
        }
    }

    fn save(&self, st: &State) {
        let mut t = String::from("# tspmod keyboard lighting (managed by tspmod)\n");
        for (i, name) in ["ac", "battery"].iter().enumerate() {
            let p = &st.profiles[i];
            if let Some(b) = p.brightness {
                t.push_str(&format!("brightness {name} {b}\n"));
            }
            if let Some(e) = p.effect {
                t.push_str(&format!(
                    "effect {name} {} {} {} {} {}\n",
                    e.mode.name(),
                    hex(e.colour),
                    hex(e.colour2),
                    e.speed.name(),
                    e.direction.name()
                ));
            }
        }
        if let Some(p) = st.power {
            t.push_str(&format!(
                "power {} {} {} {}\n",
                u8::from(p.boot),
                u8::from(p.awake),
                u8::from(p.sleep),
                u8::from(p.shutdown)
            ));
        }
        t.push_str(&format!("idle {}\n", st.idle));
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.file.with_extension("tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }

    pub fn max(&self) -> u8 {
        self.caps.backlight.as_ref().map_or(0, |b| b.max)
    }

    // --- hardware ----------------------------------------------------------------------------

    fn write_brightness(&self, level: u8) {
        if let Some(bl) = &self.caps.backlight {
            if let Err(e) = bl.set(&self.root, level) {
                eprintln!("tspmod: keyboard brightness: {e}");
            }
        }
    }

    fn write_effect(&self, e: &Effect) {
        if let Some(hid) = &self.caps.aura {
            if let Err(err) = lighting::write_effect(&self.root, hid, e) {
                eprintln!("tspmod: keyboard effect: {err}");
            }
        }
    }

    fn write_power(&self, p: PowerStates) {
        if let Some(hid) = &self.caps.aura {
            if let Err(err) = lighting::write_power_states(&self.root, hid, p) {
                eprintln!("tspmod: keyboard power lighting: {err}");
            }
        }
    }

    /// Bring the keyboard to the profile of the current power source (start-up, resume, plug).
    pub fn apply(&self) {
        let (p, power) = {
            let mut st = lock(&self.st);
            st.dimmed_from = None;
            (st.profiles[src_index(st.on_battery)], st.power)
        };
        if let Some(power) = power {
            self.write_power(power);
        }
        if let Some(e) = p.effect {
            self.write_effect(&e);
            lock(&self.st).effect = Some(e);
        }
        if let Some(b) = p.brightness {
            self.write_brightness(b);
        }
        let live = self
            .caps
            .backlight
            .as_ref()
            .and_then(|b| b.get(&self.root))
            .unwrap_or(0);
        lock(&self.st).brightness = live;
    }

    pub fn on_power_source(&self, on_battery: bool) {
        lock(&self.st).on_battery = on_battery;
        self.apply();
    }

    /// The firmware or a key changed the backlight level: it becomes the level of the current
    /// power source.
    pub fn brightness_changed(&self) -> bool {
        let Some(level) = self.caps.backlight.as_ref().and_then(|b| b.get(&self.root)) else {
            return false;
        };
        let mut st = lock(&self.st);
        if st.dimmed_from.is_some() {
            // Our own idle dimming.
            return false;
        }
        if st.brightness == level {
            return false;
        }
        st.brightness = level;
        let i = src_index(st.on_battery);
        st.profiles[i].brightness = Some(level);
        self.save(&st);
        true
    }

    // --- changes -----------------------------------------------------------------------------

    /// `source`: `""` for the current one.
    pub fn set_brightness(&self, source: &str, level: u8) -> Result<(), String> {
        if level > self.max() {
            return Err(format!("brightness must be 0-{}", self.max()));
        }
        let live = {
            let mut st = lock(&self.st);
            let i = src_arg(source, st.on_battery)?;
            st.profiles[i].brightness = Some(level);
            self.save(&st);
            i == src_index(st.on_battery)
        };
        if live {
            self.write_brightness(level);
            let mut st = lock(&self.st);
            st.brightness = level;
            st.dimmed_from = None;
        }
        Ok(())
    }

    pub fn set_effect(&self, source: &str, e: Effect) -> Result<(), String> {
        if !self.caps.modes.contains(&e.mode) {
            return Err(if self.caps.modes.is_empty() {
                "no lighting effects on this keyboard".into()
            } else {
                format!(
                    "effect must be one of {}",
                    self.caps
                        .modes
                        .iter()
                        .map(|m| m.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
        }
        let live = {
            let mut st = lock(&self.st);
            let i = src_arg(source, st.on_battery)?;
            st.profiles[i].effect = Some(e);
            self.save(&st);
            i == src_index(st.on_battery)
        };
        if live {
            self.write_effect(&e);
            lock(&self.st).effect = Some(e);
        }
        Ok(())
    }

    pub fn set_power_states(&self, p: PowerStates) -> Result<(), String> {
        if !self.has_effects() {
            return Err("no power-state lighting on this keyboard".into());
        }
        {
            let mut st = lock(&self.st);
            st.power = Some(p);
            self.save(&st);
        }
        self.write_power(p);
        Ok(())
    }

    pub fn set_idle(&self, secs: u32) {
        let mut st = lock(&self.st);
        st.idle = secs;
        self.save(&st);
    }

    pub fn idle_timeout(&self) -> u32 {
        lock(&self.st).idle
    }

    /// Next or previous effect (the Aura keys). Returns its name.
    pub fn cycle_effect(&self, forward: bool) -> Result<&'static str, String> {
        let modes = &self.caps.modes;
        let cur = lock(&self.st).effect.unwrap_or_default();
        let n = modes.len();
        let next = modes
            .iter()
            .position(|m| *m == cur.mode)
            .map_or(modes.first(), |i| {
                modes.get(if forward { (i + 1) % n } else { (i + n - 1) % n })
            })
            .copied()
            .ok_or("no lighting effects on this keyboard")?;
        self.set_effect("", Effect { mode: next, ..cur })?;
        Ok(next.name())
    }

    /// Next brightness level, wrapping to off after the brightest. Returns the new level.
    pub fn cycle_brightness(&self) -> u8 {
        let cur = lock(&self.st).brightness;
        let next = if cur >= self.max() { 0 } else { cur + 1 };
        let _ = self.set_brightness("", next);
        next
    }

    /// The idle timeout fired: darken the keyboard, remembering the level.
    pub fn idle_dim(&self) {
        let mut st = lock(&self.st);
        if st.dimmed_from.is_some() || st.brightness == 0 {
            return;
        }
        st.dimmed_from = Some(st.brightness);
        drop(st);
        self.write_brightness(0);
    }

    /// Input after idle dimming: bring the level back. Returns whether anything was restored.
    pub fn idle_wake(&self) -> bool {
        let Some(level) = lock(&self.st).dimmed_from.take() else {
            return false;
        };
        self.write_brightness(level);
        true
    }

    /// Whether the idle timer should run: enabled and the keyboard is lit and not yet dimmed.
    pub fn idle_armed(&self) -> Option<u32> {
        let st = lock(&self.st);
        (st.idle > 0 && st.brightness > 0 && st.dimmed_from.is_none()).then_some(st.idle)
    }

    /// Path of the `brightness_hw_changed` attribute, which signals key presses.
    pub fn hw_changed_path(&self) -> Option<PathBuf> {
        self.caps
            .backlight
            .as_ref()
            .map(|b| self.root.path(format!("{}/brightness_hw_changed", b.dir)))
    }

    fn effect_row(e: &Effect) -> Row {
        (
            e.mode.name().to_owned(),
            (e.colour[0], e.colour[1], e.colour[2]),
            (e.colour2[0], e.colour2[1], e.colour2[2]),
            e.speed.name().to_owned(),
            e.direction.name().to_owned(),
        )
    }
}

pub struct Lighting1(pub Arc<Lighting>);

fn rgb(c: (u8, u8, u8)) -> Rgb {
    [c.0, c.1, c.2]
}

#[zbus::interface(name = "org.tspmo.Lighting1")]
impl Lighting1 {
    #[zbus(property)]
    fn max_brightness(&self) -> u8 {
        self.0.max()
    }

    #[zbus(property)]
    fn brightness(&self) -> u8 {
        lock(&self.0.st).brightness
    }

    /// Effects the keyboard has (empty: brightness only).
    #[zbus(property)]
    fn modes(&self) -> Vec<String> {
        self.0.caps.modes.iter().map(|m| m.name().to_owned()).collect()
    }

    /// The effect showing: `(mode, colour, colour2, speed, direction)`; empty mode if never set.
    #[zbus(property)]
    fn effect(&self) -> Row {
        lock(&self.0.st).effect.map_or(
            (String::new(), (0, 0, 0), (0, 0, 0), String::new(), String::new()),
            |e| Lighting::effect_row(&e),
        )
    }

    #[zbus(property)]
    fn source(&self) -> String {
        if lock(&self.0.st).on_battery {
            "battery"
        } else {
            "ac"
        }
        .to_owned()
    }

    /// `(source, brightness, effect...)` per power source; brightness -1 and an empty mode mean
    /// "leave alone".
    #[zbus(property)]
    fn profiles(&self) -> Vec<(String, i32, Row)> {
        let st = lock(&self.0.st);
        ["ac", "battery"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let p = &st.profiles[i];
                let row = p.effect.map_or(
                    (String::new(), (0, 0, 0), (0, 0, 0), String::new(), String::new()),
                    |e| Lighting::effect_row(&e),
                );
                ((*name).to_owned(), p.brightness.map_or(-1, i32::from), row)
            })
            .collect()
    }

    /// Lit in `(boot, awake, sleep, shutdown)`.
    #[zbus(property)]
    fn power_states(&self) -> (bool, bool, bool, bool) {
        let p = lock(&self.0.st).power.unwrap_or_default();
        (p.boot, p.awake, p.sleep, p.shutdown)
    }

    /// Seconds without input before the backlight goes dark; 0 is never.
    #[zbus(property)]
    fn idle_timeout(&self) -> u32 {
        self.0.idle_timeout()
    }

    /// `source`: `ac`, `battery` or empty for the one in use.
    async fn set_brightness(
        &self,
        source: String,
        level: u8,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LIGHTING).await?;
        self.0
            .set_brightness(&source, level)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    /// `effect`: `(mode, colour, colour2, speed, direction)`.
    async fn set_effect(
        &self,
        source: String,
        effect: Row,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LIGHTING).await?;
        let (mode, colour, colour2, speed, direction) = effect;
        let bad = |what: &str, v: &str| zbus::fdo::Error::InvalidArgs(format!("unknown {what} '{v}'"));
        let e = Effect {
            mode: Mode::parse(&mode).ok_or_else(|| bad("effect", &mode))?,
            colour: rgb(colour),
            colour2: rgb(colour2),
            speed: Speed::parse(&speed).ok_or_else(|| bad("speed", &speed))?,
            direction: Direction::parse(&direction).ok_or_else(|| bad("direction", &direction))?,
        };
        self.0
            .set_effect(&source, e)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_power_states(
        &self,
        boot: bool,
        awake: bool,
        sleep: bool,
        shutdown: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LIGHTING).await?;
        self.0
            .set_power_states(PowerStates {
                boot,
                awake,
                sleep,
                shutdown,
            })
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_idle_timeout(
        &self,
        seconds: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LIGHTING).await?;
        self.0.set_idle(seconds);
        changed(conn).await;
        super::hotkeys::rearm();
        Ok(())
    }

    /// Next (or previous) effect; returns its name.
    async fn cycle_effect(
        &self,
        forward: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<String> {
        authorize(conn, &hdr, super::SET_LIGHTING).await?;
        let name = self
            .0
            .cycle_effect(forward)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(name.to_owned())
    }
}

pub async fn changed(conn: &zbus::Connection) {
    let Ok(r) = conn.object_server().interface::<_, Lighting1>(PATH).await else {
        return;
    };
    let i = r.get().await;
    let e = r.signal_emitter();
    let _ = i.brightness_changed(e).await;
    let _ = i.effect_changed(e).await;
    let _ = i.source_changed(e).await;
    let _ = i.profiles_changed(e).await;
    let _ = i.power_states_changed(e).await;
    let _ = i.idle_timeout_changed(e).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tspmo_core::asus::testfix::Fix;

    const BR: &str = "/sys/class/leds/asus::kbd_backlight/brightness";

    fn light(fx: &Fix) -> Lighting {
        Lighting::new(fx.root(), fx.dir().join("state")).unwrap()
    }

    fn hid(fx: &Fix) {
        let root = fx.root();
        std::fs::create_dir_all(root.path("/dev")).unwrap();
        std::fs::write(root.path("/dev/hidraw2"), b"").unwrap();
    }

    fn written(fx: &Fix) -> Vec<u8> {
        std::fs::read(fx.root().path("/dev/hidraw2")).unwrap()
    }

    #[test]
    fn brightness_per_source() {
        let fx = Fix::new("light-src");
        fx.set("/sys/class/power_supply/ACAD/online", "1\n");
        let l = light(&fx);
        l.set_brightness("battery", 1).unwrap();
        l.set_brightness("ac", 3).unwrap();
        assert_eq!(fx.get(BR), "3");
        // Unplugged: the battery level applies.
        l.on_power_source(true);
        assert_eq!(fx.get(BR), "1");
        l.on_power_source(false);
        assert_eq!(fx.get(BR), "3");
        assert!(l.set_brightness("", 4).is_err());
        assert!(l.set_brightness("usb", 1).is_err());
    }

    #[test]
    fn untouched_means_untouched() {
        let fx = Fix::new("light-default");
        fx.set(BR, "2\n");
        let l = light(&fx);
        l.apply();
        l.on_power_source(true);
        assert_eq!(fx.get(BR), "2");
    }

    #[test]
    fn key_presses_become_the_level_of_the_source() {
        let fx = Fix::new("light-keys");
        fx.set("/sys/class/power_supply/ACAD/online", "1\n");
        fx.set(BR, "3\n");
        let l = light(&fx);
        fx.set(BR, "1\n"); // Fn key lowered it
        assert!(l.brightness_changed());
        assert!(!l.brightness_changed());
        l.on_power_source(true);
        l.on_power_source(false);
        assert_eq!(fx.get(BR), "1");
    }

    #[test]
    fn effect_goes_to_the_controller_and_survives_restart() {
        let fx = Fix::new("light-effect");
        hid(&fx);
        let l = light(&fx);
        l.set_effect(
            "",
            Effect {
                mode: Mode::Breathe,
                colour: [255, 0, 0],
                colour2: [0, 0, 255],
                ..Effect::default()
            },
        )
        .unwrap();
        let w = written(&fx);
        assert_eq!(&w[..2], &[0x5d, 0xb3]);
        assert_eq!(&w[3..7], &[1, 255, 0, 0]);
        assert!(
            l.set_effect(
                "",
                Effect {
                    mode: Mode::Comet,
                    ..Effect::default()
                }
            )
            .is_err()
        );

        let l = light(&fx);
        let st = lock(&l.st);
        let e = st.profiles[src_index(st.on_battery)].effect.unwrap();
        assert_eq!(
            (e.mode, e.colour, e.colour2),
            (Mode::Breathe, [255, 0, 0], [0, 0, 255])
        );
    }

    #[test]
    fn aura_key_cycles_effects_and_wraps() {
        let fx = Fix::new("light-cycle");
        hid(&fx);
        let l = light(&fx);
        let n = l.caps.modes.len();
        let mut seen = Vec::new();
        for _ in 0..n + 1 {
            seen.push(l.cycle_effect(true).unwrap());
        }
        assert_eq!(seen[0], "breathe");
        assert_eq!(seen[n - 1], "static");
        assert_eq!(seen[n], "breathe");
    }

    #[test]
    fn aura_prev_goes_backwards() {
        let fx = Fix::new("light-prev");
        hid(&fx);
        let l = light(&fx);
        assert_eq!(l.cycle_effect(false).unwrap(), "pulse");
        assert_eq!(l.cycle_effect(true).unwrap(), "static");
    }

    #[test]
    fn brightness_cycles_through_off() {
        let fx = Fix::new("light-bcycle");
        fx.set(BR, "2\n");
        let l = light(&fx);
        assert_eq!(l.cycle_brightness(), 3);
        assert_eq!(l.cycle_brightness(), 0);
        assert_eq!(l.cycle_brightness(), 1);
        assert_eq!(fx.get(BR), "1");
    }

    #[test]
    fn power_state_lighting() {
        let fx = Fix::new("light-power");
        hid(&fx);
        let l = light(&fx);
        l.set_power_states(PowerStates {
            boot: false,
            awake: true,
            sleep: false,
            shutdown: false,
        })
        .unwrap();
        assert_eq!(written(&fx)[..7], [0x5d, 0xbd, 0x01, 0b0000_1000, 0, 0, 0]);
    }

    #[test]
    fn idle_dims_and_restores() {
        let fx = Fix::new("light-idle");
        fx.set(BR, "2\n");
        let l = light(&fx);
        assert_eq!(l.idle_armed(), None);
        l.set_idle(30);
        assert_eq!(l.idle_armed(), Some(30));
        l.idle_dim();
        assert_eq!(fx.get(BR), "0");
        assert_eq!(l.idle_armed(), None);
        // Our own dimming must not become the saved level.
        assert!(!l.brightness_changed());
        assert!(l.idle_wake());
        assert_eq!(fx.get(BR), "2");
        assert!(!l.idle_wake());
    }

    #[test]
    fn colours_round_trip() {
        assert_eq!(unhex("ff8000"), Some([255, 128, 0]));
        assert_eq!(unhex("ff80"), None);
        assert_eq!(hex([1, 2, 255]), "0102ff");
    }
}
