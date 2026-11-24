//! Battery: charge limit, health, live power and time left.

use super::quirks::Quirks;
use tspmo_sys::{SysRoot, power_supply};

const CLASS: &str = power_supply::CLASS;

/// The system battery that has a charge limit, or the first system battery.
pub fn find(root: &SysRoot) -> Option<String> {
    let all = power_supply::batteries(root);
    all.iter()
        .find(|b| b.charge_end.is_some())
        .or_else(|| all.first())
        .map(|b| b.name.clone())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    pub status: String,
    pub percent: Option<u8>,
    /// Charge limit in force, if the battery has one.
    pub limit: Option<u8>,
    /// Full capacity as a percentage of design capacity.
    pub health: Option<f64>,
    pub cycles: Option<u32>,
    /// Power flowing in or out, watts (always positive; see `status`).
    pub power_w: Option<f64>,
    /// Seconds until empty (discharging) or full (charging).
    pub time_left: Option<u64>,
}

pub fn read(root: &SysRoot, name: &str) -> Info {
    let dir = format!("{CLASS}/{name}");
    let num = |f: &str| root.read_parse::<f64>(format!("{dir}/{f}"));
    let status = root.read_opt(format!("{dir}/status")).unwrap_or_default();
    // Amps and volts when the driver reports charge (µAh), watts when it reports energy (µWh).
    let (now, full, full_design, rate) = match (num("charge_now"), num("charge_full")) {
        (Some(n), Some(f)) => (n, f, num("charge_full_design"), num("current_now")),
        _ => (
            num("energy_now").unwrap_or(0.0),
            num("energy_full").unwrap_or(0.0),
            num("energy_full_design"),
            num("power_now"),
        ),
    };
    let power_w = num("power_now")
        .map(|p| p / 1e6)
        .or_else(|| Some(num("current_now")? * num("voltage_now")? / 1e12))
        .map(f64::abs);
    let rate = rate.map(f64::abs).filter(|r| *r > 0.0);
    let time_left = rate.and_then(|r| {
        let hours = match status.as_str() {
            "Discharging" => now / r,
            "Charging" => (full - now).max(0.0) / r,
            _ => return None,
        };
        Some((hours * 3600.0) as u64)
    });
    Info {
        percent: root.read_parse(format!("{dir}/capacity")),
        limit: root.read_parse(format!("{dir}/charge_control_end_threshold")),
        health: full_design
            .filter(|d| *d > 0.0 && full > 0.0)
            .map(|d| full * 100.0 / d),
        cycles: root.read_parse(format!("{dir}/cycle_count")),
        power_w,
        time_left,
        status,
    }
}

/// Set the charge limit if the firmware honours it. Values the firmware would round are
/// refused with the list of what it does accept.
pub fn set_limit(root: &SysRoot, name: &str, q: &Quirks, value: u8) -> Result<(), String> {
    if !root.exists(format!("{CLASS}/{name}/charge_control_end_threshold")) {
        return Err("no charge limit on this battery".into());
    }
    if !q.charge.allows(value) {
        let list = q.charge.values();
        return Err(if list.len() > 8 {
            format!("limit must be {}-100", list[0])
        } else {
            let s: Vec<String> = list.iter().map(u8::to_string).collect();
            format!("limit must be one of {}", s.join(", "))
        });
    }
    // Skip the EC write when nothing changes.
    if root.read_parse::<u8>(format!("{CLASS}/{name}/charge_control_end_threshold")) == Some(value) {
        return Ok(());
    }
    root.write(
        format!("{CLASS}/{name}/charge_control_end_threshold"),
        &value.to_string(),
    )
    .map_err(|e| format!("write failed: {e}"))
}

/// "Charge to 100% once" ends when the battery is full or the charger is unplugged.
pub fn once_finished(status: &str, percent: Option<u8>, on_battery: bool) -> bool {
    on_battery || status == "Full" || percent == Some(100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asus::{quirks, testfix::Fix};

    #[test]
    fn reads_the_fixture_battery() {
        let fx = Fix::new("bat-read");
        let root = fx.root();
        let name = find(&root).unwrap();
        assert_eq!(name, "BAT1");
        let i = read(&root, &name);
        assert_eq!(i.limit, Some(100));
        assert!(i.health.unwrap() > 50.0 && i.health.unwrap() <= 100.0);
        assert!(i.percent.is_some());
    }

    #[test]
    fn health_power_and_time() {
        let fx = Fix::new("bat-calc");
        let b = "/sys/class/power_supply/BAT1";
        fx.set(&format!("{b}/status"), "Discharging\n")
            .set(&format!("{b}/charge_now"), "2000000\n")
            .set(&format!("{b}/charge_full"), "4500000\n")
            .set(&format!("{b}/charge_full_design"), "5000000\n")
            .set(&format!("{b}/current_now"), "1000000\n")
            .set(&format!("{b}/voltage_now"), "16000000\n");
        let i = read(&fx.root(), "BAT1");
        assert!((i.health.unwrap() - 90.0).abs() < 1e-9);
        assert!((i.power_w.unwrap() - 16.0).abs() < 1e-9);
        assert_eq!(i.time_left, Some(2 * 3600));
        // Charging: time to full.
        fx.set(&format!("{b}/status"), "Charging\n");
        assert_eq!(read(&fx.root(), "BAT1").time_left, Some(2500000 * 3600 / 1000000));
        // Full: no estimate.
        fx.set(&format!("{b}/status"), "Full\n");
        assert_eq!(read(&fx.root(), "BAT1").time_left, None);
    }

    #[test]
    fn limit_follows_the_model_quirk() {
        let fx = Fix::new("bat-limit");
        let root = fx.root();
        let q = quirks::detect(&root);
        let f = "/sys/class/power_supply/BAT1/charge_control_end_threshold";
        set_limit(&root, "BAT1", &q, 80).unwrap();
        assert_eq!(fx.get(f), "80");
        let e = set_limit(&root, "BAT1", &q, 85).unwrap_err();
        assert_eq!(e, "limit must be one of 60, 65, 70, 75, 80, 100");
        assert_eq!(fx.get(f), "80");
        set_limit(&root, "BAT1", &q, 100).unwrap();
        assert_eq!(fx.get(f), "100");
        let generic = quirks::for_board("X1");
        assert_eq!(
            set_limit(&root, "BAT1", &generic, 20).unwrap_err(),
            "limit must be 40-100"
        );
        set_limit(&root, "BAT1", &generic, 85).unwrap();
    }

    #[test]
    fn once_ends_on_full_or_unplug() {
        assert!(!once_finished("Charging", Some(60), false));
        assert!(once_finished("Charging", Some(60), true));
        assert!(once_finished("Full", Some(99), false));
        assert!(once_finished("Charging", Some(100), false));
    }
}
