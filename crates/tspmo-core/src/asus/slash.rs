//! The Slash light bar on the lid of recent Zephyrus / Flow models, over hidraw.
//!
//! Which USB device carries it depends on the model generation: 2024 models have their own
//! ITE device (`0b05:193b`, report `0x5e`), later ones share the keyboard's (`0b05:19b6`,
//! report `0x5d`). The packet layouts were studied in asusctl's `rog-slash` and rewritten
//! here as small builders.

use super::hid::{self, ASUS_VENDOR, HidRaw};
use std::io;
use tspmo_sys::{SysRoot, dmi::Dmi};

/// `(board name prefix, USB product, report id)`. The longest matching prefix wins.
const MODELS: &[(&str, u16, u8)] = &[
    ("GU605", 0x193b, 0x5e),
    ("GA403", 0x193b, 0x5e),
    ("GU605C", 0x19b6, 0x5d),
    ("GA403W", 0x19b6, 0x5d),
    ("GA403UH", 0x19b6, 0x5d),
    ("GA403UM", 0x19b6, 0x5d),
    ("GA403UP", 0x19b6, 0x5d),
    ("GA403GM", 0x19b6, 0x5d),
    ("GA605", 0x19b6, 0x5d),
    ("GU405", 0x19b6, 0x5d),
    ("GU606", 0x19b6, 0x5d),
    ("G614F", 0x19b6, 0x5d),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Static,
    Bounce,
    Slash,
    Loading,
    BitStream,
    Transmission,
    Flow,
    Flux,
    Phantom,
    Spectrum,
    Hazard,
    Interfacing,
    Ramp,
    GameOver,
    Start,
    Buzzer,
}

impl Mode {
    pub const ALL: [Mode; 16] = [
        Mode::Static,
        Mode::Bounce,
        Mode::Slash,
        Mode::Loading,
        Mode::BitStream,
        Mode::Transmission,
        Mode::Flow,
        Mode::Flux,
        Mode::Phantom,
        Mode::Spectrum,
        Mode::Hazard,
        Mode::Interfacing,
        Mode::Ramp,
        Mode::GameOver,
        Mode::Start,
        Mode::Buzzer,
    ];

