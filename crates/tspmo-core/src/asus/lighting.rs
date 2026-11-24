//! Keyboard lighting: brightness through the LED class (asus-wmi forwards it to the `hid-asus`
//! driver, which talks to the keyboard for us), effects and power-state lighting through the
//! keyboard's Aura protocol on hidraw (report `0x5d`).
//!
//! The packet layouts were studied in asusctl's `rog-aura` and G-Helper's `Aura.cs` and
//! rewritten here as plain builders. What a keyboard can do is not guessed from its model: the
//! controller answers a capability query (see [`probe`]) and the effects offered follow from
//! the answer.

use super::hid::{self, ASUS_VENDOR, HidRaw, Open};
use std::io;
use tspmo_sys::SysRoot;

pub const REPORT: u8 = 0x5d;
/// USB ids of the Aura keyboard controllers of 2021+ laptops.
pub const KEYBOARD_PRODUCTS: &[u16] = &[0x19b6, 0x1a30, 0x1ce6, 0x18c6];
pub const LEDS: &str = "/sys/class/leds";
/// The feature report the capability answer comes in: id and 62 bytes.
const FEATURE_LEN: usize = 63;

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
        Self::ALL.into_iter().find(|m| {
            m.name() == s
                || matches!(
                    (m, s.as_str()),
                    (Self::Star, "star")
                        | (Self::RainbowCycle, "color-cycle" | "colour-cycle")
                        | (Self::Pulse, "strobe")
                )
        })
    }

    /// Whether the colour matters: the rainbow effects bring their own.
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
    pub const ALL: [Speed; 3] = [Speed::Low, Speed::Med, Speed::High];

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
    /// What the keyboard shows out of the factory: steady white.
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

// --- what the controller is ----------------------------------------------------------------

/// How the keyboard's backlight is built, from the controller's own answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One colour only (white or red LEDs): brightness and a few effects, no colour choice.
    White,
    /// One zone, any colour.
    SingleZone,
    MultiZone,
    PerKey,
    /// The controller did not say; only what every Aura keyboard does is offered.
    Unknown,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::White => "single-zone-white",
            Self::SingleZone => "single-zone-rgb",
            Self::MultiZone => "multi-zone-rgb",
            Self::PerKey => "per-key-rgb",
            Self::Unknown => "unknown",
        }
    }

    /// The built-in effects of this kind of backlight (G-Helper's lists, which asusctl's
    /// per-model data agrees with where both cover a model).
    pub fn modes(self) -> Vec<Mode> {
        use Mode::*;
        match self {
            // Every Aura controller has these.
            Self::White | Self::Unknown => vec![Static, Breathe, Pulse],
            // A single zone cannot show a wave.
            Self::SingleZone => vec![Static, Breathe, RainbowCycle, Pulse],
            Self::MultiZone => vec![Static, Breathe, RainbowCycle, RainbowWave, Pulse],
            Self::PerKey => Mode::ALL.to_vec(),
        }
    }

    pub fn has_colour(self) -> bool {
        !matches!(self, Self::White)
    }
}

/// The capability record of the Aura controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuraInfo {
    /// 2 multi-zone, 3 per-key, 4 single zone.
    pub backlight: u8,
    /// The model year, BCD (`0x23`).
    pub year: u8,
    pub layout: u8,
    pub feat1: u8,
    pub feat2: u8,
    /// 1 Strix, 2 Flow, 4 Zephyrus, 8 TUF; 0 before 2023.
    pub family: u8,
}

impl AuraInfo {
    pub fn kind(&self) -> Kind {
        const ONE_ZONE_RED_EFFECT: u8 = 0x10;
        match self.backlight {
            2 => Kind::MultiZone,
            3 => Kind::PerKey,
            4 if self.feat2 & ONE_ZONE_RED_EFFECT != 0 => Kind::White,
            4 | 0 => Kind::SingleZone,
            _ => Kind::Unknown,
        }
    }

    pub fn family_name(&self) -> &'static str {
        match self.family {
            1 => "Strix",
            2 => "Flow",
            4 => "Zephyrus",
            8 => "TUF",
            _ => "other",
        }
    }
}

