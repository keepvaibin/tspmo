//! hidraw access for ASUS USB controllers (keyboard lighting, Slash, AniMe).
//!
//! Found by USB id through sysfs; the report descriptor says which report ids a node speaks.
//! A node is opened **per write and closed again**: holding it open keeps the USB device out
//! of autosuspend and costs battery.

use std::io::{self, Write};
use tspmo_sys::SysRoot;

pub const CLASS: &str = "/sys/class/hidraw";
pub const ASUS_VENDOR: u16 = 0x0b05;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidRaw {
    /// `hidraw2`
    pub node: String,
    pub vendor: u16,
    pub product: u16,
    /// `(report id, output report length in bytes without the id)`.
    pub outputs: Vec<(u8, usize)>,
}

impl HidRaw {
    /// Bytes to write for a packet on `report_id`: the report's full length (id included) when
    /// the descriptor declares one, otherwise the packet as is.
    pub fn padded(&self, report_id: u8, packet: &[u8]) -> Vec<u8> {
        let len = self
            .outputs
            .iter()
            .find(|(id, _)| *id == report_id)
            .map_or(packet.len(), |(_, n)| n + 1);
        let mut v = packet.to_vec();
        if v.len() < len {
            v.resize(len, 0);
        }
        v
    }

    pub fn speaks(&self, report_id: u8) -> bool {
        self.outputs.iter().any(|(id, _)| *id == report_id)
    }
}

/// Output reports `(id, bytes)` declared by a HID report descriptor.
pub fn output_reports(desc: &[u8]) -> Vec<(u8, usize)> {
    let (mut id, mut size, mut count) = (0u8, 0usize, 0usize);
    let mut bits: Vec<(u8, usize)> = Vec::new();
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xfe {
            // Long item: length in the next byte.
            i += 3 + usize::from(*desc.get(i + 1).unwrap_or(&0));
            continue;
        }
        let n = match prefix & 3 {
            3 => 4,
            n => usize::from(n),
        };
        let mut v = 0usize;
        for (k, b) in desc.iter().skip(i + 1).take(n).enumerate() {
            v |= usize::from(*b) << (8 * k);
        }
        match (prefix >> 2 & 3, prefix >> 4) {
            (1, 7) => size = v,
            (1, 9) => count = v,
            (1, 8) => id = v as u8,
            // Main item: Output.
            (0, 9) => match bits.iter_mut().find(|(i, _)| *i == id) {
                Some(e) => e.1 += size * count,
                None => bits.push((id, size * count)),
            },
            _ => {}
        }
        i += 1 + n;
    }
    bits.into_iter().map(|(id, b)| (id, b / 8)).collect()
}

fn parse_hid_id(s: &str) -> Option<(u16, u16)> {
    // HID_ID=0003:00000B05:000019B6
    let mut it = s.split(':');
    it.next()?;
    let v = u32::from_str_radix(it.next()?, 16).ok()?;
    let p = u32::from_str_radix(it.next()?, 16).ok()?;
    Some((u16::try_from(v).ok()?, u16::try_from(p).ok()?))
}

/// Every hidraw node of a USB device `vendor:product`.
pub fn find_all(root: &SysRoot, vendor: u16, product: u16) -> Vec<HidRaw> {
    root.list(CLASS)
        .into_iter()
        .filter_map(|node| {
            let uevent = root.read_opt(format!("{CLASS}/{node}/device/uevent"))?;
            let id = uevent.lines().find_map(|l| l.strip_prefix("HID_ID="))?;
            let (v, p) = parse_hid_id(id)?;
            if (v, p) != (vendor, product) {
                return None;
            }
            let desc = std::fs::read(root.path(format!("{CLASS}/{node}/device/report_descriptor")))
                .unwrap_or_default();
            Some(HidRaw {
                node,
                vendor: v,
                product: p,
                outputs: output_reports(&desc),
            })
        })
        .collect()
}

/// The node of `vendor:product` that accepts output report `report_id`.
pub fn find(root: &SysRoot, vendor: u16, product: u16, report_id: u8) -> Option<HidRaw> {
    find_all(root, vendor, product)
        .into_iter()
        .find(|h| h.speaks(report_id))
}

/// Open the node, write each packet with one `write(2)`, close it.
pub fn write(root: &SysRoot, hid: &HidRaw, packets: &[Vec<u8>]) -> io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path(format!("/dev/{}", hid.node)))?;
    for p in packets {
        f.write_all(p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asus::testfix::Fix;

    #[test]
    fn keyboard_descriptor_has_aura_report() {
        let fx = Fix::new("hid-kbd");
        let h = find(&fx.root(), ASUS_VENDOR, 0x19b6, 0x5d).unwrap();
        assert_eq!(h.node, "hidraw2");
        // 0x5d: 62 data bytes (+ id).
        assert!(h.outputs.contains(&(0x5d, 62)), "{:?}", h.outputs);
        assert_eq!(h.padded(0x5d, &[0x5d, 0xb4]).len(), 63);
        assert_eq!(h.padded(0x5d, &[0x5d, 0xb4])[..2], [0x5d, 0xb4]);
        // The keyboard doesn't speak the Slash's report.
        assert!(find(&fx.root(), ASUS_VENDOR, 0x19b6, 0x5e).is_none());
    }

    #[test]
    fn slash_descriptor_has_its_report() {
        let fx = Fix::new("hid-slash");
        let h = find(&fx.root(), ASUS_VENDOR, 0x193b, 0x5e).unwrap();
        assert_eq!(h.node, "hidraw1");
        assert!(h.outputs.contains(&(0x5e, 127)), "{:?}", h.outputs);
    }

    #[test]
    fn missing_device() {
        let fx = Fix::new("hid-none");
        assert!(find(&fx.root(), ASUS_VENDOR, 0x1234, 0x5d).is_none());
        assert!(find_all(&fx.root(), 0x2808, 0x0219).len() <= 1);
    }

    #[test]
    fn hid_id_parse() {
        assert_eq!(parse_hid_id("0003:00000B05:000019B6"), Some((0x0b05, 0x19b6)));
        assert_eq!(parse_hid_id("junk"), None);
    }

    #[test]
    fn writes_each_packet_and_closes() {
        let fx = Fix::new("hid-write");
        let root = fx.root();
        let h = find(&root, ASUS_VENDOR, 0x19b6, 0x5d).unwrap();
        std::fs::create_dir_all(root.path("/dev")).unwrap();
        std::fs::write(root.path("/dev/hidraw2"), b"").unwrap();
        write(&root, &h, &[vec![1, 2], vec![3]]).unwrap();
        assert_eq!(std::fs::read(root.path("/dev/hidraw2")).unwrap(), vec![1, 2, 3]);
    }
}
