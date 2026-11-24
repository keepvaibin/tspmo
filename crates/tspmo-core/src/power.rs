//! Power profiles: what `power-saver`, `balanced` and `performance` actually do to the hardware.
//!
//! [`plan`] is a pure function of (machine capabilities, profile, power source) → a list of
//! sysfs writes, so the tuning is unit-tested against fixtures. [`apply`] performs the writes,
//! skipping any attribute that already has the target value: firmware writes are not free
//! (on ASUS, every `platform_profile` write makes the EC notify the NVIDIA driver, waking the
//! dGPU for a few seconds).

use tspmo_sys::SysRoot;

pub const PLATFORM_PROFILE: &str = "/sys/firmware/acpi/platform_profile";
const CPU: &str = "/sys/devices/system/cpu";

/// The three profiles of the freedesktop power-profiles API (shared with KDE/GNOME).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    PowerSaver,
    Balanced,
    Performance,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::PowerSaver, Profile::Balanced, Profile::Performance];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PowerSaver => "power-saver",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "power-saver" | "silent" | "quiet" | "saver" => Some(Self::PowerSaver),
            "balanced" => Some(Self::Balanced),
            "performance" | "turbo" => Some(Self::Performance),
            _ => None,
        }
    }

    /// Map a kernel `platform_profile` value back to a profile (for hotkey changes like Fn+F5 / Fn+Q).
    pub fn from_platform(s: &str) -> Option<Self> {
        match s.trim() {
            "low-power" | "quiet" | "cool" => Some(Self::PowerSaver),
            "balanced" | "balanced-performance" => Some(Self::Balanced),
            "performance" | "max-power" => Some(Self::Performance),
            _ => None, // `custom` and unknown values: keep the current profile
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::PowerSaver => Self::Balanced,
            Self::Balanced => Self::Performance,
            Self::Performance => Self::PowerSaver,
        }
    }
}

/// Knobs a user can change; defaults are the battery-first choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunables {
    /// Disable CPU turbo in power-saver while on battery.
    pub saver_no_turbo_on_battery: bool,
    /// PCIe ASPM `powersupersave` on battery.
    pub aspm_on_battery: bool,
    /// Audio codec power-down after 1 s idle on battery.
    pub audio_power_save_on_battery: bool,
    /// Flush dirty pages every 15 s instead of 5 s on battery.
    pub lazy_writeback_on_battery: bool,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            saver_no_turbo_on_battery: true,
            aspm_on_battery: true,
            audio_power_save_on_battery: true,
            lazy_writeback_on_battery: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub path: String,
    pub value: String,
    pub what: &'static str,
}

fn w(path: impl Into<String>, value: impl Into<String>, what: &'static str) -> Write {
    Write {
        path: path.into(),
        value: value.into(),
        what,
    }
}

/// The sysfs writes that put the machine into `profile` for the given power source.
pub fn plan(root: &SysRoot, profile: Profile, on_battery: bool, t: &Tunables) -> Vec<Write> {
    let mut out = Vec::new();

    // 1. Firmware/EC thermal mode (fans, power limits, ASUS/Lenovo mode LEDs).
    if let Some(choices) = root.read_opt(format!("{PLATFORM_PROFILE}_choices")) {
        let choices: Vec<&str> = choices.split_whitespace().collect();
        let wanted: &[&str] = match profile {
            Profile::PowerSaver => &["quiet", "low-power", "cool"],
            Profile::Balanced => &["balanced"],
            Profile::Performance => &["performance", "balanced-performance"],
        };
        if let Some(v) = wanted.iter().find(|v| choices.contains(v)) {
            out.push(w(PLATFORM_PROFILE, *v, "firmware thermal mode"));
        }
    }

    // 2. CPU energy/performance preference (intel_pstate and amd-pstate active mode).
    let epp = match (profile, on_battery) {
        (Profile::PowerSaver, _) => "power",
        (Profile::Balanced, true) => "balance_power",
        (Profile::Balanced, false) => "balance_performance",
        (Profile::Performance, _) => "performance",
    };
    for policy in root.glob(&format!("{CPU}/cpufreq/policy*")) {
        let avail = root
            .read_opt(format!("{policy}/energy_performance_available_preferences"))
            .unwrap_or_default();
        if avail.split_whitespace().any(|v| v == epp) {
            out.push(w(
                format!("{policy}/energy_performance_preference"),
                epp,
                "CPU energy preference",
            ));
        }
    }

    // 3. Intel energy-performance bias (affects uncore/package decisions besides HWP).
    let epb = match (profile, on_battery) {
        (Profile::PowerSaver, _) => "15",
        (Profile::Balanced, true) => "8",
        (Profile::Balanced, false) => "6",
        (Profile::Performance, _) => "0",
    };
    for p in root.glob(&format!("{CPU}/cpu*/power/energy_perf_bias")) {
        out.push(w(p, epb, "CPU energy bias"));
    }

    // 4. Turbo.
    let turbo = !(profile == Profile::PowerSaver && on_battery && t.saver_no_turbo_on_battery);
    if root.exists(format!("{CPU}/intel_pstate/no_turbo")) {
        out.push(w(
            format!("{CPU}/intel_pstate/no_turbo"),
            if turbo { "0" } else { "1" },
            "CPU turbo",
        ));
    } else if root.exists(format!("{CPU}/cpufreq/boost")) {
        out.push(w(
            format!("{CPU}/cpufreq/boost"),
            if turbo { "1" } else { "0" },
            "CPU turbo",
        ));
    }

    // 5. Power-source tunables (independent of the profile).
    let aspm = "/sys/module/pcie_aspm/parameters/policy";
    if t.aspm_on_battery && root.read_opt(aspm).is_some_and(|v| v.contains("powersupersave")) {
        out.push(w(
            aspm,
            if on_battery { "powersupersave" } else { "default" },
            "PCIe link power",
        ));
    }
    let hda = "/sys/module/snd_hda_intel/parameters/power_save";
    if t.audio_power_save_on_battery && root.exists(hda) {
        out.push(w(
            hda,
            if on_battery { "1" } else { "10" },
            "audio codec power-down (s)",
        ));
    }
    let wb = "/proc/sys/vm/dirty_writeback_centisecs";
    if t.lazy_writeback_on_battery && root.exists(wb) {
        out.push(w(
            wb,
            if on_battery { "1500" } else { "500" },
            "disk writeback interval",
        ));
    }

    out
}

