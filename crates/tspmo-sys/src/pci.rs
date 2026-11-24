//! PCI display devices and their runtime power state. Reading these never wakes a GPU.

use crate::SysRoot;

pub const DEVICES: &str = "/sys/bus/pci/devices";

pub const VENDOR_INTEL: u16 = 0x8086;
pub const VENDOR_AMD: u16 = 0x1002;
pub const VENDOR_NVIDIA: u16 = 0x10de;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    pub address: String,
    pub vendor: u16,
    pub device: u16,
    pub driver: Option<String>,
    /// `active`, `suspended`, ... from `power/runtime_status`.
    pub runtime_status: Option<String>,
    /// `D0`, `D3hot`, `D3cold`, ... from `power_state`.
    pub power_state: Option<String>,
    /// `power/control`: `auto` lets the device runtime-suspend.
    pub pm_control: Option<String>,
    pub boot_vga: bool,
    /// `power/runtime_active_time` and `power/runtime_suspended_time`, milliseconds since boot
    /// (kernel counters; reading them never wakes the device).
    pub runtime_active_ms: Option<u64>,
    pub runtime_suspended_ms: Option<u64>,
}

impl Gpu {
    pub fn vendor_name(&self) -> &'static str {
        match self.vendor {
            VENDOR_INTEL => "Intel",
            VENDOR_AMD => "AMD",
            VENDOR_NVIDIA => "NVIDIA",
            _ => "Unknown",
        }
    }

    pub fn is_asleep(&self) -> bool {
        self.runtime_status.as_deref() == Some("suspended")
    }

    /// Share of the runtime-PM time (since boot) the device spent awake, 0 to 1.
    pub fn awake_share(&self) -> Option<f64> {
        let (a, s) = (self.runtime_active_ms? as f64, self.runtime_suspended_ms? as f64);
        (a + s > 0.0).then(|| a / (a + s))
    }
}

fn hex(s: Option<String>) -> Option<u16> {
    u16::from_str_radix(s?.trim().trim_start_matches("0x"), 16).ok()
}

/// All display-class (0x03xxxx) PCI devices.
pub fn gpus(root: &SysRoot) -> Vec<Gpu> {
    root.list(DEVICES)
        .into_iter()
        .filter_map(|addr| {
            let dir = format!("{DEVICES}/{addr}");
            let class = root.read_opt(format!("{dir}/class"))?;
            if !class.trim_start_matches("0x").starts_with("03") {
                return None;
            }
            Some(Gpu {
                vendor: hex(root.read_opt(format!("{dir}/vendor")))?,
                device: hex(root.read_opt(format!("{dir}/device"))).unwrap_or(0),
                driver: root.link_name(format!("{dir}/driver")),
                runtime_status: root.read_opt(format!("{dir}/power/runtime_status")),
                power_state: root.read_opt(format!("{dir}/power_state")),
                pm_control: root.read_opt(format!("{dir}/power/control")),
                boot_vga: root.read_parse::<u8>(format!("{dir}/boot_vga")) == Some(1),
                runtime_active_ms: root.read_parse(format!("{dir}/power/runtime_active_time")),
                runtime_suspended_ms: root.read_parse(format!("{dir}/power/runtime_suspended_time")),
                address: addr,
            })
        })
        .collect()
}

// --- keeping away from the dGPU ----------------------------------------------------------
//
// Reading most attributes that belong to an NVIDIA GPU (its backlight, hwmon sensors, DRM
// connector status or EDID, /proc/driver/nvidia) makes the driver runtime-resume the GPU. A wake
// that coincides with a power event has crashed its firmware (Xid 119/154) until reboot. So
// nothing outside this module's own `gpus()` list (vendor, class, device, boot_vga,
// power/runtime_status, power/runtime_active_time, power/runtime_suspended_time, power/control,
// power_state) is ever read from such a device. These
// helpers decide, from names and symlink targets only, whether a sysfs class entry is one.

/// PCI addresses of the NVIDIA display devices (from attributes that never wake them).
pub fn nvidia_addresses(root: &SysRoot) -> Vec<String> {
    gpus(root)
        .into_iter()
        .filter(|g| g.vendor == VENDOR_NVIDIA)
        .map(|g| g.address)
        .collect()
}

