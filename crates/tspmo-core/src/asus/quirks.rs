//! Per-model facts the kernel doesn't describe. Keyed by DMI board name prefix, so one entry
//! covers a whole family (`GU605M` matches `GU605MI`, `GU605MU`, ...). Everything else is
//! discovered from sysfs.

use tspmo_sys::{SysRoot, dmi::Dmi};

/// Battery charge limits the firmware honours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeSteps {
    /// Any whole percentage in `min..=100`.
    Range { min: u8 },
    /// Only these values (the firmware rounds anything else, silently).
    List(&'static [u8]),
}

impl ChargeSteps {
    pub fn values(&self) -> Vec<u8> {
        match self {
            Self::Range { min } => (*min..=100).collect(),
            Self::List(l) => l.to_vec(),
        }
    }

    pub fn allows(&self, v: u8) -> bool {
        match self {
            Self::Range { min } => (*min..=100).contains(&v),
            Self::List(l) => l.contains(&v),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quirks {
    pub charge: ChargeSteps,
    /// Power-limit writes are refused until a custom fan curve is active (the kernel returns
    /// EBUSY). This is the `IsFanRequired` / `IsPowerRequired` coupling other tools describe.
    pub power_needs_fan_curve: bool,
}

const DEFAULT: Quirks = Quirks {
    charge: ChargeSteps::Range { min: 40 },
    power_needs_fan_curve: false,
};

/// `(board name prefix, quirks)`; the longest matching prefix wins.
const TABLE: &[(&str, Quirks)] = &[
    (
        "GU605M",
        Quirks {
            charge: ChargeSteps::List(&[60, 65, 70, 75, 80, 100]),
            power_needs_fan_curve: true,
        },
    ),
    (
        "GU606",
        Quirks {
            charge: ChargeSteps::List(&[60, 65, 70, 75, 80, 100]),
            power_needs_fan_curve: true,
        },
    ),
    (
        "GA605",
        Quirks {
            charge: ChargeSteps::Range { min: 40 },
            power_needs_fan_curve: true,
        },
    ),
];

pub fn for_board(board_name: &str) -> Quirks {
    TABLE
        .iter()
        .filter(|(p, _)| board_name.starts_with(p))
        .max_by_key(|(p, _)| p.len())
        .map_or(DEFAULT, |(_, q)| *q)
}

pub fn detect(root: &SysRoot) -> Quirks {
    for_board(&Dmi::read(root).board_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gu605_family() {
        let q = for_board("GU605MI");
        assert_eq!(q.charge.values(), vec![60, 65, 70, 75, 80, 100]);
        assert!(q.power_needs_fan_curve);
        assert!(q.charge.allows(80));
        assert!(!q.charge.allows(85));
    }

    #[test]
    fn unknown_board_is_generic() {
        let q = for_board("X123");
        assert!(!q.power_needs_fan_curve);
        assert!(q.charge.allows(41) && !q.charge.allows(39));
        assert_eq!(q.charge.values().len(), 61);
    }

    #[test]
    fn longest_prefix_wins() {
        assert_eq!(for_board("GU606CW"), for_board("GU606"));
    }
}
