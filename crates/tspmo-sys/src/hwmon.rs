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
            // An NVIDIA GPU's sensors wake it when read.
            if crate::pci::entry_is_nvidia(root, &dir) {
                return None;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::FakeRoot;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_dgpus_sensors_are_not_read() {
        let f = FakeRoot::new("hwmon-nv");
        f.file("/sys/bus/pci/devices/0000:01:00.0/class", "0x030200\n")
            .file("/sys/bus/pci/devices/0000:01:00.0/vendor", "0x10de\n")
            .file("/sys/class/hwmon/hwmon4/name", "coretemp\n")
            .file("/sys/class/hwmon/hwmon7/name", "nouveau\n");
        let p = f.path().join("sys/class/hwmon/hwmon7/device");
        std::os::unix::fs::symlink("../../../bus/pci/devices/0000:01:00.0", p).unwrap();
        // A read of the dGPU's `name` would fail.
        let name = f.path().join("sys/class/hwmon/hwmon7/name");
        std::fs::set_permissions(name, std::fs::Permissions::from_mode(0o000)).unwrap();
        let names: Vec<String> = list(&f.root()).into_iter().map(|h| h.name).collect();
        assert_eq!(names, ["coretemp"]);
    }
}