/// Does the class entry (`/sys/class/<class>/<name>`) hang off an NVIDIA GPU? Looks only at the
/// target name of its `device` symlink.
pub fn entry_is_nvidia(root: &SysRoot, entry_dir: &str) -> bool {
    root.link_name(format!("{entry_dir}/device"))
        .is_some_and(|addr| nvidia_addresses(root).contains(&addr))
}

/// `card0-DP-5` and `card0` are both on `card0`.
pub fn drm_card(entry: &str) -> &str {
    entry.split('-').next().unwrap_or(entry)
}

/// Is this `/sys/class/drm` entry (card or connector) on an NVIDIA GPU?
pub fn drm_entry_is_nvidia(root: &SysRoot, entry: &str) -> bool {
    entry_is_nvidia(root, &format!("/sys/class/drm/{}", drm_card(entry)))
}

/// Is this logical path inside something that belongs to an NVIDIA GPU and is not one of the
/// attributes that are safe to read? Used by `tspmo dump` and anything globbing sysfs.
pub fn touches_dgpu(root: &SysRoot, logical: &str) -> bool {
    let parts: Vec<&str> = logical.split('/').filter(|p| !p.is_empty()).collect();
    match parts[..] {
        ["sys", "class", "backlight", name, ..] => {
            name.starts_with("nvidia") || entry_is_nvidia(root, &format!("/sys/class/backlight/{name}"))
        }
        ["sys", "class", "hwmon", name, ..] => entry_is_nvidia(root, &format!("/sys/class/hwmon/{name}")),
        ["sys", "class", "drm", name, ..] => drm_entry_is_nvidia(root, name),
        ["proc", "driver", "nvidia", ..] => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::FakeRoot;
    use std::os::unix::fs::symlink;

    /// An Intel iGPU at 00:02.0 and an NVIDIA dGPU at 01:00.0, with the entries that hang off them.
    fn laptop(tag: &str) -> FakeRoot {
        let f = FakeRoot::new(tag);
        for (addr, vendor, boot) in [("0000:00:02.0", "0x8086", "1"), ("0000:01:00.0", "0x10de", "0")] {
            f.file(&format!("/sys/bus/pci/devices/{addr}/class"), "0x030000\n")
                .file(
                    &format!("/sys/bus/pci/devices/{addr}/vendor"),
                    &format!("{vendor}\n"),
                )
                .file(
                    &format!("/sys/bus/pci/devices/{addr}/boot_vga"),
                    &format!("{boot}\n"),
                );
        }
        let link = |from: &str, to: &str| {
            let p = f.path().join(from.trim_start_matches('/'));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            symlink(to, p).unwrap();
        };
        f.file("/sys/class/drm/card0/x", "");
        f.file("/sys/class/drm/card1/x", "");
        link(
            "/sys/class/drm/card0/device",
            "../../../bus/pci/devices/0000:01:00.0",
        );
        link(
            "/sys/class/drm/card1/device",
            "../../../bus/pci/devices/0000:00:02.0",
        );
        f.file("/sys/class/hwmon/hwmon3/name", "nouveau\n");
        link(
            "/sys/class/hwmon/hwmon3/device",
            "../../../bus/pci/devices/0000:01:00.0",
        );
        f.file("/sys/class/hwmon/hwmon4/name", "coretemp\n");
        f
    }

    #[test]
    fn recognises_entries_of_the_nvidia_gpu() {
        let f = laptop("nv-entries");
        let r = f.root();
        assert_eq!(nvidia_addresses(&r), ["0000:01:00.0"]);
        assert!(drm_entry_is_nvidia(&r, "card0-DP-5"));
        assert!(drm_entry_is_nvidia(&r, "card0"));
        assert!(!drm_entry_is_nvidia(&r, "card1-eDP-1"));
        assert!(touches_dgpu(&r, "/sys/class/drm/card0-HDMI-A-1/edid"));
        assert!(!touches_dgpu(&r, "/sys/class/drm/card1-eDP-1/edid"));
        assert!(touches_dgpu(&r, "/sys/class/hwmon/hwmon3/temp1_input"));
        assert!(!touches_dgpu(&r, "/sys/class/hwmon/hwmon4/temp1_input"));
        assert!(touches_dgpu(&r, "/sys/class/backlight/nvidia_0/brightness"));
        assert!(touches_dgpu(&r, "/proc/driver/nvidia/gpus/0000:01:00.0/power"));
        assert!(!touches_dgpu(
            &r,
            "/sys/class/backlight/intel_backlight/brightness"
        ));
    }
}
