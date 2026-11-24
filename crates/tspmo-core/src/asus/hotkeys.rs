//! ASUS vendor keys: which input device sends which Linux key code for which key.
//!
//! The two sources (kernel 7.3, `asus-nb-wmi` keymap and `hid-asus` mapping) reuse the same
//! `KEY_PROG*` codes for different keys, so a code means nothing without its source.

use tspmo_sys::SysRoot;

pub const KEY_PROG1: u16 = 148;
pub const KEY_PROG2: u16 = 149;
pub const KEY_PROG3: u16 = 202;
pub const KEY_PROG4: u16 = 203;
pub const KEY_FN_F5: u16 = 0x1d6;
pub const KEY_KBDILLUMUP: u16 = 230;
pub const KEY_KBDILLUMDOWN: u16 = 229;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `Asus WMI hotkeys` (`asus-nb-wmi`).
    Wmi,
    /// The ASUS USB keyboard (`hid-asus`).
    Keyboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    /// ROG / Armoury Crate key.
    Rog,
    /// Aura key: next effect.
    AuraNext,
    /// Fn+Left on keyboards that have a previous-effect key.
    AuraPrev,
    /// Fn+F5, cycles the thermal mode.
    Fan,
}

/// `hid-asus` `asus_input_mapping` (vendor usage page 0xff31 usages 0x38, 0xb2, 0xb3, 0xae/0x99)
/// and `asus-nb-wmi`'s sparse keymap (0x38/0xe0 Armoury, 0xb3 Aura, 0xae/0x99 fan).
pub fn decode(source: Source, code: u16) -> Option<Hotkey> {
    match (source, code) {
        (Source::Keyboard, KEY_PROG1) => Some(Hotkey::Rog),
        (Source::Keyboard, KEY_PROG2) => Some(Hotkey::AuraPrev),
        (Source::Keyboard, KEY_PROG3) => Some(Hotkey::AuraNext),
        (Source::Keyboard, KEY_PROG4) => Some(Hotkey::Fan),
        (Source::Wmi, KEY_PROG3) => Some(Hotkey::Rog),
        (Source::Wmi, KEY_PROG4) => Some(Hotkey::AuraNext),
        (Source::Wmi, KEY_FN_F5) => Some(Hotkey::Fan),
        _ => None,
    }
}

const INPUT: &str = "/sys/class/input";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    /// `/dev/input/event4`
    pub node: String,
    pub name: String,
    pub source: Source,
}

/// Input devices that carry ASUS vendor keys: the WMI hotkey device and ASUS USB keyboards.
pub fn input_devices(root: &SysRoot) -> Vec<InputDevice> {
    root.list(INPUT)
        .into_iter()
        .filter(|n| n.starts_with("event"))
        .filter_map(|n| {
            let dir = format!("{INPUT}/{n}/device");
            let name = root.read_opt(format!("{dir}/name"))?;
            let id = |f: &str| root.read_opt(format!("{dir}/id/{f}"));
            let usb_asus =
                id("bustype").as_deref() == Some("0003") && id("vendor").as_deref() == Some("0b05");
            let source = if name == "Asus WMI hotkeys" {
                Source::Wmi
            } else if usb_asus {
                Source::Keyboard
            } else {
                return None;
            };
            Some(InputDevice {
                node: format!("/dev/input/{n}"),
                name,
                source,
            })
        })
        .collect()
}

/// Touchpads count as activity for the keyboard-light idle timeout.
pub fn touchpads(root: &SysRoot) -> Vec<String> {
    root.list(INPUT)
        .into_iter()
        .filter(|n| n.starts_with("event"))
        .filter(|n| {
            root.read_opt(format!("{INPUT}/{n}/device/name"))
                .is_some_and(|name| name.ends_with("Touchpad"))
        })
        .map(|n| format!("/dev/input/{n}"))
        .collect()
}

/// One `struct input_event` as the kernel writes it on 64-bit Linux (24 bytes).
pub const EVENT_SIZE: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

pub const EV_KEY: u16 = 1;

pub fn parse_event(b: &[u8; EVENT_SIZE]) -> InputEvent {
    InputEvent {
        kind: u16::from_ne_bytes([b[16], b[17]]),
        code: u16::from_ne_bytes([b[18], b[19]]),
        value: i32::from_ne_bytes([b[20], b[21], b[22], b[23]]),
    }
}

impl InputEvent {
    /// A key going down (not a repeat, not a release).
    pub fn press(&self) -> Option<u16> {
        (self.kind == EV_KEY && self.value == 1).then_some(self.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tspmo_sys::SysRoot;

    #[test]
    fn same_code_different_keys() {
        assert_eq!(decode(Source::Keyboard, KEY_PROG1), Some(Hotkey::Rog));
        assert_eq!(decode(Source::Wmi, KEY_PROG3), Some(Hotkey::Rog));
        assert_eq!(decode(Source::Keyboard, KEY_PROG3), Some(Hotkey::AuraNext));
        assert_eq!(decode(Source::Wmi, KEY_PROG4), Some(Hotkey::AuraNext));
        assert_eq!(decode(Source::Keyboard, KEY_PROG4), Some(Hotkey::Fan));
        assert_eq!(decode(Source::Wmi, 30), None);
    }

    #[test]
    fn events_parse() {
        let mut b = [0u8; EVENT_SIZE];
        b[16..18].copy_from_slice(&EV_KEY.to_ne_bytes());
        b[18..20].copy_from_slice(&KEY_PROG1.to_ne_bytes());
        b[20..24].copy_from_slice(&1i32.to_ne_bytes());
        assert_eq!(parse_event(&b).press(), Some(KEY_PROG1));
        b[20..24].copy_from_slice(&2i32.to_ne_bytes()); // autorepeat
        assert_eq!(parse_event(&b).press(), None);
        b[20..24].copy_from_slice(&0i32.to_ne_bytes()); // release
        assert_eq!(parse_event(&b).press(), None);
    }

    fn dev(root: &std::path::Path, n: &str, name: &str, bus: &str, vendor: &str) {
        let d = root.join(format!("sys/class/input/{n}/device"));
        std::fs::create_dir_all(d.join("id")).unwrap();
        std::fs::write(d.join("name"), format!("{name}\n")).unwrap();
        std::fs::write(d.join("id/bustype"), format!("{bus}\n")).unwrap();
        std::fs::write(d.join("id/vendor"), format!("{vendor}\n")).unwrap();
    }

    #[test]
    fn finds_asus_devices_only() {
        let dir = std::env::temp_dir().join(format!("tspmo-hotkeys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dev(&dir, "event4", "Asus WMI hotkeys", "0019", "0000");
        dev(&dir, "event5", "ITE Tech. Inc. ITE Device(8910)", "0003", "0b05");
        dev(&dir, "event6", "Logitech USB Receiver", "0003", "046d");
        dev(&dir, "event10", "ASUF1207:00 2808:0219 Touchpad", "0018", "2808");
        dev(&dir, "mouse2", "ITE Tech. Inc. ITE Device(8910)", "0003", "0b05");
        let root = SysRoot::new(&dir);
        let d = input_devices(&root);
        assert_eq!(
            d.iter().map(|d| (d.node.as_str(), d.source)).collect::<Vec<_>>(),
            vec![
                ("/dev/input/event4", Source::Wmi),
                ("/dev/input/event5", Source::Keyboard)
            ]
        );
        assert_eq!(touchpads(&root), vec!["/dev/input/event10"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
