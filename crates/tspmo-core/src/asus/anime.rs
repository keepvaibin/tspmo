//! The AniMe Matrix dot display on the lid of some Zephyrus models (GA401, GA402, GU604, ...),
//! over hidraw (`0b05:193b`, report `0x5e`, 640-byte packets).
//!
//! No such hardware on the development laptop: this is tested against packet layouts only. The
//! layouts were studied in asusctl's `rog-anime` and rewritten here; only the GA402 family's
//! LED geometry is implemented (the other models' tables are irregular and unverified), the
//! others accept raw LED buffers.

use super::hid::{self, ASUS_VENDOR, HidRaw};
use std::io;
use tspmo_sys::{SysRoot, dmi::Dmi};

pub const PRODUCT: u16 = 0x193b;
pub const PAGE: u8 = 0x5e;
pub const PACKET: usize = 640;
/// Packet header (7 bytes) plus data, with 6 bytes of padding the HID feature report needs.
const DATA_START: usize = 7;
const DATA_END: usize = 634;
const PANE: usize = DATA_END - DATA_START;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ga401,
    Ga402,
    Gu604,
}

impl Kind {
    pub fn from_board(board: &str) -> Option<Self> {
        let b = board.to_ascii_uppercase();
        if b.starts_with("GA401I") || b.starts_with("GA401Q") {
            Some(Self::Ga401)
        } else if ["GA402R", "GA402X", "GA402N"].iter().any(|p| b.starts_with(p)) {
            Some(Self::Ga402)
        } else if b.starts_with("GU604V") {
            Some(Self::Gu604)
        } else {
            None
        }
    }

    /// Width and height of the diagonal image.
    pub fn size(self) -> (usize, usize) {
        match self {
            Self::Ga401 => (74, 36),
            Self::Ga402 => (74, 39),
            Self::Gu604 => (70, 43),
        }
    }

    /// Bytes of LED data.
    pub fn data_len(self) -> usize {
        match self {
            Self::Ga401 => PANE * 2,
            Self::Ga402 | Self::Gu604 => PANE * 3,
        }
    }

    /// Where each pane's LEDs start (1-based) and how many: the 7-byte packet headers.
    fn headers(self) -> Vec<[u8; 7]> {
        let a = [PAGE, 0xc0, 0x02, 0x01, 0x00, 0x73, 0x02];
        let b = [PAGE, 0xc0, 0x02, 0x74, 0x02, 0x73, 0x02];
        let c = [PAGE, 0xc0, 0x02, 0xe7, 0x04, 0x73, 0x02];
        match self {
            Self::Ga401 => vec![a, b],
            Self::Ga402 | Self::Gu604 => vec![a, b, c],
        }
    }
}

fn packet(bytes: &[u8]) -> [u8; PACKET] {
    let mut p = [0u8; PACKET];
    p[..bytes.len()].copy_from_slice(bytes);
    p
}

/// The two packets that wake the controller after boot.
pub fn init() -> [[u8; PACKET]; 2] {
    let mut hello = vec![PAGE];
    hello.extend_from_slice(b"ASUS Tech.Inc.");
    [packet(&hello), packet(&[PAGE, 0xc2])]
}

/// Show what was written.
pub fn flush() -> [u8; PACKET] {
    packet(&[PAGE, 0xc0, 0x03])
}

/// Brightness 0 (off) to 3.
pub fn brightness(level: u8) -> [u8; PACKET] {
    packet(&[PAGE, 0xc0, 0x04, level.min(3)])
}

pub fn enable(on: bool) -> [u8; PACKET] {
    packet(&[PAGE, 0xc3, 0x01, if on { 0x00 } else { 0x80 }])
}

/// The animations the firmware plays by itself (boot, awake, sleep, shutdown).
pub fn builtin(on: bool) -> [u8; PACKET] {
    packet(&[PAGE, 0xc4, 0x01, if on { 0x00 } else { 0x80 }])
}

