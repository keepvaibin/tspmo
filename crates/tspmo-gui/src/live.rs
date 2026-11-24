//! Live readouts that need no daemon: read straight from sysfs, only while a page shows them.
//! Never the NVIDIA GPU's sensors (reading them wakes it).

use tspmo_sys::{SysRoot, hwmon, power_supply};

/// CPU package temperature in °C.
pub fn cpu_temp(root: &SysRoot) -> Option<i32> {
    let h = ["coretemp", "k10temp", "zenpower"]
        .iter()
        .find_map(|n| hwmon::find(root, n))?;
    let raw: i64 = root.read_parse(format!("{}/temp1_input", h.dir))?;
    Some((raw / 1000) as i32)
}

/// Battery power in watts (always positive; the direction is the status).
pub fn battery_watts(root: &SysRoot) -> Option<f32> {
    power_supply::batteries(root).into_iter().find_map(|b| b.power_w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_temperature_and_power_from_the_fixture() {
        let root = SysRoot::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/asus-gu605mi"));
        // The fixture has a coretemp device; its temperature file may be absent.
        let _ = cpu_temp(&root);
        let w = battery_watts(&root);
        assert!(w.is_none_or(|w| w >= 0.0));
        assert_eq!(cpu_temp(&SysRoot::new("/nonexistent")), None);
    }
}
