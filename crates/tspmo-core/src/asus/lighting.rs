//! Keyboard lighting: brightness through the LED class (the `hid-asus` driver talks to the
//! keyboard for us), effects and power-state lighting through the keyboard's Aura protocol on
//! hidraw (report `0x5d`).
//!
//! The packet layouts were studied in asusctl's `rog-aura` and rewritten here as plain
//! fixed-size builders: nothing allocates.

use super::hid::{self, ASUS_VENDOR, HidRaw};
use std::io;
use tspmo_sys::SysRoot;

pub const REPORT: u8 = 0x5d;
/// USB ids of the Aura keyboard controllers of 2021+ laptops.
pub const KEYBOARD_PRODUCTS: &[u16] = &[0x19b6, 0x1a30, 0x1ce6, 0x18c6];
pub const LEDS: &str = "/sys/class/leds";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Static,
    Breathe,
    RainbowCycle,
    RainbowWave,
    Star,
    Rain,
    Highlight,
    Laser,
    Ripple,
    Pulse,
    Comet,
    Flash,
}

impl Mode {
    pub const ALL: [Mode; 12] = [
        Mode::Static,
        Mode::Breathe,
        Mode::RainbowCycle,
        Mode::RainbowWave,
        Mode::Star,
        Mode::Rain,
        Mode::Highlight,
        Mode::Laser,
        Mode::Ripple,
        Mode::Pulse,
        Mode::Comet,
        Mode::Flash,
    ];