    pub fn id(self) -> u8 {
        match self {
            Self::Static => 0x06,
            Self::Bounce => 0x10,
            Self::Slash => 0x12,
            Self::Loading => 0x13,
            Self::BitStream => 0x1d,
            Self::Transmission => 0x1a,
            Self::Flow => 0x19,
            Self::Flux => 0x25,
            Self::Phantom => 0x24,
            Self::Spectrum => 0x26,
            Self::Hazard => 0x32,
            Self::Interfacing => 0x33,
            Self::Ramp => 0x34,
            Self::GameOver => 0x42,
            Self::Start => 0x43,
            Self::Buzzer => 0x44,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Bounce => "bounce",
            Self::Slash => "slash",
            Self::Loading => "loading",
            Self::BitStream => "bitstream",
            Self::Transmission => "transmission",
            Self::Flow => "flow",
            Self::Flux => "flux",
            Self::Phantom => "phantom",
            Self::Spectrum => "spectrum",
            Self::Hazard => "hazard",
            Self::Interfacing => "interfacing",
            Self::Ramp => "ramp",
            Self::GameOver => "game-over",
            Self::Start => "start",
            Self::Buzzer => "buzzer",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Self::ALL
            .into_iter()
            .find(|m| m.name() == s || m.name().replace('-', "") == s.replace('-', ""))
    }

    pub fn from_id(id: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// Feature reports are 127 bytes (and the report id).
pub const REPORT_LEN: usize = 128;
/// The pause between animations, in seconds (0: none).
pub const MAX_INTERVAL: u8 = 5;
/// The brightness steps of the firmware's own apps; any value 0-255 is accepted.
pub const BRIGHTNESS_LEVELS: [(&str, u8); 4] = [("off", 0), ("low", 85), ("medium", 170), ("high", 255)];

/// `report id` and bytes of each packet.
pub fn init(id: u8) -> [Vec<u8>; 2] {
    [vec![id, 0xd7, 0, 0, 1, 0xac], vec![id, 0xd2, 2, 1, 8, 0xab]]
}

pub fn enable(id: u8, on: bool) -> Vec<u8> {
    vec![id, 0xd8, 2, 0, 1, if on { 0x00 } else { 0x80 }]
}

pub fn save(id: u8) -> Vec<u8> {
    vec![id, 0xd4, 0, 0, 1, 0xab]
}

/// Display options: on/off, brightness (0-255), pause between animations (0-255).
pub fn options(id: u8, enabled: bool, brightness: u8, interval: u8) -> Vec<u8> {
    vec![
        id,
        0xd3,
        3,
        1,
        8,
        0xab,
        0xff,
        1,
        u8::from(enabled),
        6,
        brightness,
        0xff,
        interval,
    ]
}

/// The animation; follow it with [`save`].
pub fn mode(id: u8, m: Mode) -> Vec<u8> {
    vec![
        id,
        0xd3,
        4,
        0,
        0x0c,
        1,
        m.id(),
        2,
        0x19,
        3,
        0x13,
        4,
        0x11,
        5,
        0x12,
        6,
        0x13,
    ]
}

/// When the firmware itself shows the Slash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Boot,
    Sleep,
    Shutdown,
    LowBattery,
}

impl When {
    pub const ALL: [When; 4] = [When::Boot, When::Sleep, When::Shutdown, When::LowBattery];

    /// The firmware record that holds the flag.
    pub fn region(self) -> u8 {
        match self {
            Self::Boot => 0xa0,
            Self::Sleep => 0xa1,
            Self::LowBattery => 0xa2,
            Self::Shutdown => 0xa4,
        }
    }
}

/// The record of a flag as a packet to write it. Used when the firmware's own record could not
/// be read (otherwise [`flag_from_record`] keeps whatever else the record holds). A set flag is
/// 1 for every record, the sleep one included (G-Helper reads it back that way; asusctl
/// inverts it).
pub fn show_when(id: u8, w: When, on: bool) -> Vec<u8> {
    match w {
        When::Boot => vec![id, 0xd3, 3, 1, 8, 0xa0, 4, 0xff, u8::from(on), 1, 0xff, 0],
        When::Sleep => vec![id, 0xd3, 3, 1, 8, 0xa1, 0, 0xff, u8::from(on), 2, 0xff, 0xff],
        When::LowBattery => vec![id, 0xd3, 3, 1, 8, 0xa2, 1, 0xff, u8::from(on), 2, 0xff, 0xff],
        When::Shutdown => vec![id, 0xd3, 3, 1, 8, 0xa4, 5, 0xff, u8::from(on), 1, 0xff, 0],
    }
}

// --- reading the firmware's records ---------------------------------------------------------

/// Ask for the record of `region` (`a0`..): answered as a feature report.
pub fn read_region(id: u8, region: u8) -> Vec<u8> {
    vec![id, 0xd2, 2, 1, 8, region]
}

/// Ask for the animation record.
pub fn read_mode(id: u8) -> Vec<u8> {
    vec![id, 0xd2, 3, 0, 0x0c]
}

/// A record that answers `region`, and is not the all-zero answer to a region that is not there.
fn region_record(id: u8, rec: &[u8], region: u8) -> Option<&[u8]> {
    let ok = rec.len() >= 13
        && rec[..3] == [id, 0xd2, 2]
        && rec[5] == region
        && rec[6..12].iter().any(|b| *b != 0);
    ok.then_some(rec)
}

/// `(enabled, brightness, interval)` from the display-options record (`ab`).
pub fn parse_options(id: u8, rec: &[u8]) -> Option<(bool, u8, u8)> {
    let r = region_record(id, rec, 0xab)?;
    Some((r[8] == 1, r[10], r[12].min(MAX_INTERVAL)))
}

/// Whether the flag of `w` is set.
pub fn parse_flag(id: u8, w: When, rec: &[u8]) -> Option<bool> {
    Some(region_record(id, rec, w.region())?[8] == 1)
}

/// The animation from its record.
pub fn parse_mode(id: u8, rec: &[u8]) -> Option<Mode> {
    (rec.len() >= 7 && rec[..6] == [id, 0xd2, 3, 0, 0x0c, 1])
        .then(|| Mode::from_id(rec[6]))
        .flatten()
}

/// The record with a flag changed, as a packet to write back.
pub fn flag_from_record(id: u8, w: When, rec: &[u8], on: bool) -> Option<Vec<u8>> {
    let mut v = region_record(id, rec, w.region())?.to_vec();
    v[1] = 0xd3;
    v[2] = 3;
    v[8] = u8::from(on);
    Some(v)
}

/// The animation record with another animation, as a packet to write back: the other slots
/// (what the firmware plays at boot, sleep, ...) stay as this model has them.
pub fn mode_from_record(id: u8, rec: &[u8], m: Mode) -> Option<Vec<u8>> {
    parse_mode(id, rec)?;
    let mut v = rec.to_vec();
    v[1] = 0xd3;
    v[2] = 4;
    v[6] = m.id();
    Some(v)
}

/// Keep the Slash lit while running on battery.
pub fn show_on_battery(id: u8, on: bool) -> Vec<u8> {
    vec![id, 0xd8, 1, 0, 1, if on { 0x00 } else { 0x80 }]
}

/// Keep the Slash lit with the lid closed.
pub fn show_on_lid_closed(id: u8, on: bool) -> Vec<u8> {
    vec![id, 0xd8, 0, 0, 2, 0xa5, if on { 0x00 } else { 0x80 }]
}

#[derive(Debug, Clone)]
pub struct Slash {
    pub hid: HidRaw,
    /// Report id every packet starts with.
    pub report: u8,
}

/// The Slash of this machine, if the model has one and its USB device is there.
pub fn detect(root: &SysRoot) -> Option<Slash> {
    let board = Dmi::read(root).board_name.to_ascii_uppercase();
    let (_, product, report) = MODELS
        .iter()
        .filter(|(p, _, _)| board.starts_with(p))
        .max_by_key(|(p, _, _)| p.len())?;
    hid::find(root, ASUS_VENDOR, *product, *report).map(|hid| Slash { hid, report: *report })
}

impl Slash {
    /// Open the device once and send the packets in order.
    pub fn send(&self, root: &SysRoot, packets: &[Vec<u8>]) -> io::Result<()> {
        hid::write(root, &self.hid, packets)
    }

    /// Open the device for an exchange: questions and answers as well as writes.
    pub fn open(&self, root: &SysRoot) -> io::Result<Session<'_>> {
        Ok(Session {
            slash: self,
            dev: hid::Open::new(root, &self.hid)?,
        })
    }
}

/// The Slash, open. Questions that get no answer (a fixture, a device that went away) are
/// `None`, and callers fall back to writing the whole record.
pub struct Session<'a> {
    slash: &'a Slash,
    dev: hid::Open,
}

