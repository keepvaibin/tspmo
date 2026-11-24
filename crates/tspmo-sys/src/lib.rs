//! Thin, allocation-light helpers for the kernel interfaces tspmo talks to.
//!
//! Every path is *logical* (e.g. `/sys/class/backlight`) and is resolved under a
//! [`SysRoot`]. In production the root is `/`; in tests it points at a captured
//! fixture tree, so every backend can be tested without the hardware.

pub mod backlight;
pub mod dmi;
pub mod fwattr;
pub mod hwmon;
pub mod pci;
pub mod power_supply;
pub mod usb;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Environment variable that relocates the root (used by tests and `tspmo doctor --root`).
pub const SYSROOT_ENV: &str = "TSPMO_SYSROOT";

/// Root that logical kernel paths are resolved against.
#[derive(Debug, Clone)]
pub struct SysRoot {
    root: PathBuf,
}

impl Default for SysRoot {
    fn default() -> Self {
        Self::new("/")
    }
}

impl SysRoot {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `$TSPMO_SYSROOT` if set, otherwise `/`.
    pub fn from_env() -> Self {
        std::env::var_os(SYSROOT_ENV).map_or_else(Self::default, Self::new)
    }

    pub fn is_real(&self) -> bool {
        self.root == Path::new("/")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a logical absolute path to a real one.
    pub fn path(&self, logical: impl AsRef<Path>) -> PathBuf {
        let logical = logical.as_ref();
        let rel = logical.strip_prefix("/").unwrap_or(logical);
        self.root.join(rel)
    }

    pub fn exists(&self, logical: impl AsRef<Path>) -> bool {
        self.path(logical).exists()
    }

    /// Read a sysfs attribute with the trailing newline removed.
    pub fn read(&self, logical: impl AsRef<Path>) -> io::Result<String> {
        let mut s = fs::read_to_string(self.path(logical))?;
        let len = s.trim_end_matches(['\n', '\r', ' ', '\0']).len();
        s.truncate(len);
        Ok(s)
    }

    /// Read and parse an attribute; `None` on any error.
    pub fn read_parse<T: std::str::FromStr>(&self, logical: impl AsRef<Path>) -> Option<T> {
        self.read(logical).ok()?.trim().parse().ok()
    }

    pub fn read_opt(&self, logical: impl AsRef<Path>) -> Option<String> {
        self.read(logical).ok()
    }

    /// Write a value to an attribute (one `write(2)`, as sysfs expects).
    pub fn write(&self, logical: impl AsRef<Path>, value: &str) -> io::Result<()> {
        fs::write(self.path(logical), value.as_bytes())
    }

    /// Names of the entries in a directory, sorted. Empty if it doesn't exist.
    pub fn list(&self, logical: impl AsRef<Path>) -> Vec<String> {
        let mut names: Vec<String> = match fs::read_dir(self.path(logical)) {
            Ok(rd) => rd
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort_unstable();
        names
    }

    /// Final component of a symlink target (e.g. a device's `driver` link).
    pub fn link_name(&self, logical: impl AsRef<Path>) -> Option<String> {
        let target = fs::read_link(self.path(logical)).ok()?;
        Some(target.file_name()?.to_str()?.to_owned())
    }

    /// Expand a logical path pattern where any segment may contain `*` wildcards.
    /// Returns logical paths, sorted.
    pub fn glob(&self, pattern: &str) -> Vec<String> {
        let mut out = vec![String::new()];
        for seg in pattern.split('/').filter(|s| !s.is_empty()) {
            let mut next = Vec::new();
            for base in &out {
                if seg.contains('*') {
                    for name in self.list(if base.is_empty() { "/" } else { base }) {
                        if wildcard_match(seg, &name) {
                            next.push(format!("{base}/{name}"));
                        }
                    }
                } else {
                    let candidate = format!("{base}/{seg}");
                    if self.exists(&candidate) {
                        next.push(candidate);
                    }
                }
            }
            out = next;
            if out.is_empty() {
                break;
            }
        }
        out.sort_unstable();
        out
    }
}

/// `*` matches any run of characters (including none). No other metacharacters.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};

    /// A temporary fake sysfs tree, removed on drop.
    pub struct FakeRoot(pub PathBuf);

    impl FakeRoot {
        pub fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "tspmo-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn file(&self, logical: &str, contents: &str) -> &Self {
            let p = self.0.join(logical.trim_start_matches('/'));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, contents).unwrap();
            self
        }

        pub fn root(&self) -> super::SysRoot {
            super::SysRoot::new(&self.0)
        }

        #[allow(dead_code)]
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for FakeRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::FakeRoot;
    use super::*;

    #[test]
    fn wildcard() {
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("hwmon*", "hwmon3"));
        assert!(wildcard_match("pwm*_auto_point*_pwm", "pwm1_auto_point8_pwm"));
        assert!(!wildcard_match("pwm*_auto_point*_pwm", "pwm1_auto_point8_temp"));
        assert!(wildcard_match("BAT*", "BAT1"));
        assert!(!wildcard_match("BAT*", "ACAD"));
        assert!(wildcard_match("a*a", "aa"));
        assert!(!wildcard_match("a*a", "a"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exactly"));
    }

    #[test]
    fn read_trims_and_globs() {
        let fake = FakeRoot::new("glob");
        fake.file("/sys/class/hwmon/hwmon0/name", "acpitz\n")
            .file("/sys/class/hwmon/hwmon3/name", "asus_custom_fan_curve\n");
        let root = fake.root();
        assert_eq!(
            root.read("/sys/class/hwmon/hwmon3/name").unwrap(),
            "asus_custom_fan_curve"
        );
        assert_eq!(
            root.glob("/sys/class/hwmon/hwmon*/name"),
            vec!["/sys/class/hwmon/hwmon0/name", "/sys/class/hwmon/hwmon3/name"]
        );
        assert!(root.glob("/sys/class/nope/*").is_empty());
    }
}
