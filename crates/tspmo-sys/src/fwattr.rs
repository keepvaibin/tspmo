//! The kernel `firmware-attributes` class: `/sys/class/firmware-attributes/<driver>/attributes/<name>/`.
//!
//! Used by `asus-armoury` (ASUS), `lenovo-wmi-other` (Legion/LOQ) and `thinklmi` (ThinkPad BIOS).
//! Each attribute describes itself (type, bounds, allowed values), so tspmo discovers what a
//! machine supports instead of hard-coding model lists.

use crate::SysRoot;
use std::fmt;

pub const CLASS: &str = "/sys/class/firmware-attributes";

/// Firmware-attribute drivers tspmo knows how to drive.
pub const KNOWN_DRIVERS: &[&str] = &["asus-armoury", "lenovo-wmi-other", "thinklmi"];

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Integer { min: i64, max: i64, step: i64 },
    Enumeration { values: Vec<String> },
    String,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub driver: String,
    pub name: String,
    pub display_name: Option<String>,
    pub kind: Kind,
    pub current: Option<String>,
    pub default: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SetError {
    OutOfRange { min: i64, max: i64 },
    BadStep { step: i64 },
    NotAnInteger,
    NotAllowed { allowed: Vec<String> },
    Io(String),
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange { min, max } => write!(f, "value must be between {min} and {max}"),
            Self::BadStep { step } => write!(f, "value must be a multiple of {step}"),
            Self::NotAnInteger => f.write_str("value must be an integer"),
            Self::NotAllowed { allowed } => write!(f, "value must be one of: {}", allowed.join(", ")),
            Self::Io(e) => write!(f, "write failed: {e}"),
        }
    }
}

impl std::error::Error for SetError {}

fn attr_dir(driver: &str, name: &str) -> String {
    format!("{CLASS}/{driver}/attributes/{name}")
}

/// Drivers present on this machine.
pub fn drivers(root: &SysRoot) -> Vec<String> {
    root.list(CLASS)
}

/// All attributes a driver exposes, with their self-description.
pub fn enumerate(root: &SysRoot, driver: &str) -> Vec<Attribute> {
    let base = format!("{CLASS}/{driver}/attributes");
    root.list(&base)
        .into_iter()
        // `pending_reboot` (and similar) are plain files next to the attribute directories.
        .filter(|name| root.exists(format!("{base}/{name}/current_value")))
        .filter_map(|name| read(root, driver, &name))
        .collect()
}

/// Read one attribute's description and current value.
pub fn read(root: &SysRoot, driver: &str, name: &str) -> Option<Attribute> {
    let dir = attr_dir(driver, name);
    if !root.exists(&dir) {
        return None;
    }
    let get = |f: &str| root.read_opt(format!("{dir}/{f}"));
    let kind = match get("type").as_deref() {
        Some("integer") => Kind::Integer {
            min: get("min_value").and_then(|v| v.parse().ok()).unwrap_or(i64::MIN),
            max: get("max_value").and_then(|v| v.parse().ok()).unwrap_or(i64::MAX),
            step: get("scalar_increment")
                .and_then(|v| v.parse().ok())
                .filter(|s: &i64| *s > 0)
                .unwrap_or(1),
        },
        Some("enumeration") => Kind::Enumeration {
            values: get("possible_values")
                .map(|v| split_values(&v))
                .unwrap_or_default(),
        },
        Some("string") => Kind::String,
        Some(other) => Kind::Other(other.to_owned()),
        // Some drivers omit `type` (seen on older think-lmi); infer from bounds.
        None if get("min_value").is_some() => Kind::Integer {
            min: get("min_value").and_then(|v| v.parse().ok()).unwrap_or(i64::MIN),
            max: get("max_value").and_then(|v| v.parse().ok()).unwrap_or(i64::MAX),
            step: 1,
        },
        None => Kind::Other(String::new()),
    };
    Some(Attribute {
        driver: driver.to_owned(),
        name: name.to_owned(),
        display_name: get("display_name"),
        kind,
        current: get("current_value"),
        default: get("default_value"),
    })
}