impl Session<'_> {
    pub fn write(&mut self, packets: &[Vec<u8>]) -> io::Result<()> {
        packets.iter().try_for_each(|p| self.dev.write(p))
    }

    fn ask(&mut self, request: Vec<u8>) -> Option<Vec<u8>> {
        self.dev.set_feature(&request, REPORT_LEN).ok()?;
        self.dev.get_feature(self.slash.report, REPORT_LEN).ok()
    }

    pub fn region(&mut self, region: u8) -> Option<Vec<u8>> {
        self.ask(read_region(self.slash.report, region))
    }

    pub fn mode_record(&mut self) -> Option<Vec<u8>> {
        self.ask(read_mode(self.slash.report))
    }

    /// Everything the firmware says about the display now.
    pub fn read(&mut self) -> Option<Live> {
        let id = self.slash.report;
        let (enabled, brightness, interval) = parse_options(id, &self.region(0xab)?)?;
        let mode = self.mode_record().and_then(|r| parse_mode(id, &r));
        let flag = |s: &mut Self, w: When| s.region(w.region()).and_then(|r| parse_flag(id, w, &r));
        Some(Live {
            enabled,
            brightness,
            interval,
            mode,
            boot: flag(self, When::Boot),
            sleep: flag(self, When::Sleep),
            shutdown: flag(self, When::Shutdown),
            low_battery: flag(self, When::LowBattery),
        })
    }

    /// Set one of the "show when" flags, keeping the rest of its record.
    pub fn set_flag(&mut self, w: When, on: bool) -> io::Result<()> {
        let id = self.slash.report;
        let p = self
            .region(w.region())
            .and_then(|r| flag_from_record(id, w, &r, on))
            .unwrap_or_else(|| show_when(id, w, on));
        self.write(&[p])
    }

    /// Choose the animation, keeping the other slots of its record.
    pub fn set_mode(&mut self, m: Mode) -> io::Result<()> {
        let id = self.slash.report;
        let p = self
            .mode_record()
            .and_then(|r| mode_from_record(id, &r, m))
            .unwrap_or_else(|| mode(id, m));
        self.write(&[p])
    }
}

