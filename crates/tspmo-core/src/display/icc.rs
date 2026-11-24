//! A small ICC profile writer: matrix/TRC display profiles (version 2.4), which is all a
//! compositor needs to colour-manage a panel whose primaries are known.
//!
//! The colorants are the primaries adapted to the D50 profile connection space with the
//! Bradford transform, as the ICC specification requires.

use super::edid::{Primaries, Xy};

type Mat3 = [[f64; 3]; 3];
type Vec3 = [f64; 3];

/// Display P3 (DCI-P3 primaries, D65 white).
pub const DISPLAY_P3: Primaries = Primaries {
    red: (0.680, 0.320),
    green: (0.265, 0.690),
    blue: (0.150, 0.060),
    white: D65,
};

pub const SRGB: Primaries = Primaries {
    red: (0.640, 0.330),
    green: (0.300, 0.600),
    blue: (0.150, 0.060),
    white: D65,
};

const D65: Xy = (0.3127, 0.3290);
const D50_XYZ: Vec3 = [0.9642, 1.0, 0.8249];

/// Tone curve of the profile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trc {
    Srgb,
    Gamma(f64),
}

impl Trc {
    fn eval(self, x: f64) -> f64 {
        match self {
            Self::Srgb if x <= 0.04045 => x / 12.92,
            Self::Srgb => ((x + 0.055) / 1.055).powf(2.4),
            Self::Gamma(g) => x.powf(g),
        }
    }
}

fn xyz(xy: Xy) -> Vec3 {
    [xy.0 / xy.1, 1.0, (1.0 - xy.0 - xy.1) / xy.1]
}

fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

fn apply(m: &Mat3, v: Vec3) -> Vec3 {
    [0, 1, 2].map(|i| (0..3).map(|k| m[i][k] * v[k]).sum())
}

fn inverse(m: &Mat3) -> Mat3 {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let d = 1.0 / det;
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ]
}

/// Linear RGB → XYZ for these primaries, scaled so that RGB (1,1,1) is the white point.
pub fn rgb_to_xyz(p: &Primaries) -> Mat3 {
    let (r, g, b) = (xyz(p.red), xyz(p.green), xyz(p.blue));
    let cols = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = apply(&inverse(&cols), xyz(p.white));
    [0, 1, 2].map(|i| [cols[i][0] * s[0], cols[i][1] * s[1], cols[i][2] * s[2]])
}

/// Bradford chromatic adaptation from `from` white to `to` white (both XYZ).
fn bradford(from: Vec3, to: Vec3) -> Mat3 {
    const M: Mat3 = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let (s, d) = (apply(&M, from), apply(&M, to));
    let scale = [
        [d[0] / s[0], 0.0, 0.0],
        [0.0, d[1] / s[1], 0.0],
        [0.0, 0.0, d[2] / s[2]],
    ];
    mul(&inverse(&M), &mul(&scale, &M))
}

/// The D50-adapted colorant columns (red, green, blue) of a display with these primaries.
pub fn colorants(p: &Primaries) -> [Vec3; 3] {
    let m = mul(&bradford(xyz(p.white), D50_XYZ), &rgb_to_xyz(p));
    [0, 1, 2].map(|c| [m[0][c], m[1][c], m[2][c]])
}

fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz_tag(v: Vec3) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    for c in v {
        t.extend(s15(c));
    }
    t
}

fn desc_tag(text: &str) -> Vec<u8> {
    let mut t = b"desc\0\0\0\0".to_vec();
    t.extend((text.len() as u32 + 1).to_be_bytes());
    t.extend(text.as_bytes());
    t.push(0);
    t.extend([0u8; 4 + 4 + 2 + 1 + 67]); // no Unicode or Macintosh versions
    t
}

fn text_tag(text: &str) -> Vec<u8> {
    let mut t = b"text\0\0\0\0".to_vec();
    t.extend(text.as_bytes());
    t.push(0);
    t
}

fn curve_tag(trc: Trc) -> Vec<u8> {
    const N: usize = 1024;
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend((N as u32).to_be_bytes());
    for i in 0..N {
        let y = trc.eval(i as f64 / (N - 1) as f64);
        t.extend(((y * 65535.0).round() as u16).to_be_bytes());
    }
    t
}

