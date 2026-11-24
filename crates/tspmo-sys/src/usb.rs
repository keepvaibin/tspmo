//! USB devices and their runtime power management.

use crate::SysRoot;

pub const DEVICES: &str = "/sys/bus/usb/devices";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    /// Bus path such as `3-6`.
    pub port: String,
    pub vendor: u16,
    pub product: u16,
    pub name: Option<String>,
    /// `auto` allows autosuspend; `on` keeps it powered.
    pub pm_control: Option<String>,
    pub runtime_status: Option<String>,
}

impl UsbDevice {
    pub fn id(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor, self.product)
    }

    pub fn autosuspend_enabled(&self) -> bool {
        self.pm_control.as_deref() == Some("auto")
    }
}

/// Physical USB devices (not root hubs or interfaces).
pub fn devices(root: &SysRoot) -> Vec<UsbDevice> {
    root.list(DEVICES)
        .into_iter()
        .filter(|p| !p.contains(':') && !p.starts_with("usb"))
        .filter_map(|port| {
            let dir = format!("{DEVICES}/{port}");
            let hex = |f: &str| u16::from_str_radix(&root.read_opt(format!("{dir}/{f}"))?, 16).ok();
            Some(UsbDevice {
                vendor: hex("idVendor")?,
                product: hex("idProduct")?,
                name: root.read_opt(format!("{dir}/product")),
                pm_control: root.read_opt(format!("{dir}/power/control")),
                runtime_status: root.read_opt(format!("{dir}/power/runtime_status")),
                port,
            })
        })
        .collect()
}
