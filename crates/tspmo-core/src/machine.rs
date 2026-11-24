//! A snapshot of the machine's identity and the control surfaces it exposes.

use tspmo_sys::{SysRoot, backlight, dmi, fwattr, hwmon, pci, power_supply};

pub const PLATFORM_PROFILE: &str = "/sys/firmware/acpi/platform_profile";
pub const ASUS_WMI: &str = "/sys/devices/platform/asus-nb-wmi";
pub const IDEAPAD_GLOB: &str = "/sys/bus/platform/devices/VPC2004:*";
pub const THINKPAD: &str = "/sys/devices/platform/thinkpad_acpi";

#[derive(Debug, Clone)]
pub struct Machine {
    pub dmi: dmi::Dmi,
    pub platform_profile: Option<String>,
    pub platform_profile_choices: Vec<String>,
    pub fw_drivers: Vec<String>,
    pub fw_attributes: Vec<fwattr::Attribute>,
    pub hwmons: Vec<hwmon::Hwmon>,
    pub gpus: Vec<pci::Gpu>,
    pub backlights: Vec<backlight::Backlight>,
    pub batteries: Vec<power_supply::Battery>,
    pub on_ac: Option<bool>,
    /// Vendor platform drivers present (`asus-nb-wmi`, `ideapad`, `thinkpad_acpi`).
    pub platform_drivers: Vec<&'static str>,
}

impl Machine {
    pub fn probe(root: &SysRoot) -> Self {
        let fw_drivers = fwattr::drivers(root);
        let fw_attributes = fw_drivers
            .iter()
            .flat_map(|d| fwattr::enumerate(root, d))
            .collect();
        let mut platform_drivers = Vec::new();
        if root.exists(ASUS_WMI) {
            platform_drivers.push("asus-nb-wmi");
        }
        if !root.glob(IDEAPAD_GLOB).is_empty() {
            platform_drivers.push("ideapad");
        }
        if root.exists(THINKPAD) {
            platform_drivers.push("thinkpad_acpi");
        }
        Self {
            dmi: dmi::Dmi::read(root),
            platform_profile: root.read_opt(PLATFORM_PROFILE),
            platform_profile_choices: root
                .read_opt(format!("{PLATFORM_PROFILE}_choices"))
                .map(|s| s.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
            fw_drivers,
            fw_attributes,
            hwmons: hwmon::list(root),
            gpus: pci::gpus(root),
            backlights: backlight::list(root),
            batteries: power_supply::batteries(root),
            on_ac: power_supply::on_ac(root),
            platform_drivers,
        }
    }

    pub fn has_nvidia(&self) -> bool {
        self.gpus.iter().any(|g| g.vendor == pci::VENDOR_NVIDIA)
    }

    pub fn fw_attr(&self, name: &str) -> Option<&fwattr::Attribute> {
        self.fw_attributes.iter().find(|a| a.name == name)
    }
}