/// A `vcgt` (video card gamma table) tag that scales all three channels by `scale`.
fn vcgt_tag(scale: f64) -> Vec<u8> {
    const N: usize = 256;
    let mut t = b"vcgt\0\0\0\0".to_vec();
    t.extend(0u32.to_be_bytes()); // table, not formula
    t.extend(3u16.to_be_bytes());
    t.extend((N as u16).to_be_bytes());
    t.extend(2u16.to_be_bytes());
    for _ in 0..3 {
        for i in 0..N {
            let v = i as f64 / (N - 1) as f64 * scale;
            t.extend(((v * 65535.0).round() as u16).to_be_bytes());
        }
    }
    t
}

fn chad_tag(m: &Mat3) -> Vec<u8> {
    let mut t = b"sf32\0\0\0\0".to_vec();
    for row in m {
        for v in row {
            t.extend(s15(*v));
        }
    }
    t
}

/// Build a version 2.4 matrix/TRC display profile named `description`.
pub fn build(description: &str, p: &Primaries, trc: Trc) -> Vec<u8> {
    build_dimmed(description, p, trc, 1.0)
}

/// Like [`build`], with the image darkened to `dim` (1.0 = untouched, 0.5 = half the light) by
/// a `vcgt` tag, which compositors apply to the whole output for free. Dimming the *encoded*
/// values by `dim^(1/2.2)` dims the light by `dim`.
pub fn build_dimmed(description: &str, p: &Primaries, trc: Trc, dim: f64) -> Vec<u8> {
    let [r, g, b] = colorants(p);
    let curve = curve_tag(trc);
    let mut tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc_tag(description)),
        (b"cprt", text_tag("tspmo, public domain")),
        (b"wtpt", xyz_tag(D50_XYZ)),
        (b"chad", chad_tag(&bradford(xyz(p.white), D50_XYZ))),
        (b"rXYZ", xyz_tag(r)),
        (b"gXYZ", xyz_tag(g)),
        (b"bXYZ", xyz_tag(b)),
        (b"rTRC", curve),
    ];
    if dim < 1.0 {
        tags.push((b"vcgt", vcgt_tag(dim.clamp(0.001, 1.0).powf(1.0 / 2.2))));
    }

    let table_len = 4 + 12 * (tags.len() + 2);
    let mut offset = 128 + table_len;
    let mut table = Vec::new();
    let mut data: Vec<u8> = Vec::new();
    table.extend(((tags.len() + 2) as u32).to_be_bytes());
    let mut trc_at = (0, 0);
    for (sig, body) in &tags {
        table.extend(*sig);
        table.extend((offset as u32).to_be_bytes());
        table.extend((body.len() as u32).to_be_bytes());
        if **sig == *b"rTRC" {
            trc_at = (offset as u32, body.len() as u32);
        }
        data.extend(body);
        while data.len() % 4 != 0 {
            data.push(0);
        }
        offset = 128 + table_len + data.len();
    }
    // The three channels share one curve.
    for sig in [b"gTRC", b"bTRC"] {
        table.extend(sig);
        table.extend(trc_at.0.to_be_bytes());
        table.extend(trc_at.1.to_be_bytes());
    }

    let mut out = vec![0u8; 128];
    out[8..12].copy_from_slice(&[2, 0x40, 0, 0]);
    out[12..16].copy_from_slice(b"mntr");
    out[16..20].copy_from_slice(b"RGB ");
    out[20..24].copy_from_slice(b"XYZ ");
    out[24..36].copy_from_slice(&[0x07, 0xea, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]); // 2026-01-01
    out[36..40].copy_from_slice(b"acsp");
    for (i, c) in D50_XYZ.iter().enumerate() {
        out[68 + 4 * i..72 + 4 * i].copy_from_slice(&s15(*c));
    }
    out.extend(table);
    out.extend(data);
    let len = out.len() as u32;
    out[0..4].copy_from_slice(&len.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    fn tag<'a>(b: &'a [u8], sig: &[u8; 4]) -> &'a [u8] {
        let n = be32(b, 128) as usize;
        for i in 0..n {
            let e = 132 + 12 * i;
            if &b[e..e + 4] == sig {
                let (off, len) = (be32(b, e + 4) as usize, be32(b, e + 8) as usize);
                return &b[off..off + len];
            }
        }
        panic!("no {}", String::from_utf8_lossy(sig));
    }

    fn xyz_of(t: &[u8]) -> Vec3 {
        [0, 1, 2].map(|i| be32(t, 8 + 4 * i) as i32 as f64 / 65536.0)
    }

    #[test]
    fn srgb_colorants_match_the_icc_reference() {
        let r = colorants(&SRGB);
        // The sRGB v2 profile's D50 colorants (ICC.1 sRGB reference).
        let expect = [
            [0.4360, 0.2225, 0.0139],
            [0.3851, 0.7169, 0.0971],
            [0.1431, 0.0606, 0.7141],
        ];
        for (got, want) in r.iter().zip(expect) {
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 0.002, "{got:?} vs {want:?}");
            }
        }
        // White maps to D50.
        let sum: Vec3 = [0, 1, 2].map(|i| r[0][i] + r[1][i] + r[2][i]);
        for (s, d) in sum.iter().zip(D50_XYZ) {
            assert!((s - d).abs() < 0.002);
        }
    }

    #[test]
    fn profile_structure_is_valid() {
        let b = build("Test panel", &DISPLAY_P3, Trc::Srgb);
        assert_eq!(be32(&b, 0) as usize, b.len());
        assert_eq!(&b[36..40], b"acsp");
        assert_eq!(&b[12..16], b"mntr");
        let n = be32(&b, 128) as usize;
        assert_eq!(n, 10);
        for i in 0..n {
            let e = 132 + 12 * i;
            let (off, len) = (be32(&b, e + 4) as usize, be32(&b, e + 8) as usize);
            assert_eq!(off % 4, 0);
            assert!(off + len <= b.len());
        }
        let curve = tag(&b, b"gTRC");
        assert_eq!(&curve[..4], b"curv");
        assert_eq!(be32(curve, 8), 1024);
        assert_eq!(tag(&b, b"rTRC").as_ptr(), curve.as_ptr());
        let last = u16::from_be_bytes([curve[curve.len() - 2], curve[curve.len() - 1]]);
        assert_eq!(last, 65535);
        let w = xyz_of(tag(&b, b"wtpt"));
        assert!((w[0] - 0.9642).abs() < 1e-3 && (w[2] - 0.8249).abs() < 1e-3);
        assert_eq!(&tag(&b, b"desc")[12..22], b"Test panel");
    }

    #[test]
    fn dimming_adds_a_scaled_vcgt() {
        let plain = build("x", &SRGB, Trc::Srgb);
        assert!(!plain.windows(4).any(|w| w == b"vcgt"));
        let dim = build_dimmed("x", &SRGB, Trc::Srgb, 0.25);
        let v = tag(&dim, b"vcgt");
        assert_eq!(be32(v, 8), 0); // table
        assert_eq!(u16::from_be_bytes([v[12], v[13]]), 3); // channels
        assert_eq!(u16::from_be_bytes([v[14], v[15]]), 256);
        let last_red = u16::from_be_bytes([v[18 + 2 * 255], v[19 + 2 * 255]]);
        // 25% of the light is 0.25^(1/2.2) = 53% of the encoded value.
        assert!(
            (f64::from(last_red) / 65535.0 - 0.532).abs() < 0.005,
            "{last_red}"
        );
        assert_eq!(be32(&dim, 0) as usize, dim.len());
    }

    #[test]
    fn white_point_is_declared_through_chad() {
        let b = build("x", &SRGB, Trc::Srgb);
        let c = tag(&b, b"chad");
        assert_eq!(&c[..4], b"sf32");
        // D65 -> D50 adaptation is close to identity but not equal to it.
        let m00 = be32(c, 8) as i32 as f64 / 65536.0;
        assert!((m00 - 1.0478).abs() < 0.01, "{m00}");
    }

    /// `TSPMO_ICC_OUT=/tmp/x TSPMO_ICC_DIM=0.4 cargo test -p tspmo-core write_sample -- --ignored`
    #[test]
    #[ignore]
    fn write_sample() {
        let dim: f64 = std::env::var("TSPMO_ICC_DIM")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);
        let out = std::env::var("TSPMO_ICC_OUT").unwrap();
        std::fs::write(out, build_dimmed("tspmo sample", &SRGB, Trc::Gamma(2.2), dim)).unwrap();
    }

    #[test]
    fn p3_is_wider_than_srgb() {
        let x = |c: Vec3| c[0] / (c[0] + c[1] + c[2]);
        let (s, p) = (colorants(&SRGB), colorants(&DISPLAY_P3));
        assert!(x(p[0]) > x(s[0])); // redder red
        assert!(x(p[1]) < x(s[1])); // greener green
    }
}
