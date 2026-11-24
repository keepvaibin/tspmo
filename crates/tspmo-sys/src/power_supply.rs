//! `/sys/class/power_supply`: batteries, AC adapters and USB-C sources.

use crate::SysRoot;

pub const CLASS: &str = "/sys/class/power_supply";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Battery {
    pub name: String,
    pub status: String,
    pub capacity: Option<u8>,
    /// Discharge (negative while discharging is *not* assumed: sign comes from the driver).
    pub power_w: Option<f32>,
    pub charge_start: Option<u8>,
    pub charge_end: Option<u8>,
    pub cycle_count: Option<u32>,
    /// Full capacity as a percentage of design capacity.
    pub health_pct: Option<f32>,
}

pub fn supplies(root: &SysRoot) -> Vec<(String, String)> {
    root.list(CLASS)
        .into_iter()
        .filter_map(|n| {
            let t = root.read_opt(format!("{CLASS}/{n}/type"))?;
            Some((n, t))
        })
        .collect()
}

pub fn batteries(root: &SysRoot) -> Vec<Battery> {
    supplies(root)
        .into_iter()
        .filter(|(_, t)| t == "Battery")
        // USB peripherals (mice, headsets) report `scope=Device`; we only want system batteries.
        .filter(|(n, _)| {
            root.read_opt(format!("{CLASS}/{n}/scope"))
                .is_none_or(|s| s == "System")
        })
        .map(|(name, _)| read_battery(root, name))
        .collect()
}

fn read_battery(root: &SysRoot, name: String) -> Battery {
    let dir = format!("{CLASS}/{name}");
    let num = |f: &str| root.read_parse::<f64>(format!("{dir}/{f}"));
    // Prefer power_now (µW); otherwise current_now (µA) × voltage_now (µV).
    let power_w = num("power_now")
        .map(|p| p / 1e6)
        .or_else(|| Some(num("current_now")? * num("voltage_now")? / 1e12))
        .map(|w| w.abs() as f32);
    let health_pct = num("energy_full")
        .zip(num("energy_full_design"))
        .or_else(|| num("charge_full").zip(num("charge_full_design")))
        .filter(|(_, d)| *d > 0.0)
        .map(|(f, d)| (f * 100.0 / d) as f32);
    Battery {
        status: root.read_opt(format!("{dir}/status")).unwrap_or_default(),
        capacity: root.read_parse(format!("{dir}/capacity")),
        power_w,
        charge_start: root.read_parse(format!("{dir}/charge_control_start_threshold")),
        charge_end: root.read_parse(format!("{dir}/charge_control_end_threshold")),
        cycle_count: root.read_parse(format!("{dir}/cycle_count")),
        health_pct,
        name,
    }
}

/// `Some(true)` on AC, `Some(false)` on battery, `None` if no mains supply is reported.
pub fn on_ac(root: &SysRoot) -> Option<bool> {
    let mains: Vec<_> = supplies(root).into_iter().filter(|(_, t)| t == "Mains").collect();
    if mains.is_empty() {
        return None;
    }
    Some(
        mains
            .iter()
            .any(|(n, _)| root.read_parse::<u8>(format!("{CLASS}/{n}/online")) == Some(1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::FakeRoot;

    #[test]
    fn battery_from_current_and_voltage() {
        let f = FakeRoot::new("psy");
        f.file("/sys/class/power_supply/ACAD/type", "Mains\n")
            .file("/sys/class/power_supply/ACAD/online", "0\n")
            .file("/sys/class/power_supply/BAT1/type", "Battery\n")
            .file("/sys/class/power_supply/BAT1/status", "Discharging\n")
            .file("/sys/class/power_supply/BAT1/capacity", "52\n")
            .file("/sys/class/power_supply/BAT1/current_now", "500000\n")
            .file("/sys/class/power_supply/BAT1/voltage_now", "16000000\n")
            .file("/sys/class/power_supply/BAT1/charge_full", "4500000\n")
            .file("/sys/class/power_supply/BAT1/charge_full_design", "5000000\n")
            .file(
                "/sys/class/power_supply/BAT1/charge_control_end_threshold",
                "80\n",
            );
        let root = f.root();
        assert_eq!(on_ac(&root), Some(false));
        let b = &batteries(&root)[0];
        assert_eq!(b.capacity, Some(52));
        assert_eq!(b.charge_end, Some(80));
        assert!((b.power_w.unwrap() - 8.0).abs() < 1e-3);
        assert!((b.health_pct.unwrap() - 90.0).abs() < 1e-3);
    }
}