/// `possible_values` is `;`-separated (some drivers use `,`).
fn split_values(raw: &str) -> Vec<String> {
    raw.split([';', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

impl Attribute {
    /// Check a value against what the kernel says the attribute accepts.
    pub fn validate(&self, value: &str) -> Result<(), SetError> {
        match &self.kind {
            Kind::Integer { min, max, step } => {
                let v: i64 = value.trim().parse().map_err(|_| SetError::NotAnInteger)?;
                if v < *min || v > *max {
                    return Err(SetError::OutOfRange { min: *min, max: *max });
                }
                let base = if *min == i64::MIN { 0 } else { *min };
                if (v - base) % step != 0 {
                    return Err(SetError::BadStep { step: *step });
                }
                Ok(())
            }
            Kind::Enumeration { values } if !values.is_empty() => {
                if values.iter().any(|a| a == value.trim()) {
                    Ok(())
                } else {
                    Err(SetError::NotAllowed {
                        allowed: values.clone(),
                    })
                }
            }
            _ => Ok(()),
        }
    }

    /// Validate, then write `current_value`.
    pub fn set(&self, root: &SysRoot, value: &str) -> Result<(), SetError> {
        self.validate(value)?;
        root.write(
            format!("{}/current_value", attr_dir(&self.driver, &self.name)),
            value.trim(),
        )
        .map_err(|e| SetError::Io(e.to_string()))
    }
}

/// Whether a firmware change is waiting for a reboot (e.g. a GPU MUX switch).
pub fn pending_reboot(root: &SysRoot, driver: &str) -> bool {
    root.read_parse::<u8>(format!("{CLASS}/{driver}/attributes/pending_reboot"))
        .is_some_and(|v| v != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::FakeRoot;

    fn fake() -> FakeRoot {
        let f = FakeRoot::new("fwattr");
        let a = "/sys/class/firmware-attributes/asus-armoury/attributes";
        f.file(&format!("{a}/pending_reboot"), "0\n")
            .file(&format!("{a}/ppt_pl1_spl/type"), "integer\n")
            .file(&format!("{a}/ppt_pl1_spl/current_value"), "80\n")
            .file(&format!("{a}/ppt_pl1_spl/default_value"), "80\n")
            .file(&format!("{a}/ppt_pl1_spl/min_value"), "5\n")
            .file(&format!("{a}/ppt_pl1_spl/max_value"), "90\n")
            .file(&format!("{a}/ppt_pl1_spl/scalar_increment"), "1\n")
            .file(&format!("{a}/gpu_mux_mode/type"), "enumeration\n")
            .file(&format!("{a}/gpu_mux_mode/current_value"), "1\n")
            .file(&format!("{a}/gpu_mux_mode/possible_values"), "0;1\n");
        f
    }

    #[test]
    fn enumerates_and_validates() {
        let f = fake();
        let root = f.root();
        assert_eq!(drivers(&root), vec!["asus-armoury"]);
        let attrs = enumerate(&root, "asus-armoury");
        let names: Vec<_> = attrs.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["gpu_mux_mode", "ppt_pl1_spl"]);

        let pl1 = read(&root, "asus-armoury", "ppt_pl1_spl").unwrap();
        assert_eq!(
            pl1.kind,
            Kind::Integer {
                min: 5,
                max: 90,
                step: 1
            }
        );
        assert!(pl1.validate("45").is_ok());
        assert_eq!(pl1.validate("4"), Err(SetError::OutOfRange { min: 5, max: 90 }));
        assert_eq!(pl1.validate("x"), Err(SetError::NotAnInteger));

        let mux = read(&root, "asus-armoury", "gpu_mux_mode").unwrap();
        assert!(mux.validate("0").is_ok());
        assert!(matches!(mux.validate("2"), Err(SetError::NotAllowed { .. })));

        pl1.set(&root, "60").unwrap();
        assert_eq!(
            read(&root, "asus-armoury", "ppt_pl1_spl")
                .unwrap()
                .current
                .as_deref(),
            Some("60")
        );
        assert!(!pending_reboot(&root, "asus-armoury"));
    }
}
