//! `tspmo dump`: copy the kernel interfaces tspmo reads into a directory tree that mirrors
//! their logical paths, so it can be used as `TSPMO_SYSROOT` for tests and bug reports.
//!
//! Privacy: no serial numbers, UUIDs or MAC addresses are copied, and the root filesystem
//! UUID is scrubbed from the kernel command line.

use std::fs;
use std::io;
use std::path::Path;
use tspmo_sys::SysRoot;

/// Logical path patterns captured by a dump (`*` matches one path segment, or part of one).
const PATTERNS: &[&str] = &[
    // identity (public fields only)
    "/sys/class/dmi/id/sys_vendor",
    "/sys/class/dmi/id/product_name",
    "/sys/class/dmi/id/product_family",
    "/sys/class/dmi/id/product_version",
    "/sys/class/dmi/id/product_sku",
    "/sys/class/dmi/id/board_vendor",
    "/sys/class/dmi/id/board_name",
    "/sys/class/dmi/id/board_version",
    "/sys/class/dmi/id/bios_vendor",
    "/sys/class/dmi/id/bios_version",
    "/sys/class/dmi/id/bios_date",
    "/sys/class/dmi/id/chassis_type",
    "/proc/cmdline",
    "/proc/sys/kernel/osrelease",
    // performance modes
    "/sys/firmware/acpi/platform_profile",
    "/sys/firmware/acpi/platform_profile_choices",
    "/sys/class/platform-profile/*/*",
    // firmware attributes (asus-armoury, lenovo-wmi-other, thinklmi)
    "/sys/class/firmware-attributes/*/attributes/*",
    "/sys/class/firmware-attributes/*/attributes/*/*",
    // vendor platform drivers
    "/sys/devices/platform/asus-nb-wmi/*",
    "/sys/bus/platform/devices/VPC2004:*/*",
    "/sys/devices/platform/thinkpad_acpi/*",
    // fans and sensors
    "/sys/class/hwmon/*/name",
    "/sys/class/hwmon/*/*_input",
    "/sys/class/hwmon/*/*_label",
    "/sys/class/hwmon/*/*_max",
    "/sys/class/hwmon/*/*_min",
    "/sys/class/hwmon/*/*_target",
    "/sys/class/hwmon/*/pwm*",
    // batteries and chargers
    "/sys/class/power_supply/*/*",
    // screen
    "/sys/class/backlight/*/*",
    "/sys/class/drm/*/status",
    "/sys/class/drm/*/enabled",
    "/sys/class/drm/*/modes",
    // keyboard / lighting
    "/sys/class/leds/*/brightness",
    "/sys/class/leds/*/max_brightness",
    "/sys/class/leds/*/multi_index",
    "/sys/class/leds/*/multi_intensity",
    // CPU power management
    "/sys/devices/system/cpu/cpufreq/policy0/*",
    "/sys/devices/system/cpu/intel_pstate/*",
    "/sys/devices/system/cpu/amd_pstate/*",
    "/sys/devices/system/cpu/cpufreq/boost",
    "/sys/class/powercap/intel-rapl:*/name",
    "/sys/class/powercap/intel-rapl:*/constraint_*",
    "/sys/class/powercap/intel-rapl:*/enabled",
    // GPUs and other PCI devices
    "/sys/bus/pci/devices/*/class",
    "/sys/bus/pci/devices/*/vendor",
    "/sys/bus/pci/devices/*/device",
    "/sys/bus/pci/devices/*/boot_vga",
    "/sys/bus/pci/devices/*/power_state",
    "/sys/bus/pci/devices/*/power/control",
    "/sys/bus/pci/devices/*/power/runtime_status",
    "/sys/module/nvidia/parameters/NVreg_DynamicPowerManagement",
    "/sys/module/pcie_aspm/parameters/policy",
    "/sys/module/snd_hda_intel/parameters/power_save",
    // USB devices
    "/sys/bus/usb/devices/*/idVendor",
    "/sys/bus/usb/devices/*/idProduct",
    "/sys/bus/usb/devices/*/product",
    "/sys/bus/usb/devices/*/power/control",
    "/sys/bus/usb/devices/*/power/runtime_status",
    // input devices (names + capabilities, used to find vendor keys)
    "/proc/bus/input/devices",
];

/// Files that must never leave the machine.
const DENY: &[&str] = &[
    "serial",
    "uuid",
    "address",
    "uevent",
    "modalias",
    "manufacturer_serial",
];

/// Largest file copied; sysfs attributes are tiny, anything bigger is not an attribute.
const MAX_LEN: u64 = 64 * 1024;

pub fn dump(root: &SysRoot, out: &Path) -> io::Result<usize> {
    fs::create_dir_all(out)?;
    let mut n = 0;
    for pattern in PATTERNS {
        for logical in root.glob(pattern) {
            let src = root.path(&logical);
            let name = logical.rsplit('/').next().unwrap_or_default();
            if DENY.iter().any(|d| name.contains(d)) {
                continue;
            }
            // Skip directories, write-only attributes, and anything unreadable.
            let Ok(meta) = fs::metadata(&src) else { continue };
            if !meta.is_file() || meta.len() > MAX_LEN {
                continue;
            }
            let Ok(mut contents) = fs::read(&src) else {
                continue;
            };
            if logical == "/proc/cmdline" {
                contents = scrub_cmdline(&String::from_utf8_lossy(&contents)).into_bytes();
            }
            let dst = out.join(logical.trim_start_matches('/'));
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(dst, contents)?;
            n += 1;
        }
    }
    // `driver` symlinks are how devices are matched to drivers; recreate them as symlinks.
    for logical in root.glob("/sys/bus/pci/devices/*/driver") {
        if let Some(name) = root.link_name(&logical) {
            let dst = out.join(logical.trim_start_matches('/'));
            let _ = std::os::unix::fs::symlink(format!("../../../bus/pci/drivers/{name}"), dst);
        }
    }
    Ok(n)
}

fn scrub_cmdline(cmdline: &str) -> String {
    let mut out: Vec<String> = cmdline
        .split_whitespace()
        .map(|p| match p.split_once('=') {
            Some((k, v)) if v.contains("UUID=") || k.ends_with("uuid") || k == "cryptdevice" => {
                format!("{k}=REDACTED")
            }
            _ => p.to_owned(),
        })
        .collect();
    out.push(String::new());
    out.join(" ").trim_end().to_owned() + "\n"
}

#[cfg(test)]
mod tests {
    use super::scrub_cmdline;

    #[test]
    fn scrubs_root_uuid() {
        assert_eq!(
            scrub_cmdline("quiet root=UUID=2005-abc rw rootflags=subvol=/@\n"),
            "quiet root=REDACTED rw rootflags=subvol=/@\n"
        );
    }
}
