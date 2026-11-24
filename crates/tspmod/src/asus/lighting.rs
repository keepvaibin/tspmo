//! `org.tspmo.Lighting1`: keyboard brightness, effects, per-power-source profiles, power-state
//! lighting and an idle timeout.
//!
//! What is offered follows what the keyboard's controller says it is (`Kind`): a one-colour
//! backlight gets no colour choice, a single zone gets no waves. Nothing is sent that the
//! board does not support, and the state shown is the state the keyboard was last told: at
//! start-up and after resume the daemon tells the keyboard everything again (wake-up string,
//! power states, effect, brightness), so the kernel's idea of the LED and the controller's
//! cannot stay apart.
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
    /// The effect the keyboard was last told (the factory's steady white until chosen).
    effect: Effect,
    power: PowerStates,
    /// Seconds without input before the backlight goes dark (0: never).
    idle: u32,
    /// The level to restore when input resumes, while the idle timeout has darkened the keyboard.
    dimmed_from: Option<u8>,
    /// Eco+ is active: the backlight goes dark after [`ECO_IDLE`] seconds at most.
    eco: bool,
}

/// Idle time after which Eco+ darkens the keyboard, seconds.
pub const ECO_IDLE: u32 = 10;

pub struct Lighting {
    root: SysRoot,
    caps: Caps,
    file: PathBuf,
    st: Mutex<State>,
}