/// `5d 41 53 55 53 20 54 65 63 68 2e 49 6e 63 2e 00`: "ASUS Tech.Inc.", which wakes the
/// controller's command interface (the kernel driver sends it once at probe).
pub const INIT: [u8; 16] = [
    REPORT, b'A', b'S', b'U', b'S', b' ', b'T', b'e', b'c', b'h', b'.', b'I', b'n', b'c', b'.', 0,
];
/// The capability question: the answer is the feature report, starting with these six bytes.
pub const QUERY: [u8; 6] = [REPORT, 0x05, 0x20, 0x31, 0x00, 0x20];

/// Decode the answer to [`QUERY`].
pub fn parse_info(report: &[u8]) -> Option<AuraInfo> {
    if report.len() < 18 || report[..4] != QUERY[..4] {
        return None;
    }
    Some(AuraInfo {
        backlight: report[9],
        year: report[10],
        layout: report[12],
        feat1: report[13],
        feat2: report[14],
        family: if report[10] >= 0x23 { report[17] } else { 0 },
    })
}

/// Ask the controller what it is. `None` if it does not answer (or this is a test fixture).
pub fn probe(root: &SysRoot, hid: &HidRaw) -> Option<AuraInfo> {
    let mut d = Open::new(root, hid).ok()?;
    d.write(&hid.padded(REPORT, &INIT)).ok()?;
    d.write(&hid.padded(REPORT, &QUERY)).ok()?;
    parse_info(&d.get_feature(REPORT, FEATURE_LEN).ok()?)
}

// --- packets -------------------------------------------------------------------------------

/// `5d b3 zone mode r g b speed direction flag r2 g2 b2`, the 17-byte effect message. One-colour
/// keyboards take the brightness of the first colour from red alone; the flag byte is 1 for
/// breathing and `ff` for "pick colours yourself" (black).
pub fn effect_packet(e: &Effect, kind: Kind) -> [u8; 17] {
    let colour = if e.mode.uses_colour() {
        e.colour
    } else {
        [255, 255, 255]
    };
    let (c, c2) = if kind.has_colour() {
        (colour, e.colour2)
    } else {
        ([colour[0], 0, 0], [e.colour2[0], 0, 0])
    };
    let mut p = [0u8; 17];
    p[0] = REPORT;
    p[1] = 0xb3;
    // p[2] is the zone: 0 for keyboards with a single zone.
    p[3] = e.mode.id();
    p[4..7].copy_from_slice(&c);
    p[7] = e.speed.byte();
    p[8] = e.direction.byte();
    p[9] = if c == [0, 0, 0] {
        0xff
    } else {
        u8::from(e.mode == Mode::Breathe)
    };
    p[10..13].copy_from_slice(&c2);
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
    /// The keyboard zone's bits: boot 1, awake 3, sleep 5, shutdown 7 (the even ones belong to
    /// the logo, which this keyboard does not have).
    pub fn mask(self) -> u8 {
        u8::from(self.boot) << 1
            | u8::from(self.awake) << 3
            | u8::from(self.sleep) << 5
            | u8::from(self.shutdown) << 7
    }
}

/// `5d bd 01 keyboard lightbar lid rear ff`: the zones this keyboard lacks stay zero.
pub fn power_packet(p: PowerStates) -> [u8; 8] {
    [REPORT, 0xbd, 0x01, p.mask(), 0, 0, 0, 0xff]
}

