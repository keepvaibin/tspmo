//! Firmware toggles: whatever `asus-armoury` exposes besides power limits and GPU modes,
//! driven entirely by each attribute's own description (type, allowed values, writability).

use super::limits;
use std::os::unix::fs::PermissionsExt;
use tspmo_sys::{
    SysRoot,
    fwattr::{self, Attribute, Kind},
};

pub const DRIVER: &str = limits::DRIVER;

/// Attributes other parts of tspmo own: GPU modes (they need live switching and a reboot
/// flow) and the power limits.
fn excluded(name: &str) -> bool {
    limits::is_limit(name) || matches!(name, "gpu_mux_mode" | "dgpu_disable" | "egpu_enable")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind2 {
    /// `0;1`
    Toggle,
    /// Any other enumeration.
    Choice,
    Number,
    Text,
}

impl Kind2 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Choice => "choice",
            Self::Number => "number",
            Self::Text => "text",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub label: String,
    pub kind: Kind2,
    pub current: String,
    /// Allowed values of a toggle or choice.
    pub values: Vec<String>,
    pub min: i64,
    pub max: i64,
    pub writable: bool,
}

fn label(name: &str) -> String {
    match name {
        "panel_overdrive" => "Panel overdrive".into(),
        "boot_sound" => "Boot sound".into(),
        "mini_led_mode" => "Mini-LED".into(),
        "charge_mode" => "Charger".into(),
        "mcu_powersave" => "MCU power save".into(),
        "panel_hd_mode" => "Panel HD mode".into(),
        "screen_auto_brightness" => "Auto brightness".into(),
        "apu_mem" => "APU memory".into(),
        "egpu_connected" => "eGPU connected".into(),
        "nv_base_tgp" => "GPU base power".into(),
        other => {
            let s = other.replace('_', " ");
            let mut c = s.chars();
            c.next()
                .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        }
    }
}

/// A readable name for a value, where the kernel documents one.
pub fn value_label(name: &str, value: &str) -> String {
    match (name, value) {
        ("charge_mode", "0") => "none".into(),
        ("charge_mode", "1") => "barrel".into(),
        ("charge_mode", "2") => "usb-c".into(),
        (_, "0") => "off".into(),
        (_, "1") => "on".into(),
        _ => value.to_owned(),
    }
}

fn item(root: &SysRoot, a: &Attribute) -> Option<Item> {
    let current = a.current.clone()?;
    let path = root.path(format!(
        "{}/{DRIVER}/attributes/{}/current_value",
        fwattr::CLASS,
        a.name
    ));
    let writable = std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o200 != 0);
    let (kind, values, min, max) = match &a.kind {
        Kind::Enumeration { values } => {
            let toggle = values.len() == 2 && values.iter().all(|v| v == "0" || v == "1");
            (
                if toggle { Kind2::Toggle } else { Kind2::Choice },
                values.clone(),
                0,
                0,
            )
        }
        Kind::Integer { min, max, .. } => (Kind2::Number, Vec::new(), *min, *max),
        Kind::String | Kind::Other(_) => (Kind2::Text, Vec::new(), 0, 0),
    };
    Some(Item {
        name: a.name.clone(),
        label: label(&a.name),
        kind,
        current,
        values,
        min,
        max,
        writable,
    })
}

pub fn discover(root: &SysRoot) -> Vec<Item> {
    fwattr::enumerate(root, DRIVER)
        .iter()
        .filter(|a| !excluded(&a.name))
        .filter_map(|a| item(root, a))
        .collect()
}

pub fn get(root: &SysRoot, name: &str) -> Option<Item> {
    if excluded(name) {
        return None;
    }
    item(root, &fwattr::read(root, DRIVER, name)?)
}

/// Set an attribute after checking it against the kernel's own description.
pub fn set(root: &SysRoot, name: &str, value: &str) -> Result<(), String> {
    let it = get(root, name).ok_or_else(|| format!("unknown setting '{name}'"))?;
    if !it.writable {
        return Err(format!("{} is read-only", it.label));
    }
    let attr = fwattr::read(root, DRIVER, name).ok_or_else(|| format!("unknown setting '{name}'"))?;
    if it.current == value.trim() {
        return Ok(());
    }
    attr.set(root, value).map_err(|e| format!("{}: {e}", it.label))
}

/// A change waits for a reboot (some firmware settings only apply then).
pub fn reboot_pending(root: &SysRoot) -> bool {
    fwattr::pending_reboot(root, DRIVER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asus::testfix::Fix;

    const A: &str = "/sys/class/firmware-attributes/asus-armoury/attributes";

    #[test]
    fn fixture_toggles_exclude_power_and_gpu() {
        let fx = Fix::new("hw-discover");
        let items = discover(&fx.root());
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["boot_sound", "charge_mode", "panel_overdrive"]);
        let od = items.iter().find(|i| i.name == "panel_overdrive").unwrap();
        assert_eq!(od.kind, Kind2::Toggle);
        assert_eq!(od.label, "Panel overdrive");
        let cm = items.iter().find(|i| i.name == "charge_mode").unwrap();
        assert_eq!(cm.kind, Kind2::Choice);
        assert_eq!(cm.values, vec!["0", "1", "2"]);
        assert!(get(&fx.root(), "gpu_mux_mode").is_none());
        assert!(get(&fx.root(), "ppt_pl1_spl").is_none());
    }

    #[test]
    fn set_checks_values_and_writability() {
        let fx = Fix::new("hw-set");
        let root = fx.root();
        set(&root, "panel_overdrive", "0").unwrap();
        assert_eq!(fx.get(&format!("{A}/panel_overdrive/current_value")), "0");
        assert!(
            set(&root, "panel_overdrive", "2")
                .unwrap_err()
                .contains("must be one of")
        );
        assert!(set(&root, "gpu_mux_mode", "0").unwrap_err().contains("unknown"));

        // charge_mode is read-only in the kernel.
        let p = root.path(format!("{A}/charge_mode/current_value"));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(set(&root, "charge_mode", "1").unwrap_err().contains("read-only"));
        assert!(!get(&root, "charge_mode").unwrap().writable);
    }

    #[test]
    fn labels_and_values() {
        assert_eq!(label("some_new_thing"), "Some new thing");
        assert_eq!(value_label("charge_mode", "2"), "usb-c");
        assert_eq!(value_label("boot_sound", "1"), "on");
        assert_eq!(value_label("apu_mem", "8"), "8");
    }

    #[test]
    fn no_driver_no_items() {
        let fx = Fix::new("hw-none");
        std::fs::remove_dir_all(fx.root().path("/sys/class/firmware-attributes")).unwrap();
        assert!(discover(&fx.root()).is_empty());
    }
}
