//! The parts of an EDID tspmo needs: the panel's name, its colour primaries and whether it is
//! an OLED.

/// CIE 1931 xy chromaticity.
pub type Xy = (f64, f64);

#[derive(Debug, Clone, PartialEq)]
pub struct Primaries {
    pub red: Xy,
    pub green: Xy,
    pub blue: Xy,
    pub white: Xy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edid {
    /// Three-letter PNP vendor id, e.g. `SDC`.
    pub vendor: String,
    pub product: u16,
    /// The monitor-name descriptor, e.g. `ATNA60DL01-0`.
    pub name: String,
    pub primaries: Option<Primaries>,
    pub gamma: Option<f64>,
    /// Physical size in millimetres, when the EDID gives one.
    pub size_mm: Option<(u32, u32)>,
}

const HEADER: [u8; 8] = [0, 255, 255, 255, 255, 255, 255, 0];

pub fn parse(b: &[u8]) -> Option<Edid> {
    if b.len() < 128 || b[..8] != HEADER {
        return None;
    }
    if b[..128].iter().fold(0u8, |a, &x| a.wrapping_add(x)) != 0 {
        return None;
    }
    let id = u16::from_be_bytes([b[8], b[9]]);
    let letter = |v: u16| char::from(b'A' + (v & 0x1f) as u8 - 1);
    let vendor: String = [letter(id >> 10), letter(id >> 5), letter(id)]
        .into_iter()
        .collect();

    let mut name = String::new();
    for d in (0x36..=0x6c).step_by(18) {
        if b[d..d + 3] == [0, 0, 0] && b[d + 3] == 0xfc {
            name = b[d + 5..d + 18]
                .iter()
                .take_while(|&&c| c != 0x0a)
                .map(|&c| char::from(c))
                .collect::<String>()
                .trim()
                .to_owned();
        }
    }

    let hi_lo = |hi: u8, lo: u8| (f64::from(hi) * 4.0 + f64::from(lo & 3)) / 1024.0;
    let (l1, l2) = (b[0x19], b[0x1a]);
    let primaries = (b[0x1b..0x23].iter().any(|&v| v != 0)).then(|| Primaries {
        red: (hi_lo(b[0x1b], l1 >> 6), hi_lo(b[0x1c], l1 >> 4)),
        green: (hi_lo(b[0x1d], l1 >> 2), hi_lo(b[0x1e], l1)),
        blue: (hi_lo(b[0x1f], l2 >> 6), hi_lo(b[0x20], l2 >> 4)),
        white: (hi_lo(b[0x21], l2 >> 2), hi_lo(b[0x22], l2)),
    });
    let gamma = (b[0x17] != 0xff).then(|| (f64::from(b[0x17]) + 100.0) / 100.0);
    let size_mm = (b[0x15] != 0 && b[0x16] != 0).then(|| (u32::from(b[0x15]) * 10, u32::from(b[0x16]) * 10));

    Some(Edid {
        vendor,
        product: u16::from_le_bytes([b[10], b[11]]),
        name,
        primaries,
        gamma,
        size_mm,
    })
}

impl Edid {
    /// Samsung Display's OLED laptop panels are named `ATNA...`, `ATNB...` and so on; `AMS...`
    /// are its older AMOLED parts. Other makers follow no rule, so those rely on the DMI model
    /// containing "OLED" (see [`is_oled`]).
    pub fn looks_oled(&self) -> bool {
        self.name.starts_with("ATN") || self.name.starts_with("AMS")
    }
}

/// Is this panel an OLED: by its EDID name, or because the laptop model says so.
pub fn is_oled(edid: Option<&Edid>, dmi_model: &str) -> bool {
    edid.is_some_and(Edid::looks_oled) || dmi_model.to_ascii_uppercase().contains("OLED")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The Samsung ATNA60DL01 EDID of the GU605MI, trimmed to the base block.
    pub fn gu605_edid() -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[..8].copy_from_slice(&HEADER);
        b[8..10].copy_from_slice(&[0x4c, 0x83]);
        b[10..12].copy_from_slice(&[0xa3, 0x41]);
        b[0x15] = 34;
        b[0x16] = 22;
        b[0x17] = 0x78;
        b[0x19..0x23].copy_from_slice(&[0x0c, 0xf1, 0xae, 0x52, 0x3c, 0xb9, 0x23, 0x0c, 0x50, 0x54]);
        let d = 0x6c;
        b[d + 3] = 0xfc;
        b[d + 5..d + 17].copy_from_slice(b"ATNA60DL01-0");
        b[d + 17] = 0x0a;
        let sum = b[..127].iter().fold(0u8, |a, &x| a.wrapping_add(x));
        b[127] = 0u8.wrapping_sub(sum);
        b
    }

    #[test]
    fn parses_name_vendor_and_primaries() {
        let e = parse(&gu605_edid()).unwrap();
        assert_eq!(e.vendor, "SDC");
        assert_eq!(e.name, "ATNA60DL01-0");
        assert_eq!(e.size_mm, Some((340, 220)));
        assert_eq!(e.gamma, Some(2.2));
        let p = e.primaries.unwrap();
        // Red is near the DCI-P3 red (0.680, 0.320).
        assert!((p.red.0 - 0.68).abs() < 0.01, "{:?}", p.red);
        assert!((p.red.1 - 0.32).abs() < 0.01);
        assert!((p.white.0 - 0.3125).abs() < 0.01);
    }

    #[test]
    fn rejects_bad_edids() {
        assert!(parse(&[0; 128]).is_none());
        let mut b = gu605_edid();
        b[20] ^= 1; // breaks the checksum
        assert!(parse(&b).is_none());
        assert!(parse(&b[..64]).is_none());
    }

    #[test]
    fn oled_detection() {
        let e = parse(&gu605_edid()).unwrap();
        assert!(is_oled(Some(&e), "ROG Zephyrus G16 GU605MI"));
        let mut lcd = e.clone();
        lcd.name = "NE160QDM-NZ6".into();
        assert!(!is_oled(Some(&lcd), "ROG Zephyrus G16"));
        assert!(is_oled(Some(&lcd), "Zenbook OLED UX3405"));
        assert!(!is_oled(None, "TUF Gaming"));
    }
}