    /// The number the firmware uses.
    pub fn id(self) -> u8 {
        match self {
            Self::Static => 0,
            Self::Breathe => 1,
            Self::RainbowCycle => 2,
            Self::RainbowWave => 3,
            Self::Star => 4,
            Self::Rain => 5,
            Self::Highlight => 6,
            Self::Laser => 7,
            Self::Ripple => 8,
            Self::Pulse => 10,
            Self::Comet => 11,
            Self::Flash => 12,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Breathe => "breathe",
            Self::RainbowCycle => "rainbow-cycle",
            Self::RainbowWave => "rainbow-wave",
            Self::Star => "stars",
            Self::Rain => "rain",
            Self::Highlight => "highlight",
            Self::Laser => "laser",
            Self::Ripple => "ripple",
            Self::Pulse => "pulse",
            Self::Comet => "comet",
            Self::Flash => "flash",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace('_', "-");
        Self::ALL
            .into_iter()
            .find(|m| m.name() == s || (s == "star" && *m == Self::Star))
    }

    pub fn uses_colour(self) -> bool {
        !matches!(self, Self::RainbowCycle | Self::RainbowWave | Self::Rain)
    }

    pub fn uses_second_colour(self) -> bool {
        matches!(self, Self::Breathe | Self::Star)
    }

    pub fn uses_speed(self) -> bool {
        !matches!(self, Self::Static)
    }

    pub fn uses_direction(self) -> bool {
        matches!(self, Self::RainbowWave)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    Low,
    Med,
    High,
}

impl Speed {
    fn byte(self) -> u8 {
        match self {
            Self::Low => 0xe1,
            Self::Med => 0xeb,
            Self::High => 0xf5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Med => "med",
            Self::High => "high",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" | "slow" => Some(Self::Low),
            "med" | "medium" => Some(Self::Med),
            "high" | "fast" => Some(Self::High),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Right,
    Left,
    Up,
    Down,
}

impl Direction {
    fn byte(self) -> u8 {
        match self {
            Self::Right => 0,
            Self::Left => 1,
            Self::Up => 2,
            Self::Down => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Left => "left",
            Self::Up => "up",
            Self::Down => "down",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "right" => Some(Self::Right),
            "left" => Some(Self::Left),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            _ => None,
        }
    }
}

pub type Rgb = [u8; 3];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effect {
    pub mode: Mode,
    pub colour: Rgb,
    pub colour2: Rgb,
    pub speed: Speed,
    pub direction: Direction,
}

impl Default for Effect {
    fn default() -> Self {
        Self {
            mode: Mode::Static,
            colour: [255, 255, 255],
            colour2: [0, 0, 0],
            speed: Speed::Med,
            direction: Direction::Right,
        }
    }
}

/// `5d b3 zone mode r g b speed direction 00 r2 g2 b2`, the 17-byte effect message.
pub fn effect_packet(e: &Effect) -> [u8; 17] {
    let mut p = [0u8; 17];
    p[0] = REPORT;
    p[1] = 0xb3;
    // p[2] is the zone: 0 for keyboards with a single zone.
    p[3] = e.mode.id();
    p[4..7].copy_from_slice(&e.colour);
    p[7] = e.speed.byte();
    p[8] = e.direction.byte();
    p[10..13].copy_from_slice(&e.colour2);
    p
}

/// Ends an effect message: show it.
pub const SET: [u8; 17] = [REPORT, 0xb5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// Ends a sequence: keep it across power cycles.
pub const APPLY: [u8; 17] = [REPORT, 0xb4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// Which power states light the keyboard (`5d bd 01` + bit mask).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerStates {
    pub boot: bool,
    pub awake: bool,
    pub sleep: bool,
    pub shutdown: bool,
}

impl Default for PowerStates {
    fn default() -> Self {
        Self {
            boot: true,
            awake: true,
            sleep: true,
            shutdown: true,
        }
    }
}

impl PowerStates {
    /// The keyboard zone's bits: boot 1, awake 3, sleep 5, shutdown 7.
    pub fn mask(self) -> u8 {
        u8::from(self.boot) << 1
            | u8::from(self.awake) << 3
            | u8::from(self.sleep) << 5
            | u8::from(self.shutdown) << 7
    }
}

pub fn power_packet(p: PowerStates) -> [u8; 7] {
    [REPORT, 0xbd, 0x01, p.mask(), 0, 0, 0]
}

/// Effects the Aura controller accepts. Every 2021+ controller has these five (asusctl's
/// data for the GU605M agrees); reactive and star effects need per-model data we don't have.
pub fn supported_modes() -> Vec<Mode> {
    vec![
        Mode::Static,
        Mode::Breathe,
        Mode::RainbowCycle,
        Mode::RainbowWave,
        Mode::Pulse,
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlight {
    /// Logical directory of the LED, e.g. `/sys/class/leds/asus::kbd_backlight`.
    pub dir: String,
    pub max: u8,
}

pub fn backlight(root: &SysRoot) -> Option<Backlight> {
    root.list(LEDS)
        .into_iter()
        .find(|n| n.ends_with("kbd_backlight"))
        .map(|n| format!("{LEDS}/{n}"))
        .and_then(|dir| {
            let max = root.read_parse(format!("{dir}/max_brightness"))?;
            Some(Backlight { dir, max })
        })
}

impl Backlight {
    pub fn get(&self, root: &SysRoot) -> Option<u8> {
        root.read_parse(format!("{}/brightness", self.dir))
    }

    pub fn set(&self, root: &SysRoot, level: u8) -> io::Result<()> {
        if level > self.max {
            return Err(io::Error::other(format!("brightness must be 0-{}", self.max)));
        }
        if self.get(root) == Some(level) {
            return Ok(());
        }
        root.write(format!("{}/brightness", self.dir), &level.to_string())
    }
}

/// What the keyboard can do on this machine.
#[derive(Debug, Clone)]
pub struct Caps {
    pub backlight: Option<Backlight>,
    /// The Aura controller on USB: effects and power-state lighting.
    pub aura: Option<HidRaw>,
    pub modes: Vec<Mode>,
}

pub fn detect(root: &SysRoot) -> Caps {
    let aura = KEYBOARD_PRODUCTS
        .iter()
        .find_map(|p| hid::find(root, ASUS_VENDOR, *p, REPORT));
    let modes = if aura.is_some() {
        supported_modes()
    } else {
        Vec::new()
    };
    Caps {
        backlight: backlight(root),
        aura,
        modes,
    }
}

/// Show an effect: effect, set, apply. One open of the device.
pub fn write_effect(root: &SysRoot, hid: &HidRaw, e: &Effect) -> io::Result<()> {
    let packets = [effect_packet(e), SET, APPLY].map(|p| hid.padded(REPORT, &p));
    hid::write(root, hid, &packets)
}

pub fn write_power_states(root: &SysRoot, hid: &HidRaw, p: PowerStates) -> io::Result<()> {
    hid::write(root, hid, &[hid.padded(REPORT, &power_packet(p))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asus::testfix::Fix;

    #[test]
    fn static_effect_packet() {
        let e = Effect {
            mode: Mode::Static,
            colour: [0xff, 0x11, 0xdd],
            ..Effect::default()
        };
        assert_eq!(
            effect_packet(&e),
            [
                0x5d, 0xb3, 0, 0, 0xff, 0x11, 0xdd, 0xeb, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
    }

    #[test]
    fn rainbow_wave_packet_carries_speed_direction_and_second_colour() {
        let e = Effect {
            mode: Mode::RainbowWave,
            colour: [1, 2, 3],
            colour2: [4, 5, 6],
            speed: Speed::High,
            direction: Direction::Down,
        };
        let p = effect_packet(&e);
        assert_eq!(&p[..4], &[0x5d, 0xb3, 0, 3]);
        assert_eq!(&p[4..9], &[1, 2, 3, 0xf5, 3]);
        assert_eq!(&p[10..13], &[4, 5, 6]);
    }

    #[test]
    fn power_masks() {
        assert_eq!(PowerStates::default().mask(), 0b1010_1010);
        let only_awake = PowerStates {
            boot: false,
            awake: true,
            sleep: false,
            shutdown: false,
        };
        assert_eq!(power_packet(only_awake), [0x5d, 0xbd, 0x01, 0b0000_1000, 0, 0, 0]);
        let none = PowerStates {
            boot: false,
            awake: false,
            sleep: false,
            shutdown: false,
        };
        assert_eq!(none.mask(), 0);
    }

    #[test]
    fn mode_numbers_and_names() {
        assert_eq!(Mode::Pulse.id(), 10);
        assert_eq!(Mode::parse("Rainbow_Wave"), Some(Mode::RainbowWave));
        assert_eq!(Mode::parse("star"), Some(Mode::Star));
        assert_eq!(Mode::parse("nope"), None);
        assert!(Mode::Breathe.uses_second_colour());
        assert!(!Mode::RainbowCycle.uses_colour());
        assert!(Mode::RainbowWave.uses_direction());
        let ids: std::collections::HashSet<_> = Mode::ALL.iter().map(|m| m.id()).collect();
        assert_eq!(ids.len(), Mode::ALL.len());
    }

    #[test]
    fn detects_the_fixture_keyboard() {
        let fx = Fix::new("light-detect");
        let c = detect(&fx.root());
        let bl = c.backlight.unwrap();
        assert_eq!(bl.max, 3);
        assert_eq!(c.aura.unwrap().product, 0x19b6);
        assert!(c.modes.contains(&Mode::Static));
    }

    #[test]
    fn brightness_is_bounded() {
        let fx = Fix::new("light-bright");
        let root = fx.root();
        let bl = backlight(&root).unwrap();
        bl.set(&root, 2).unwrap();
        assert_eq!(bl.get(&root), Some(2));
        assert!(bl.set(&root, 4).is_err());
    }

    #[test]
    fn effect_is_written_in_three_packets() {
        let fx = Fix::new("light-write");
        let root = fx.root();
        let hid = detect(&root).aura.unwrap();
        std::fs::create_dir_all(root.path("/dev")).unwrap();
        std::fs::write(root.path("/dev/hidraw2"), b"").unwrap();
        write_effect(&root, &hid, &Effect::default()).unwrap();
        // A regular file stands in for the device: the three 63-byte reports land back to back.
        let bytes = std::fs::read(root.path("/dev/hidraw2")).unwrap();
        assert_eq!(bytes.len(), 3 * 63);
        assert_eq!(&bytes[..2], &[0x5d, 0xb3]);
        assert_eq!(&bytes[63..65], &[0x5d, 0xb5]);
        assert_eq!(&bytes[126..128], &[0x5d, 0xb4]);
    }
}