/// Which built-in animation plays in each state; each is 0 or 1.
pub fn builtin_choice(boot: u8, awake: u8, sleep: u8, shutdown: u8) -> [u8; PACKET] {
    let b = (awake & 1) | (sleep & 1) << 1 | (shutdown & 1) << 2 | (boot & 1) << 3;
    packet(&[PAGE, 0xc5, b])
}

/// LED data to packets: one per pane, each with its header.
pub fn frame(kind: Kind, leds: &[u8]) -> Result<Vec<[u8; PACKET]>, String> {
    if leds.len() != kind.data_len() {
        return Err(format!("frame must be {} bytes", kind.data_len()));
    }
    let mut out: Vec<[u8; PACKET]> = vec![[0; PACKET]; kind.headers().len()];
    for ((p, chunk), h) in out.iter_mut().zip(leds.chunks(PANE)).zip(kind.headers()) {
        p[..7].copy_from_slice(&h);
        p[DATA_START..DATA_START + chunk.len()].copy_from_slice(chunk);
    }
    Ok(out)
}

/// A grey-scale image of the display's size, 0-255 per dot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Grid {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![0; w * h],
        }
    }

    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }

    pub fn set(&mut self, x: usize, y: usize, v: u8) {
        if x < self.w && y < self.h {
            self.px[y * self.w + x] = v;
        }
    }

    /// Text in a 5x7 font at the vertical middle, starting where the display has dots (about 5
    /// characters fit); what doesn't fit is cut.
    pub fn text(w: usize, h: usize, s: &str) -> Self {
        let mut g = Self::new(w, h);
        let top = h.saturating_sub(7) / 2;
        // The dots lie on a parallelogram: at mid height the first 18 columns have none.
        let left = 18.min(w);
        let start = left;
        for (i, c) in s.chars().enumerate() {
            let x0 = start + i * 6;
            for (dy, row) in glyph(c).iter().enumerate() {
                for dx in 0..5 {
                    if row >> (4 - dx) & 1 == 1 {
                        g.set(x0 + dx, top + dy, 255);
                    }
                }
            }
        }
        g
    }

    /// A binary or ASCII PGM (`P5` / `P2`) image, scaled to `w` x `h` (nearest neighbour).
    pub fn from_pgm(data: &[u8], w: usize, h: usize) -> Result<Self, String> {
        let bad = || "not a PGM image (P2 or P5)".to_owned();
        let mut pos = 0;
        let mut token = |data: &[u8]| -> Option<String> {
            loop {
                while data.get(pos).is_some_and(u8::is_ascii_whitespace) {
                    pos += 1;
                }
                if data.get(pos) == Some(&b'#') {
                    while data.get(pos).is_some_and(|c| *c != b'\n') {
                        pos += 1;
                    }
                } else {
                    break;
                }
            }
            let start = pos;
            while data.get(pos).is_some_and(|c| !c.is_ascii_whitespace()) {
                pos += 1;
            }
            (pos > start).then(|| String::from_utf8_lossy(&data[start..pos]).into_owned())
        };
        let magic = token(data).ok_or_else(bad)?;
        let iw: usize = token(data).and_then(|t| t.parse().ok()).ok_or_else(bad)?;
        let ih: usize = token(data).and_then(|t| t.parse().ok()).ok_or_else(bad)?;
        let max: usize = token(data)
            .and_then(|t| t.parse().ok())
            .filter(|m| (1..=255).contains(m))
            .ok_or_else(bad)?;
        if iw == 0 || ih == 0 || iw > 4096 || ih > 4096 {
            return Err(bad());
        }
        let pixels: Vec<u8> = match magic.as_str() {
            "P5" => {
                pos += 1; // the single whitespace after the header
                data.get(pos..pos + iw * ih).ok_or_else(bad)?.to_vec()
            }
            "P2" => {
                let mut v = Vec::with_capacity(iw * ih);
                for _ in 0..iw * ih {
                    v.push(token(data).and_then(|t| t.parse::<u8>().ok()).ok_or_else(bad)?);
                }
                v
            }
            _ => return Err(bad()),
        };
        let mut g = Self::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = usize::from(pixels[y * ih / h * iw + x * iw / w]);
                g.px[y * w + x] = (v * 255 / max) as u8;
            }
        }
        Ok(g)
    }

    /// Dim every dot (0.0-1.0).
    pub fn scaled(mut self, brightness: f32) -> Self {
        for p in &mut self.px {
            *p = (f32::from(*p) * brightness.clamp(0.0, 1.0)) as u8;
        }
        self
    }

    /// The LED buffer of this display. Only the GA402 family's geometry is known.
    pub fn leds(&self, kind: Kind) -> Result<Vec<u8>, String> {
        let (w, h) = kind.size();
        if (self.w, self.h) != (w, h) {
            return Err(format!("image must be {w}x{h}"));
        }
        if kind != Kind::Ga402 {
            return Err("this model's dot layout isn't supported yet".into());
        }
        let mut out = vec![0u8; kind.data_len()];
        let mut at = 0;
        // The dots run along diagonals: twelve rows of 34 where the start steps in x and y
        // alternately, then rows from the bottom edge that shorten by one every second row.
        let starts = [
            (0, 5),
            (1, 5),
            (1, 4),
            (2, 4),
            (2, 3),
            (3, 3),
            (3, 2),
            (4, 2),
            (4, 1),
            (5, 1),
            (5, 0),
            (6, 0),
        ];
        let mut rows: Vec<(usize, usize, usize)> = starts.iter().map(|&(x, y)| (x, y, 34)).collect();
        rows.extend((7..=55).map(|x| (x, 0, 33 - (x - 7) / 2)));
        for (x, y, len) in rows {
            for i in 0..len {
                out[at] = self.get(x + i, h - y - i - 1);
                at += 1;
            }
        }
        Ok(out)
    }
}

