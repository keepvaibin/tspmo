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
                address: addr,
            })
        })
        .collect()
}
