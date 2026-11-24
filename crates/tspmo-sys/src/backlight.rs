//! `/sys/class/backlight` devices.

use crate::SysRoot;

pub const CLASS: &str = "/sys/class/backlight";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlight {
    pub name: String,
    /// `raw`, `platform` or `firmware`.
    pub kind: String,
    pub brightness: u32,
    pub max: u32,
    pub actual: Option<u32>,
}

impl Backlight {
    pub fn percent(&self) -> f32 {
        if self.max == 0 {
            0.0
        } else {
            self.brightness as f32 * 100.0 / self.max as f32
        }
    }

    /// Backlights registered by the NVIDIA driver (`nvidia_0`, `nvidia_wmi_ec_backlight`).
    pub fn is_nvidia(&self) -> bool {
        self.name.starts_with("nvidia")
    }
}

pub fn list(root: &SysRoot) -> Vec<Backlight> {
    root.list(CLASS)
        .into_iter()
        .filter_map(|name| {
            let dir = format!("{CLASS}/{name}");
            Some(Backlight {
                kind: root.read_opt(format!("{dir}/type")).unwrap_or_default(),
                brightness: root.read_parse(format!("{dir}/brightness"))?,
                max: root.read_parse(format!("{dir}/max_brightness"))?,
                actual: root.read_parse(format!("{dir}/actual_brightness")),
                name,
            })
        })
        .collect()
}