/// 5x7 glyph rows (5 bits each, most significant bit left). Unknown characters are blank.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0e, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x0e, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0c],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
        '6' => [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        '.' => [0, 0, 0, 0, 0, 0x0c, 0x0c],
        ':' => [0, 0x0c, 0x0c, 0, 0x0c, 0x0c, 0],
        '-' => [0, 0, 0, 0x1f, 0, 0, 0],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0, 0x04],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        _ => [0; 7],
    }
}

#[derive(Debug, Clone)]
pub struct Anime {
    pub hid: HidRaw,
    pub kind: Kind,
}

/// The AniMe Matrix of this machine, if the model has one and the device is there.
pub fn detect(root: &SysRoot) -> Option<Anime> {
    let kind = Kind::from_board(&Dmi::read(root).board_name)?;
    hid::find(root, ASUS_VENDOR, PRODUCT, PAGE).map(|hid| Anime { hid, kind })
}

impl Anime {
    pub fn send(&self, root: &SysRoot, packets: &[Vec<u8>]) -> io::Result<()> {
        hid::write(root, &self.hid, packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asus::testfix::Fix;

    #[test]
    fn control_packets() {
        let [hello, second] = init();
        assert_eq!(&hello[..15], b"\x5eASUS Tech.Inc.");
        assert_eq!(&second[..2], &[0x5e, 0xc2]);
        assert_eq!(&flush()[..3], &[0x5e, 0xc0, 0x03]);
        assert_eq!(&brightness(2)[..4], &[0x5e, 0xc0, 0x04, 2]);
        assert_eq!(brightness(9)[3], 3);
        assert_eq!(enable(false)[3], 0x80);
        assert_eq!(builtin(true)[3], 0);
        // awake 1, sleep 0, shutdown 1, boot 1
        assert_eq!(builtin_choice(1, 1, 0, 1)[2], 0b1101);
        assert_eq!(enable(true).len(), PACKET);
    }

    #[test]
    fn frames_are_split_per_pane() {
        let leds: Vec<u8> = (0..Kind::Ga402.data_len()).map(|i| (i % 251) as u8).collect();
        let p = frame(Kind::Ga402, &leds).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(&p[0][..7], &[0x5e, 0xc0, 0x02, 0x01, 0x00, 0x73, 0x02]);
        assert_eq!(&p[2][..7], &[0x5e, 0xc0, 0x02, 0xe7, 0x04, 0x73, 0x02]);
        assert_eq!(p[1][7], leds[PANE]);
        assert_eq!(frame(Kind::Ga401, &leds[..PANE * 2]).unwrap().len(), 2);
        assert!(frame(Kind::Ga402, &leds[..10]).is_err());
    }

    #[test]
    fn models() {
        assert_eq!(Kind::from_board("GA402XV"), Some(Kind::Ga402));
        assert_eq!(Kind::from_board("GU604VI"), Some(Kind::Gu604));
        assert_eq!(Kind::from_board("GU605MI"), None);
    }

    #[test]
    fn geometry_fills_the_first_1449_leds() {
        let (w, h) = Kind::Ga402.size();
        let mut g = Grid::new(w, h);
        // The bottom-left dot is the first LED, and all dots are lit.
        g.px.iter_mut().for_each(|p| *p = 1);
        let leds = g.leds(Kind::Ga402).unwrap();
        assert_eq!(leds.len(), 1881);
        assert_eq!(leds.iter().filter(|v| **v == 1).count(), 1449);
        assert!(leds[1449..].iter().all(|v| *v == 0));
        let mut g = Grid::new(w, h);
        g.set(0, h - 5 - 1, 200);
        assert_eq!(g.leds(Kind::Ga402).unwrap()[0], 200);
        assert!(Grid::new(5, 5).leds(Kind::Ga402).is_err());
        assert!(Grid::new(70, 43).leds(Kind::Gu604).is_err());
    }

    #[test]
    fn text_lights_dots() {
        let g = Grid::text(74, 39, "HI 5");
        assert!(g.px.iter().filter(|p| **p == 255).count() > 20);
        // Text starts right of the dotless corner: 'H' at column 18.
        assert_eq!(g.get(18, 16), 255);
        assert_eq!(g.get(20, 16), 0);
        // Every lit dot of the text has an LED behind it.
        let leds = g.leds(Kind::Ga402).unwrap();
        assert_eq!(
            leds.iter().filter(|v| **v == 255).count(),
            g.px.iter().filter(|v| **v == 255).count()
        );
        assert_eq!(
            Grid::text(74, 39, "   ").px.iter().filter(|p| **p != 0).count(),
            0
        );
    }

    #[test]
    fn pgm_images() {
        let p5 = b"P5\n# c\n2 2\n255\n\x00\xff\xff\x00";
        let g = Grid::from_pgm(p5, 4, 4).unwrap();
        assert_eq!(g.get(0, 0), 0);
        assert_eq!(g.get(3, 0), 255);
        assert_eq!(g.get(0, 3), 255);
        let p2 = b"P2 2 1 15\n0 15\n";
        let g = Grid::from_pgm(p2, 2, 1).unwrap();
        assert_eq!((g.get(0, 0), g.get(1, 0)), (0, 255));
        assert!(Grid::from_pgm(b"P6 1 1 255 xxx", 2, 2).is_err());
        assert!(Grid::from_pgm(b"P5 2 2 255\n\x00", 2, 2).is_err());
        assert_eq!(Grid::new(2, 1).scaled(0.5).px, vec![0, 0]);
        assert_eq!(Grid::from_pgm(p2, 2, 1).unwrap().scaled(0.5).get(1, 0), 127);
    }

    #[test]
    fn not_on_this_laptop() {
        let fx = Fix::new("anime-none");
        assert!(detect(&fx.root()).is_none());
        // Same USB id as the Slash (the fixture's device), but a model with an AniMe uses it as one.
        fx.set("/sys/class/dmi/id/board_name", "GA402XV\n");
        assert_eq!(detect(&fx.root()).map(|a| a.kind), Some(Kind::Ga402));
    }
}