/// What the firmware reports; `None` for what it did not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Live {
    pub enabled: bool,
    pub brightness: u8,
    pub interval: u8,
    pub mode: Option<Mode>,
    pub boot: Option<bool>,
    pub sleep: Option<bool>,
    pub shutdown: Option<bool>,
    pub low_battery: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asus::testfix::Fix;

    #[test]
    fn packets_match_the_documented_layouts() {
        assert_eq!(init(0x5e)[0], vec![0x5e, 0xd7, 0, 0, 1, 0xac]);
        assert_eq!(init(0x5e)[1], vec![0x5e, 0xd2, 2, 1, 8, 0xab]);
        assert_eq!(enable(0x5e, true), vec![0x5e, 0xd8, 2, 0, 1, 0x00]);
        assert_eq!(enable(0x5e, false)[5], 0x80);
        assert_eq!(save(0x5e), vec![0x5e, 0xd4, 0, 0, 1, 0xab]);
        let o = options(0x5e, true, 200, 7);
        assert_eq!(o.len(), 13);
        assert_eq!((o[8], o[10], o[12]), (1, 200, 7));
        let m = mode(0x5e, Mode::Flow);
        assert_eq!(m.len(), 17);
        assert_eq!(&m[..8], &[0x5e, 0xd3, 4, 0, 0x0c, 1, 0x19, 2]);
    }

    #[test]
    fn show_when_flags() {
        assert_eq!(show_when(0x5e, When::Boot, true)[8], 1);
        assert_eq!(show_when(0x5e, When::Boot, false)[8], 0);
        assert_eq!(show_when(0x5e, When::Sleep, true)[8], 1);
        assert_eq!(show_when(0x5e, When::Sleep, false)[8], 0);
        assert_eq!(show_when(0x5e, When::LowBattery, true)[5], 0xa2);
        assert_eq!(show_when(0x5e, When::Shutdown, true)[5], 0xa4);
        assert_eq!(show_on_battery(0x5e, false), vec![0x5e, 0xd8, 1, 0, 1, 0x80]);
        assert_eq!(show_on_lid_closed(0x5e, true), vec![0x5e, 0xd8, 0, 0, 2, 0xa5, 0]);
    }

    /// Records as the GU605MI's firmware answered them.
    fn record(bytes: &[u8]) -> Vec<u8> {
        let mut v = bytes.to_vec();
        v.resize(REPORT_LEN, 0);
        v
    }

    #[test]
    fn records_read_back() {
        let opt = record(&[0x5e, 0xd2, 2, 1, 8, 0xab, 0xff, 1, 1, 6, 0xaa, 0xff, 3, 0, 0]);
        assert_eq!(parse_options(0x5e, &opt), Some((true, 0xaa, 3)));
        let off = record(&[0x5e, 0xd2, 2, 1, 8, 0xab, 0xff, 1, 0, 6, 0xff, 0xff, 0xff]);
        // An interval beyond what the firmware's apps offer reads as the longest one.
        assert_eq!(parse_options(0x5e, &off), Some((false, 0xff, MAX_INTERVAL)));
        // A region that is not there answers with zeros; a different region is not this one.
        assert_eq!(parse_options(0x5e, &record(&[0x5e, 0xd2, 2, 1, 8, 0xab])), None);
        assert_eq!(
            parse_options(0x5e, &record(&[0x5e, 0xd2, 2, 1, 8, 0xac, 1, 1, 1, 1, 1, 1, 1])),
            None
        );

        let boot = record(&[0x5e, 0xd2, 2, 1, 8, 0xa0, 4, 0xff, 1, 1, 0xff, 0, 0, 0x11]);
        assert_eq!(parse_flag(0x5e, When::Boot, &boot), Some(true));
        assert_eq!(parse_flag(0x5e, When::Sleep, &boot), None);
        let off = flag_from_record(0x5e, When::Boot, &boot, false).unwrap();
        assert_eq!(&off[..9], &[0x5e, 0xd3, 3, 1, 8, 0xa0, 4, 0xff, 0]);
        // The rest of the record, the byte we know nothing about included, is kept.
        assert_eq!(off[13], 0x11);
        assert_eq!(off.len(), REPORT_LEN);

        let m = record(&[
            0x5e, 0xd2, 3, 0, 0x0c, 1, 0x12, 2, 0x19, 3, 0x13, 4, 0x11, 5, 0x12, 6, 0x13,
        ]);
        assert_eq!(parse_mode(0x5e, &m), Some(Mode::Slash));
        let w = mode_from_record(0x5e, &m, Mode::Flux).unwrap();
        assert_eq!(&w[..9], &[0x5e, 0xd3, 4, 0, 0x0c, 1, 0x25, 2, 0x19]);
        assert_eq!(
            mode_from_record(0x5e, &record(&[0x5e, 0xd2, 3, 0, 0x0c, 0, 0x12]), Mode::Flux),
            None
        );
    }

    #[test]
    fn modes_are_unique_and_parse() {
        let ids: std::collections::HashSet<_> = Mode::ALL.iter().map(|m| m.id()).collect();
        assert_eq!(ids.len(), 16);
        assert_eq!(Mode::parse("Game_Over"), Some(Mode::GameOver));
        assert_eq!(Mode::parse("bit-stream"), Some(Mode::BitStream));
        assert_eq!(Mode::parse("BitStream"), Some(Mode::BitStream));
        assert_eq!(Mode::parse("nope"), None);
    }

    #[test]
    fn detects_the_gu605_slash() {
        let fx = Fix::new("slash-detect");
        let s = detect(&fx.root()).unwrap();
        assert_eq!((s.hid.node.as_str(), s.report), ("hidraw1", 0x5e));
    }

    #[test]
    fn no_slash_on_other_models() {
        let fx = Fix::new("slash-none");
        fx.set("/sys/class/dmi/id/board_name", "FX507ZC4\n");
        assert!(detect(&fx.root()).is_none());
        // A GU605C would use the keyboard's device instead.
        fx.set("/sys/class/dmi/id/board_name", "GU605CW\n");
        assert_eq!(detect(&fx.root()).map(|s| s.report), Some(0x5d));
    }
}