// --- the LED class -------------------------------------------------------------------------

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

    /// Always writes, even when the level reads the same: what the LED class reports is the
    /// kernel's idea, and the keyboard may have been told otherwise (or nothing, if it was
    /// asleep). Writing again makes the keyboard agree.
    pub fn set(&self, root: &SysRoot, level: u8) -> io::Result<()> {
        if level > self.max {
            return Err(io::Error::other(format!("brightness must be 0-{}", self.max)));
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
    /// Its answer to the capability query, if it gave one.
    pub info: Option<AuraInfo>,
    pub kind: Kind,
    pub modes: Vec<Mode>,
}

pub fn detect(root: &SysRoot) -> Caps {
    let aura = KEYBOARD_PRODUCTS
        .iter()
        .find_map(|p| hid::find(root, ASUS_VENDOR, *p, REPORT));
    let info = aura.as_ref().and_then(|h| probe(root, h));
    let kind = info.map_or(Kind::Unknown, |i| i.kind());
    let modes = if aura.is_some() { kind.modes() } else { Vec::new() };
    Caps {
        backlight: backlight(root),
        aura,
        info,
        kind,
        modes,
    }
}

// --- writing -------------------------------------------------------------------------------

/// Show an effect: effect, set, and (`keep`) apply, which also keeps it across power cycles.
/// One open of the device.
pub fn write_effect(root: &SysRoot, hid: &HidRaw, kind: Kind, e: &Effect, keep: bool) -> io::Result<()> {
    let mut packets = vec![effect_packet(e, kind).to_vec(), SET.to_vec()];
    if keep {
        packets.push(APPLY.to_vec());
    }
    let packets: Vec<_> = packets.into_iter().map(|p| hid.padded(REPORT, &p)).collect();
    hid::write(root, hid, &packets)
}

pub fn write_power_states(root: &SysRoot, hid: &HidRaw, p: PowerStates) -> io::Result<()> {
    hid::write(root, hid, &[hid.padded(REPORT, &power_packet(p))])
}

/// Bring the controller to a known state: wake it, set when each power state lights the
/// keyboard, show the effect. One open of the device.
pub fn write_all(root: &SysRoot, hid: &HidRaw, kind: Kind, p: PowerStates, e: &Effect) -> io::Result<()> {
    let packets: Vec<_> = [
        INIT.to_vec(),
        power_packet(p).to_vec(),
        effect_packet(e, kind).to_vec(),
        SET.to_vec(),
    ]
    .into_iter()
    .map(|p| hid.padded(REPORT, &p))
    .collect();
    hid::write(root, hid, &packets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asus::testfix::Fix;

    /// The answer the GU605MI's controller gives (captured with the query above).
    fn gu605_answer() -> Vec<u8> {
        let mut r = vec![0u8; FEATURE_LEN];
        r[..18].copy_from_slice(&[
            0x5d, 0x05, 0x20, 0x31, 0x00, 0x20, 0x01, 0, 0, 0x04, 0x23, 0x04, 0x01, 0x00, 0x46, 0x03, 0x0f,
            0x04,
        ]);
        r
    }

    #[test]
    fn capability_answer_of_the_gu605mi() {
        let i = parse_info(&gu605_answer()).unwrap();
        assert_eq!(
            (i.backlight, i.year, i.layout, i.feat1, i.feat2, i.family),
            (4, 0x23, 1, 0, 0x46, 4)
        );
        assert_eq!(i.kind(), Kind::SingleZone);
        assert_eq!(i.family_name(), "Zephyrus");
        assert_eq!(
            Kind::SingleZone.modes(),
            vec![Mode::Static, Mode::Breathe, Mode::RainbowCycle, Mode::Pulse]
        );
    }

    #[test]
    fn answers_of_other_keyboards_and_junk() {
        let mut r = gu605_answer();
        r[14] = 0x56; // "one zone red effect"
        assert_eq!(parse_info(&r).unwrap().kind(), Kind::White);
        assert_eq!(
            Kind::White.modes(),
            vec![Mode::Static, Mode::Breathe, Mode::Pulse]
        );
        r[9] = 3;
        assert_eq!(parse_info(&r).unwrap().kind(), Kind::PerKey);
        assert_eq!(Kind::PerKey.modes().len(), 12);
        r[9] = 2;
        assert_eq!(parse_info(&r).unwrap().kind(), Kind::MultiZone);
        r[9] = 9;
        assert_eq!(parse_info(&r).unwrap().kind(), Kind::Unknown);
        // Not an answer to our question.
        r[1] = 0x99;
        assert_eq!(parse_info(&r), None);
        assert_eq!(parse_info(&[0x5d, 5, 0x20]), None);
    }

    #[test]
    fn static_effect_packet() {
        let e = Effect {
            mode: Mode::Static,
            colour: [0xff, 0x11, 0xdd],
            ..Effect::default()
        };
        assert_eq!(
            effect_packet(&e, Kind::SingleZone),
            [
                0x5d, 0xb3, 0, 0, 0xff, 0x11, 0xdd, 0xeb, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        // A one-colour keyboard only has red.
        assert_eq!(&effect_packet(&e, Kind::White)[4..7], &[0xff, 0, 0]);
    }

    #[test]
    fn breathe_and_black_flags() {
        let e = Effect {
            mode: Mode::Breathe,
            colour: [1, 2, 3],
            colour2: [4, 5, 6],
            speed: Speed::High,
            ..Effect::default()
        };
        let p = effect_packet(&e, Kind::SingleZone);
        assert_eq!(&p[..10], &[0x5d, 0xb3, 0, 1, 1, 2, 3, 0xf5, 0, 1]);
        assert_eq!(&p[10..13], &[4, 5, 6]);
        let black = Effect {
            colour: [0, 0, 0],
            ..e
        };
        assert_eq!(effect_packet(&black, Kind::SingleZone)[9], 0xff);
    }

    #[test]
    fn rainbow_ignores_the_colour() {
        let e = Effect {
            mode: Mode::RainbowCycle,
            colour: [0, 0, 0],
            ..Effect::default()
        };
        let p = effect_packet(&e, Kind::SingleZone);
        assert_eq!(&p[3..7], &[2, 255, 255, 255]);
        assert_eq!(p[9], 0);
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
        assert_eq!(
            power_packet(only_awake),
            [0x5d, 0xbd, 0x01, 0b0000_1000, 0, 0, 0, 0xff]
        );
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
        assert_eq!(Mode::parse("Strobe"), Some(Mode::Pulse));
        assert_eq!(Mode::parse("color-cycle"), Some(Mode::RainbowCycle));
        assert_eq!(Mode::parse(""), None);
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
        // The fixture cannot answer the query: only what every Aura keyboard does is offered.
        assert_eq!(c.info, None);
        assert_eq!(c.kind, Kind::Unknown);
        assert_eq!(c.modes, vec![Mode::Static, Mode::Breathe, Mode::Pulse]);
    }

    #[test]
    fn brightness_is_bounded_and_always_written() {
        let fx = Fix::new("light-bright");
        let root = fx.root();
        let bl = backlight(&root).unwrap();
        bl.set(&root, 2).unwrap();
        assert_eq!(bl.get(&root), Some(2));
        assert!(bl.set(&root, 4).is_err());
        // Same level again: still written (a file's mtime would tell; the content is equal).
        bl.set(&root, 2).unwrap();
        assert_eq!(bl.get(&root), Some(2));
    }

    #[test]
    fn effect_is_written_in_three_packets() {
        let fx = Fix::new("light-write");
        let root = fx.root();
        let hid = detect(&root).aura.unwrap();
        std::fs::create_dir_all(root.path("/dev")).unwrap();
        std::fs::write(root.path("/dev/hidraw2"), b"").unwrap();
        write_effect(&root, &hid, Kind::SingleZone, &Effect::default(), true).unwrap();
        // A regular file stands in for the device: the three 63-byte reports land back to back.
        let bytes = std::fs::read(root.path("/dev/hidraw2")).unwrap();
        assert_eq!(bytes.len(), 3 * 63);
        assert_eq!(&bytes[..2], &[0x5d, 0xb3]);
        assert_eq!(&bytes[63..65], &[0x5d, 0xb5]);
        assert_eq!(&bytes[126..128], &[0x5d, 0xb4]);
    }

    #[test]
    fn full_state_is_init_power_effect_set() {
        let fx = Fix::new("light-all");
        let root = fx.root();
        let hid = detect(&root).aura.unwrap();
        std::fs::create_dir_all(root.path("/dev")).unwrap();
        std::fs::write(root.path("/dev/hidraw2"), b"").unwrap();
        write_all(
            &root,
            &hid,
            Kind::SingleZone,
            PowerStates::default(),
            &Effect::default(),
        )
        .unwrap();
        let b = std::fs::read(root.path("/dev/hidraw2")).unwrap();
        assert_eq!(b.len(), 4 * 63);
        assert_eq!(&b[..5], &[0x5d, b'A', b'S', b'U', b'S']);
        assert_eq!(&b[63..67], &[0x5d, 0xbd, 1, 0xaa]);
        assert_eq!(&b[126..128], &[0x5d, 0xb3]);
        assert_eq!(&b[189..191], &[0x5d, 0xb5]);
    }
}