/// Normalise a sysfs read for comparison: `[default] performance` → `default`.
fn current_value(raw: &str) -> &str {
    if let (Some(s), Some(e)) = (raw.find('['), raw.find(']')) {
        if s < e {
            return &raw[s + 1..e];
        }
    }
    raw.trim()
}

#[derive(Debug, Default)]
pub struct ApplyReport {
    pub changed: Vec<Write>,
    pub unchanged: usize,
    pub failed: Vec<(Write, String)>,
}

/// Perform the writes, skipping attributes that already hold the target value.
pub fn apply(root: &SysRoot, writes: &[Write]) -> ApplyReport {
    let mut r = ApplyReport::default();
    for wr in writes {
        if root
            .read_opt(&wr.path)
            .is_some_and(|cur| current_value(&cur) == wr.value)
        {
            r.unchanged += 1;
            continue;
        }
        match root.write(&wr.path, &wr.value) {
            Ok(()) => r.changed.push(wr.clone()),
            Err(e) => r.failed.push((wr.clone(), e.to_string())),
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(std::path::PathBuf);
    impl Fake {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("tspmo-power-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            Self(d)
        }
        fn file(&self, p: &str, c: &str) -> &Self {
            let p = self.0.join(p.trim_start_matches('/'));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
            self
        }
        fn root(&self) -> SysRoot {
            SysRoot::new(&self.0)
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn gu605(tag: &str) -> Fake {
        let f = Fake::new(tag);
        f.file(PLATFORM_PROFILE, "balanced\n")
            .file(
                &format!("{PLATFORM_PROFILE}_choices"),
                "quiet balanced performance\n",
            )
            .file(
                "/sys/devices/system/cpu/cpufreq/policy0/energy_performance_available_preferences",
                "default performance balance_performance balance_power power \n",
            )
            .file(
                "/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference",
                "balance_performance\n",
            )
            .file("/sys/devices/system/cpu/cpu0/power/energy_perf_bias", "6\n")
            .file("/sys/devices/system/cpu/intel_pstate/no_turbo", "0\n")
            .file(
                "/sys/module/pcie_aspm/parameters/policy",
                "[default] performance powersave powersupersave \n",
            )
            .file("/sys/module/snd_hda_intel/parameters/power_save", "10\n")
            .file("/proc/sys/vm/dirty_writeback_centisecs", "500\n");
        f
    }

    fn get<'a>(p: &'a [Write], what: &str) -> Vec<&'a str> {
        p.iter()
            .filter(|w| w.what == what)
            .map(|w| w.value.as_str())
            .collect()
    }

    #[test]
    fn power_saver_on_battery_is_frugal() {
        let f = gu605("saver");
        let p = plan(&f.root(), Profile::PowerSaver, true, &Tunables::default());
        assert_eq!(get(&p, "firmware thermal mode"), ["quiet"]);
        assert_eq!(get(&p, "CPU energy preference"), ["power"]);
        assert_eq!(get(&p, "CPU energy bias"), ["15"]);
        assert_eq!(get(&p, "CPU turbo"), ["1"]); // no_turbo=1
        assert_eq!(get(&p, "PCIe link power"), ["powersupersave"]);
        assert_eq!(get(&p, "audio codec power-down (s)"), ["1"]);
    }

    #[test]
    fn performance_on_ac() {
        let f = gu605("perf");
        let p = plan(&f.root(), Profile::Performance, false, &Tunables::default());
        assert_eq!(get(&p, "firmware thermal mode"), ["performance"]);
        assert_eq!(get(&p, "CPU energy preference"), ["performance"]);
        assert_eq!(get(&p, "CPU turbo"), ["0"]);
        assert_eq!(get(&p, "PCIe link power"), ["default"]);
    }

    #[test]
    fn apply_skips_unchanged_and_reads_bracketed() {
        let f = gu605("apply");
        let root = f.root();
        let p = plan(&root, Profile::Balanced, false, &Tunables::default());
        let r = apply(&root, &p);
        // Already balanced / balance_performance / 6 / turbo on / [default] / 10 / 500.
        assert!(r.changed.is_empty(), "{:?}", r.changed);
        assert!(r.failed.is_empty());
        let r = apply(
            &root,
            &plan(&root, Profile::PowerSaver, true, &Tunables::default()),
        );
        assert_eq!(r.changed.len(), 7);
        assert_eq!(root.read(PLATFORM_PROFILE).unwrap(), "quiet");
    }

    #[test]
    fn hotkey_mapping() {
        assert_eq!(Profile::from_platform("low-power"), Some(Profile::PowerSaver));
        assert_eq!(Profile::from_platform("max-power"), Some(Profile::Performance));
        assert_eq!(Profile::from_platform("custom"), None);
    }
}
