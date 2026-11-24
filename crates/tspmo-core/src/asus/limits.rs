//! Power limits through `asus-armoury` (firmware attributes). The kernel describes each limit
//! itself (min, max, default), and the bounds differ between AC and battery, so they are read
//! fresh every time and never cached.

use tspmo_sys::{
    SysRoot,
    fwattr::{self, Attribute, Kind, SetError},
};

pub const DRIVER: &str = "asus-armoury";

/// `(attribute, label, unit)`, in display order. Whichever of these the kernel exposes.
pub const KNOWN: &[(&str, &str, &str)] = &[
    ("ppt_pl1_spl", "CPU sustained", "W"),
    ("ppt_pl2_sppt", "CPU boost", "W"),
    ("ppt_pl3_fppt", "CPU burst", "W"),
    ("ppt_apu_sppt", "APU boost", "W"),
    ("ppt_platform_sppt", "Platform", "W"),
    ("nv_dynamic_boost", "GPU boost", "W"),
    ("nv_temp_target", "GPU temperature", "°C"),
    ("nv_tgp", "GPU power", "W"),
];

pub fn is_limit(name: &str) -> bool {
    KNOWN.iter().any(|(n, _, _)| *n == name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    pub name: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub min: i64,
    pub max: i64,
    /// The firmware default (`default_value`, else the maximum).
    pub default: i64,
    pub current: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitError {
    Unknown,
    Invalid(String),
    /// The firmware refuses power-limit writes until a custom fan curve is active (EBUSY).
    NeedsFanCurve,
    Io(String),
}

impl std::fmt::Display for LimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("unknown limit"),
            Self::Invalid(e) => f.write_str(e),
            Self::NeedsFanCurve => f.write_str("custom fan curve required"),
            Self::Io(e) => write!(f, "write failed: {e}"),
        }
    }
}

impl std::error::Error for LimitError {}

fn limit_of(a: &Attribute) -> Option<Limit> {
    let &(name, label, unit) = KNOWN.iter().find(|(n, _, _)| *n == a.name)?;
    let Kind::Integer { min, max, .. } = a.kind else {
        return None;
    };
    let current = a.current.as_deref()?.trim().parse().ok()?;
    let default = a
        .default
        .as_deref()
        .and_then(|d| d.trim().parse().ok())
        .unwrap_or(max);
    Some(Limit {
        name,
        label,
        unit,
        min,
        max,
        default,
        current,
    })
}

/// The limits this machine exposes, with bounds valid for the current power source.
pub fn discover(root: &SysRoot) -> Vec<Limit> {
    KNOWN
        .iter()
        .filter_map(|(n, _, _)| limit_of(&fwattr::read(root, DRIVER, n)?))
        .collect()
}

pub fn get(root: &SysRoot, name: &str) -> Option<Limit> {
    limit_of(&fwattr::read(root, DRIVER, name)?)
}

impl Limit {
    /// Whether the firmware lets this limit move right now (some are fixed on battery).
    pub fn adjustable(&self) -> bool {
        self.max > self.min
    }

    pub fn clamp(&self, v: i64) -> i64 {
        v.clamp(self.min, self.max)
    }
}

/// Write one limit. The value must be within the bounds the kernel reports right now.
pub fn set(root: &SysRoot, name: &str, value: i64) -> Result<(), LimitError> {
    if !is_limit(name) {
        return Err(LimitError::Unknown);
    }
    let attr = fwattr::read(root, DRIVER, name).ok_or(LimitError::Unknown)?;
    attr.validate(&value.to_string()).map_err(|e| match e {
        SetError::OutOfRange { min, max } => LimitError::Invalid(format!("{name}: must be {min}-{max}")),
        other => LimitError::Invalid(other.to_string()),
    })?;
    root.write(
        format!("{}/{DRIVER}/attributes/{name}/current_value", fwattr::CLASS),
        &value.to_string(),
    )
    .map_err(|e| {
        if e.raw_os_error() == Some(16) {
            LimitError::NeedsFanCurve
        } else {
            LimitError::Io(e.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asus::testfix::Fix;

    #[test]
    fn discovers_what_the_kernel_exposes() {
        // The fixture was captured on battery: narrow bounds, GPU boost fixed.
        let fx = Fix::new("limits-discover");
        let l = discover(&fx.root());
        let names: Vec<_> = l.iter().map(|l| l.name).collect();
        assert_eq!(
            names,
            vec![
                "ppt_pl1_spl",
                "ppt_pl2_sppt",
                "nv_dynamic_boost",
                "nv_temp_target"
            ]
        );
        let pl1 = &l[0];
        assert_eq!((pl1.min, pl1.max, pl1.default, pl1.current), (25, 35, 35, 35));
        assert_eq!(pl1.unit, "W");
        assert_eq!(pl1.clamp(500), 35);
        assert_eq!(pl1.clamp(1), 25);
        assert!(pl1.adjustable());
        assert!(!l[2].adjustable());
    }

    #[test]
    fn set_validates_against_the_kernels_bounds() {
        let fx = Fix::new("limits-set");
        let root = fx.root();
        set(&root, "ppt_pl1_spl", 30).unwrap();
        assert_eq!(get(&root, "ppt_pl1_spl").unwrap().current, 30);
        assert!(matches!(
            set(&root, "ppt_pl1_spl", 24),
            Err(LimitError::Invalid(_))
        ));
        assert!(matches!(
            set(&root, "ppt_pl1_spl", 36),
            Err(LimitError::Invalid(_))
        ));
        assert_eq!(set(&root, "gpu_mux_mode", 0), Err(LimitError::Unknown));
        assert_eq!(set(&root, "nope", 1), Err(LimitError::Unknown));

        // On AC the kernel reports wider bounds: the same value is now fine.
        let a = "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl";
        fx.set(&format!("{a}/max_value"), "90\n");
        set(&root, "ppt_pl1_spl", 45).unwrap();
    }

    #[test]
    fn missing_driver_means_no_limits() {
        let fx = Fix::new("limits-none");
        std::fs::remove_dir_all(fx.root().path("/sys/class/firmware-attributes")).unwrap();
        assert!(discover(&fx.root()).is_empty());
    }
}