/// `(mode, colour, colour2, speed, direction)`: no field is ever empty.
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
        let on_battery = power_supply::on_ac(&root) == Some(false);
        let l = Self {
            file: dir.join("lighting"),
            st: Mutex::new(State {
                profiles: [Profile::default(); 2],
                on_battery,
                brightness,
                effect: Effect::default(),
                power: PowerStates::default(),
                idle: 0,
                dimmed_from: None,
                eco: false,
            }),
            caps,
            root,
        };
        l.load();
        {
            let mut st = lock(&l.st);
            if let Some(e) = st.profiles[src_index(st.on_battery)].effect {
                st.effect = e;
            }
        }
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
                    st.power = PowerStates {
                        boot: b == "1",
                        awake: a == "1",
                        sleep: s == "1",
                        shutdown: d == "1",
                    };
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
        let p = st.power;
        t.push_str(&format!(
            "power {} {} {} {}\n",
            u8::from(p.boot),
            u8::from(p.awake),
            u8::from(p.sleep),
            u8::from(p.shutdown)
        ));
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
            if let Err(err) = lighting::write_effect(&self.root, hid, self.caps.kind, e, true) {
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

    /// Tell the keyboard everything: the profile of the current power source (start-up, resume,
    /// plug). The controller gets its wake-up string, the power states and the effect; the LED
    /// gets its level written even if it already reads the same.
    pub fn apply(&self) {
        let (p, power, effect) = {
            let mut st = lock(&self.st);
            st.dimmed_from = None;
            let p = st.profiles[src_index(st.on_battery)];
            if let Some(e) = p.effect {
                st.effect = e;
            }
            (p, st.power, st.effect)
        };
        if let Some(hid) = &self.caps.aura {
            if let Err(err) = lighting::write_all(&self.root, hid, self.caps.kind, power, &effect) {
                eprintln!("tspmod: keyboard: {err}");
            }
        }
        let live = self
            .caps
            .backlight
            .as_ref()
            .and_then(|b| b.get(&self.root))
            .unwrap_or(0);
        let level = p.brightness.unwrap_or(live).min(self.max());
        self.write_brightness(level);
        lock(&self.st).brightness = level;
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

    fn modes_list(&self) -> String {
        self.caps
            .modes
            .iter()
            .map(|m| m.name())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Set an effect from its parts as a client gives them. An empty speed or direction keeps
    /// the current one; the mode must be one this keyboard has, and a colour effect needs a
    /// colour that is not black (to turn the light off, lower the brightness).
    pub fn set_effect_row(&self, source: &str, row: Row) -> Result<(), String> {
        let (mode, c1, c2, speed, dir) = row;
        if self.caps.modes.is_empty() {
            return Err("this keyboard has no lighting effects (brightness only)".into());
        }
        let cur = lock(&self.st).effect;
        let m = Mode::parse(&mode)
            .filter(|m| self.caps.modes.contains(m))
            .ok_or_else(|| {
                format!(
                    "effect '{mode}' is not available on this keyboard (use {})",
                    self.modes_list()
                )
            })?;
        let speed = if speed.is_empty() {
            cur.speed
        } else {
            Speed::parse(&speed).ok_or_else(|| format!("speed '{speed}' is not low, med or high"))?
        };
        let direction = if dir.is_empty() {
            cur.direction
        } else {
            Direction::parse(&dir)
                .ok_or_else(|| format!("direction '{dir}' is not right, left, up or down"))?
        };
        let e = Effect {
            mode: m,
            colour: [c1.0, c1.1, c1.2],
            colour2: [c2.0, c2.1, c2.2],
            speed,
            direction,
        };
        self.set_effect(source, e)
    }

    pub fn set_effect(&self, source: &str, e: Effect) -> Result<(), String> {
        if !self.caps.modes.contains(&e.mode) {
            return Err(if self.caps.modes.is_empty() {
                "this keyboard has no lighting effects (brightness only)".into()
            } else {
                format!(
                    "effect '{}' is not available on this keyboard (use {})",
                    e.mode.name(),
                    self.modes_list()
                )
            });
        }
        if self.caps.kind.has_colour() && e.mode.uses_colour() && e.colour == [0, 0, 0] {
            return Err("the colour is black; lower the brightness to turn the light off".into());
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
            lock(&self.st).effect = e;
        }
        Ok(())
    }

    pub fn set_power_states(&self, p: PowerStates) -> Result<(), String> {
        if !self.has_effects() {
            return Err("no power-state lighting on this keyboard".into());
        }
        {
            let mut st = lock(&self.st);
            st.power = p;
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
        let cur = lock(&self.st).effect;
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
        let secs = match (st.idle, st.eco) {
            (0, true) => ECO_IDLE,
            (n, true) => n.min(ECO_IDLE),
            (n, false) => n,
        };
        (secs > 0 && st.brightness > 0 && st.dimmed_from.is_none()).then_some(secs)
    }

    /// Eco+ starts or ends. Ending it brings back a keyboard that Eco+ darkened (unless the
    /// user's own idle timeout has also run out, which `idle_armed` will say).
    pub fn set_eco(&self, eco: bool) {
        let was = std::mem::replace(&mut lock(&self.st).eco, eco);
        if was && !eco {
            self.idle_wake();
        }
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

    /// What the backlight is, from the controller's own answer: `single-zone-white`,
    /// `single-zone-rgb`, `multi-zone-rgb`, `per-key-rgb`, `unknown` (controller silent), or
    /// `brightness-only` (no Aura controller).
    #[zbus(property)]
    fn kind(&self) -> String {
        if self.0.has_effects() {
            self.0.caps.kind.name()
        } else {
            "brightness-only"
        }
        .to_owned()
    }

    /// Effects the keyboard has (empty: brightness only).
    #[zbus(property)]
    fn modes(&self) -> Vec<String> {
        self.0.caps.modes.iter().map(|m| m.name().to_owned()).collect()
    }

    /// What each effect takes, `(mode, colour, colour2, speed, direction)`: whether the field
    /// has an effect on it. Rainbow effects bring their own colours, a one-colour keyboard has
    /// no colour at all.
    #[zbus(property)]
    fn mode_params(&self) -> Vec<(String, bool, bool, bool, bool)> {
        let colour = self.0.caps.kind.has_colour();
        self.0
            .caps
            .modes
            .iter()
            .map(|m| {
                (
                    m.name().to_owned(),
                    colour && m.uses_colour(),
                    colour && m.uses_second_colour(),
                    m.uses_speed(),
                    m.uses_direction(),
                )
            })
            .collect()
    }

    /// The speeds an effect can have.
    #[zbus(property)]
    fn speeds(&self) -> Vec<String> {
        Speed::ALL.iter().map(|s| s.name().to_owned()).collect()
    }

    /// The effect showing: `(mode, colour, colour2, speed, direction)`. Before one is chosen,
    /// the factory's: steady white.
    #[zbus(property)]
    fn effect(&self) -> Row {
        Lighting::effect_row(&lock(&self.0.st).effect)
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

    /// `(source, brightness, effect)` per power source: what applies when that source is in use
    /// (the level and effect chosen for it, or what the keyboard shows now when none was).
    #[zbus(property)]
    fn profiles(&self) -> Vec<(String, u8, Row)> {
        let st = lock(&self.0.st);
        ["ac", "battery"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let p = &st.profiles[i];
                (
                    (*name).to_owned(),
                    p.brightness.unwrap_or(st.brightness),
                    Lighting::effect_row(&p.effect.unwrap_or(st.effect)),
                )
            })
            .collect()
    }

    /// Lit in `(boot, awake, sleep, shutdown)`.
    #[zbus(property)]
    fn power_states(&self) -> (bool, bool, bool, bool) {
        let p = lock(&self.0.st).power;
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

    /// `effect`: `(mode, colour, colour2, speed, direction)`. The mode must be one of `Modes`;
    /// an empty speed or direction keeps the current one.
    async fn set_effect(
        &self,
        source: String,
        effect: Row,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LIGHTING).await?;
        self.0
            .set_effect_row(&source, effect)
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
        assert_eq!(written(&fx)[..8], [0x5d, 0xbd, 0x01, 0b0000_1000, 0, 0, 0, 0xff]);
    }

    #[test]
    fn apply_tells_the_keyboard_everything() {
        let fx = Fix::new("light-apply");
        hid(&fx);
        fx.set(BR, "2\n");
        let l = light(&fx);
        l.apply();
        let w = written(&fx);
        // wake-up string, power states, effect, set: 4 reports of 63 bytes.
        assert_eq!(w.len(), 4 * 63);
        assert_eq!(&w[..5], &[0x5d, b'A', b'S', b'U', b'S']);
        assert_eq!(&w[63..68], &[0x5d, 0xbd, 1, 0xaa, 0]);
        // The factory's steady white until something else is chosen.
        assert_eq!(&w[126..134], &[0x5d, 0xb3, 0, 0, 255, 255, 255, 0xeb]);
        assert_eq!(&w[189..191], &[0x5d, 0xb5]);
        assert_eq!(fx.get(BR), "2");
    }

    #[test]
    fn the_effect_is_never_empty() {
        let fx = Fix::new("light-never-empty");
        let l = light(&fx);
        let i = Lighting1(Arc::new(l));
        let e = i.effect();
        assert_eq!(e.0, "static");
        assert!(!e.3.is_empty() && !e.4.is_empty());
        assert!(i.modes().iter().all(|m| !m.is_empty()));
        for (name, br, row) in i.profiles() {
            assert!(!name.is_empty() && !row.0.is_empty(), "{br}");
        }
        assert!(i.speeds().iter().all(|s| !s.is_empty()));
        assert!(!i.kind().is_empty());
    }

    #[test]
    fn clients_may_leave_speed_and_direction_empty_but_not_the_mode() {
        let fx = Fix::new("light-lenient");
        hid(&fx);
        let l = light(&fx);
        l.set_effect_row(
            "",
            (
                "breathe".into(),
                (255, 0, 0),
                (0, 0, 255),
                "high".into(),
                "".into(),
            ),
        )
        .unwrap();
        // Speed kept from the effect before.
        l.set_effect_row("", ("pulse".into(), (0, 255, 0), (0, 0, 0), "".into(), "".into()))
            .unwrap();
        let st = lock(&l.st);
        assert_eq!((st.effect.mode, st.effect.speed), (Mode::Pulse, Speed::High));
        drop(st);
        let err = l
            .set_effect_row("", ("".into(), (1, 1, 1), (0, 0, 0), "".into(), "".into()))
            .unwrap_err();
        assert!(err.contains("not available") && err.contains("static"), "{err}");
        assert!(
            l.set_effect_row(
                "",
                ("static".into(), (1, 1, 1), (0, 0, 0), "warp".into(), "".into())
            )
            .is_err()
        );
    }

    #[test]
    fn black_is_not_a_colour() {
        let fx = Fix::new("light-black");
        hid(&fx);
        let l = light(&fx);
        let before = written(&fx).len();
        let e = Effect {
            colour: [0, 0, 0],
            ..Effect::default()
        };
        assert!(l.set_effect("", e).unwrap_err().contains("brightness"));
        assert_eq!(written(&fx).len(), before, "nothing sent");
    }

    #[test]
    fn brightness_is_written_even_when_it_reads_the_same() {
        let fx = Fix::new("light-forced");
        fx.set(BR, "3\n");
        let l = light(&fx);
        let path = fx.root().path(BR);
        std::fs::remove_file(&path).unwrap();
        // If the level is "already right" nothing may be skipped: the file comes back.
        l.set_brightness("", 3).unwrap();
        assert_eq!(fx.get(BR), "3");
    }

    #[test]
    fn eco_plus_darkens_the_keyboard_after_ten_seconds_and_leaving_it_brings_it_back() {
        let fx = Fix::new("light-eco");
        fx.set(BR, "2\n");
        let l = light(&fx);
        assert_eq!(l.idle_armed(), None);
        l.set_eco(true);
        assert_eq!(l.idle_armed(), Some(ECO_IDLE));
        // The user's own shorter timeout still wins.
        l.set_idle(5);
        assert_eq!(l.idle_armed(), Some(5));
        l.set_idle(0);
        l.idle_dim();
        assert_eq!(fx.get(BR), "0");
        l.set_eco(false);
        assert_eq!(fx.get(BR), "2");
        assert_eq!(l.idle_armed(), None);
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
