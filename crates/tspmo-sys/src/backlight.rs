//! `/sys/class/backlight` devices.

use crate::{SysRoot, pci};

pub const CLASS: &str = "/sys/class/backlight";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlight {
    pub name: String,
    /// `raw`, `platform` or `firmware`.
    pub kind: String,
    pub brightness: u32,
    pub max: u32,
    pub actual: Option<u32>,
    /// False for backlights that were deliberately not read (NVIDIA's).
    pub sampled: bool,
}

impl Backlight {
    pub fn percent(&self) -> f32 {
        if self.max == 0 {
            0.0
        } else {
            self.brightness as f32 * 100.0 / self.max as f32
        }
    }

    /// Backlights registered by the NVIDIA driver (`nvidia_0`, `nvidia_wmi_ec_backlight`).
    pub fn is_nvidia(&self) -> bool {
        self.name.starts_with("nvidia") || !self.sampled
    }
}

/// Every backlight device. NVIDIA's are listed by name and type only: reading their `brightness`
/// or `actual_brightness` runtime-resumes the dGPU, and a wake that coincides with a power
/// event has crashed its firmware. Their values are left at 0 and `sampled` is false.
pub fn list(root: &SysRoot) -> Vec<Backlight> {
    root.list(CLASS)
        .into_iter()
        .filter_map(|name| {
            let dir = format!("{CLASS}/{name}");
            let kind = root.read_opt(format!("{dir}/type")).unwrap_or_default();
            if name.starts_with("nvidia") || pci::entry_is_nvidia(root, &dir) {
                return Some(Backlight {
                    name,
                    kind,
                    brightness: 0,
                    max: 0,
                    actual: None,
                    sampled: false,
                });
            }
            Some(Backlight {
                kind,
                brightness: root.read_parse(format!("{dir}/brightness"))?,
                max: root.read_parse(format!("{dir}/max_brightness"))?,
                actual: root.read_parse(format!("{dir}/actual_brightness")),
                sampled: true,
                name,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::FakeRoot;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn nvidia_backlights_are_never_read() {
        let f = FakeRoot::new("bl-nvidia");
        f.file("/sys/class/backlight/intel_backlight/type", "raw\n")
            .file("/sys/class/backlight/intel_backlight/brightness", "78\n")
            .file("/sys/class/backlight/intel_backlight/actual_brightness", "78\n")
            .file("/sys/class/backlight/intel_backlight/max_brightness", "512\n")
            .file("/sys/class/backlight/nvidia_0/type", "raw\n")
            .file("/sys/class/backlight/nvidia_wmi_ec_backlight/type", "firmware\n");
        // Reading these would wake the GPU: make any read fail loudly (mode 000 makes a read
        // of a regular file an error; the test would also see the values if they were read).
        for n in ["nvidia_0", "nvidia_wmi_ec_backlight"] {
            for a in ["brightness", "actual_brightness", "max_brightness"] {
                f.file(&format!("/sys/class/backlight/{n}/{a}"), "99\n");
                let p = f.path().join(format!("sys/class/backlight/{n}/{a}"));
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
            }
        }
        let list = list(&f.root());
        assert_eq!(list.len(), 3);
        let nv: Vec<_> = list.iter().filter(|b| b.is_nvidia()).collect();
        assert_eq!(nv.len(), 2);
        assert!(nv.iter().all(|b| !b.sampled && b.max == 0 && b.actual.is_none()));
        let intel = list.iter().find(|b| b.name == "intel_backlight").unwrap();
        assert!(intel.sampled);
        assert_eq!((intel.brightness, intel.max), (78, 512));
    }

    #[test]
    fn nvidia_backlight_is_found_by_its_device_too() {
        let f = FakeRoot::new("bl-dev");
        f.file("/sys/bus/pci/devices/0000:01:00.0/class", "0x030200\n")
            .file("/sys/bus/pci/devices/0000:01:00.0/vendor", "0x10de\n")
            .file("/sys/class/backlight/panel0/type", "raw\n")
            .file("/sys/class/backlight/panel0/brightness", "1\n");
        let p = f.path().join("sys/class/backlight/panel0/device");
        std::os::unix::fs::symlink("../../../bus/pci/devices/0000:01:00.0", p).unwrap();
        let b = &list(&f.root())[0];
        assert!(!b.sampled && b.brightness == 0);
    }
}
