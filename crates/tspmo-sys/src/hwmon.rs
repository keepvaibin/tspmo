//! `/sys/class/hwmon` devices, found by their `name` (never by number, which changes per boot).

use crate::SysRoot;

pub const CLASS: &str = "/sys/class/hwmon";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hwmon {
    /// Logical directory, e.g. `/sys/class/hwmon/hwmon7`.
    pub dir: String,
    pub name: String,
}

pub fn list(root: &SysRoot) -> Vec<Hwmon> {
    root.list(CLASS)
        .into_iter()
        .filter_map(|n| {
            let dir = format!("{CLASS}/{n}");
            let name = root.read_opt(format!("{dir}/name"))?;
            Some(Hwmon { dir, name })
        })
        .collect()
}

pub fn find(root: &SysRoot, name: &str) -> Option<Hwmon> {
    list(root).into_iter().find(|h| h.name == name)
}

impl Hwmon {
    /// Indices `N` for which `<prefix>N_<suffix>` exists, e.g. `("fan", "input")` → fans.
    pub fn indices(&self, root: &SysRoot, prefix: &str, suffix: &str) -> Vec<u32> {
        let mut v: Vec<u32> = root
            .list(&self.dir)
            .iter()
            .filter_map(|f| {
                f.strip_prefix(prefix)?
                    .strip_suffix(suffix)?
                    .strip_suffix('_')?
                    .parse()
                    .ok()
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}
