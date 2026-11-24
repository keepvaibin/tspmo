//! Runtime power management for the laptop's internal vendor USB devices (keyboard controller,
//! lighting / Slash / AniMe controllers).
//!
//! While these stay awake the USB controller blocks the CPU's deepest idle state. Measured on a
//! GU605MI at light use: package C10 0.2% → 12.7% and −0.57 W platform power with them asleep.
//!
//! Safe only with a keyboard driver that wakes the device before it sends backlight / Fn-lock
//! commands (tspmo's patched `hid-asus`, which exposes `wake_for_reports`); with the stock
//! driver those commands fail while the keyboard sleeps. tspmo therefore enables autosuspend
//! only when the patched driver is loaded.

use tspmo_sys::{SysRoot, usb};

pub const WAKE_MARKER: &str = "/sys/module/hid_asus/parameters/wake_for_reports";
const VENDORS: &[u16] = &[0x0b05 /* ASUS */];

/// Internal (non-removable) vendor USB devices that can autosuspend.
pub fn internal_devices(root: &SysRoot) -> Vec<usb::UsbDevice> {
    usb::devices(root)
        .into_iter()
        .filter(|d| VENDORS.contains(&d.vendor))
        .filter(|d| {
            root.read_opt(format!("{}/{}/removable", usb::DEVICES, d.port))
                .is_none_or(|r| r == "fixed")
        })
        .collect()
}

/// Whether the running keyboard driver tolerates autosuspend.
pub fn autosuspend_safe(root: &SysRoot) -> bool {
    root.read_opt(WAKE_MARKER).is_some_and(|v| v == "Y")
}

/// Set `power/control` (`auto` = may sleep, `on` = always powered) on the internal devices.
/// Returns how many devices changed.
pub fn set_control(root: &SysRoot, auto: bool) -> usize {
    let want = if auto { "auto" } else { "on" };
    internal_devices(root)
        .iter()
        .filter(|d| d.pm_control.as_deref() != Some(want))
        .filter(|d| {
            root.write(format!("{}/{}/power/control", usb::DEVICES, d.port), want)
                .is_ok()
        })
        .count()
}

/// Bring the devices to the right state: asleep-capable when safe, otherwise left alone.
pub fn apply(root: &SysRoot) -> usize {
    if autosuspend_safe(root) {
        set_control(root, true)
    } else {
        0
    }
}
