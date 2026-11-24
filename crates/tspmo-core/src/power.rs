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

/// The power modes. Three are the freedesktop power-profiles ones (shared with KDE/GNOME);
/// `EcoPlus` is tspmo's own: power-saver plus package/platform power caps, a lower uncore
/// clock and 60 Hz. The power-profiles API sees it as `power-saver` ([`Profile::ppd`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    EcoPlus,
    PowerSaver,
    Balanced,
    Performance,
}

impl Profile {
    /// The profiles of the power-profiles API.
    pub const ALL: [Profile; 3] = [Profile::PowerSaver, Profile::Balanced, Profile::Performance];
    /// Every mode, lowest power first.
    pub const MODES: [Profile; 4] = [
        Profile::EcoPlus,
        Profile::PowerSaver,
        Profile::Balanced,
        Profile::Performance,
    ];

    /// What KDE/GNOME see: Eco+ is a power-saver.
    pub fn ppd(self) -> Self {
        match self {
            Self::EcoPlus => Self::PowerSaver,
            p => p,
        }
    }

    pub fn is_eco_plus(self) -> bool {
        self == Self::EcoPlus
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::EcoPlus => "eco-plus",
            Self::PowerSaver => "power-saver",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "eco-plus" | "eco+" | "ecoplus" => Some(Self::EcoPlus),
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

    /// The next mode in the cycle: eco-plus, power-saver, balanced, performance, eco-plus.
    pub fn next(self) -> Self {
        match self {
            Self::EcoPlus => Self::PowerSaver,
            Self::PowerSaver => Self::Balanced,
            Self::Balanced => Self::Performance,
            Self::Performance => Self::EcoPlus,
        }
    }
}

/// What Eco+ puts on top of power-saver (watts, MHz; `None` leaves that limit alone).
///
/// Eco+ is a sustained-power budget with room for bursts, not a flat cap: the first design
/// (package 5/7 W, no turbo, uncore 1.6 GHz) starved the desktop whenever a build ran, because
/// a power limit slows every core alike. Now a long-term limit (PL1) averaged over a window and
/// a high short-term limit (PL2) bound the draw; the efficiency cores that background work runs
/// on are held to a low clock (see `tspmod`'s guard), which keeps that work cheap and leaves
/// the performance cores' share of the budget to the foreground. Measurements: docs/power.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcoLimits {
    /// CPU package: (long-term PL1, short-term PL2).
    pub pkg_w: Option<(u32, u32)>,
    /// PL1 averaging window.
    pub pkg_window_ms: Option<u32>,
    /// Whole platform (RAPL `psys`): (long-term, short-term).
    pub psys_w: Option<(u32, u32)>,
    /// Uncore (ring/L3/memory fabric) maximum clock.
    pub uncore_max_mhz: Option<u32>,
    /// Maximum clock of the efficiency and low-power efficiency cores.
    pub ecap_mhz: Option<u32>,
    /// Energy/performance hint of the CPUs: one of the kernel's names (`performance`,
    /// `balance_performance`, `balance_power`, `power`). A raw number is not the same thing:
    /// `128` measured 1.65x slower than `balance_performance` on this machine.
    pub epp: &'static str,
}

impl Default for EcoLimits {
    fn default() -> Self {
        Self {
            pkg_w: Some((12, 28)),
            pkg_window_ms: Some(10_000),
            psys_w: None,
            uncore_max_mhz: None,
            ecap_mhz: Some(1200),
            epp: "balance_performance",
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
    /// What Eco+ adds on top of power-saver.
    pub eco: EcoLimits,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            saver_no_turbo_on_battery: true,
            aspm_on_battery: true,
            audio_power_save_on_battery: true,
            lazy_writeback_on_battery: true,
            eco: EcoLimits::default(),
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
pub fn plan(
    root: &SysRoot,
    profile: Profile,
    on_battery: bool,
    t: &Tunables,
    restore: &[Write],
) -> Vec<Write> {
    let mut out = Vec::new();

    // 1. Firmware/EC thermal mode (fans, power limits, ASUS/Lenovo mode LEDs).
    if let Some(choices) = root.read_opt(format!("{PLATFORM_PROFILE}_choices")) {
        let choices: Vec<&str> = choices.split_whitespace().collect();
        let wanted: &[&str] = match profile {
            Profile::PowerSaver | Profile::EcoPlus => &["quiet", "low-power", "cool"],
            Profile::Balanced => &["balanced"],
            Profile::Performance => &["performance", "balanced-performance"],
        };
        if let Some(v) = wanted.iter().find(|v| choices.contains(v)) {
            out.push(w(PLATFORM_PROFILE, *v, "firmware thermal mode"));
        }
    }

    // 2. CPU energy/performance preference (intel_pstate and amd-pstate active mode).
    let epp = match (profile, on_battery) {
        (Profile::EcoPlus, _) => t.eco.epp,
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
        (Profile::EcoPlus, _) => "8",
        (Profile::PowerSaver, _) => "15",
        (Profile::Balanced, true) => "8",
        (Profile::Balanced, false) => "6",
        (Profile::Performance, _) => "0",
    };
    for p in root.glob(&format!("{CPU}/cpu*/power/energy_perf_bias")) {
        out.push(w(p, epb, "CPU energy bias"));
    }

    // 4. Turbo.
    // Eco+ keeps turbo: a burst that finishes fast costs less than a slow one that keeps the
    // package out of its idle states, and the budget (PL1) bounds the average anyway.
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

    // 6. Eco+ caps (or, when leaving Eco+, the saved originals).
    if profile == Profile::EcoPlus {
        out.extend(eco_writes(root, &t.eco));
    } else {
        out.extend(restore.iter().cloned());
    }

    out
}

// --- Eco+: RAPL power caps and uncore clock ------------------------------------------------

const POWERCAP: &str = "/sys/class/powercap";
const UNCORE: &str = "/sys/devices/system/cpu/intel_uncore_frequency/package_00_die_00";

/// A top-level RAPL zone with its two limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaplZone {
    pub dir: String,
    /// `package-0`, `psys`, ...
    pub name: String,
    /// Path of the long-term limit file, if the zone has one.
    pub long: Option<String>,
    pub short: Option<String>,
}

impl RaplZone {
    pub fn is_psys(&self) -> bool {
        self.name == "psys"
    }
    pub fn is_package(&self) -> bool {
        self.name.starts_with("package")
    }
}

/// Package and platform zones, from every RAPL interface (`intel-rapl` MSR and
/// `intel-rapl-mmio` both exist on recent Intel; the firmware honours the lower of the two).
pub fn rapl_zones(root: &SysRoot) -> Vec<RaplZone> {
    let mut out = Vec::new();
    for name in root.list(POWERCAP) {
        // `intel-rapl:0` is a zone; `intel-rapl:0:0` is a sub-zone (cores, uncore, ...).
        let top = name.starts_with("intel-rapl") && name.matches(':').count() == 1;
        if !top {
            continue;
        }
        let dir = format!("{POWERCAP}/{name}");
        let Some(zone) = root.read_opt(format!("{dir}/name")) else {
            continue;
        };
        let mut z = RaplZone {
            dir: dir.clone(),
            name: zone,
            long: None,
            short: None,
        };
        if !(z.is_psys() || z.is_package()) {
            continue;
        }
        for i in 0..3 {
            let limit = format!("{dir}/constraint_{i}_power_limit_uw");
            match root.read_opt(format!("{dir}/constraint_{i}_name")).as_deref() {
                Some("long_term") if root.exists(&limit) => z.long = Some(limit),
                Some("short_term") if root.exists(&limit) => z.short = Some(limit),
                _ => {}
            }
        }
        out.push(z);
    }
    out
}

/// The limits Eco+ changes, as they are now: the values to restore on leaving Eco+.
pub fn eco_snapshot(root: &SysRoot) -> Vec<Write> {
    let mut out = Vec::new();
    for z in rapl_zones(root) {
        for path in z.long.iter().chain(z.short.iter()) {
            if let Some(v) = root.read_opt(path) {
                out.push(w(path.clone(), v, "power limit"));
            }
        }
        if let Some(win) = z.long.as_ref().map(|l| window_path(l)) {
            if let Some(v) = root.read_opt(&win) {
                out.push(w(win, v, "power limit window"));
            }
        }
    }
    for p in efficiency_clock_paths(root) {
        if let Some(v) = root.read_opt(&p) {
            out.push(w(p, v, "efficiency core clock"));
        }
    }
    let max = format!("{UNCORE}/max_freq_khz");
    if let Some(v) = root.read_opt(&max) {
        out.push(w(max, v, "uncore clock"));
    }
    out
}

/// The time-window file next to a long-term limit file.
fn window_path(limit: &str) -> String {
    limit.replace("power_limit_uw", "time_window_us")
}

/// `scaling_max_freq` of every efficiency and low-power efficiency core.
fn efficiency_clock_paths(root: &SysRoot) -> Vec<String> {
    let t = crate::topology::detect(root);
    [t.efficiency, t.low_power]
        .concat()
        .into_iter()
        .map(|c| format!("{CPU}/cpu{c}/cpufreq/scaling_max_freq"))
        .filter(|p| root.exists(p))
        .collect()
}

fn uw(watts: u32) -> String {
    (u64::from(watts) * 1_000_000).to_string()
}

/// The writes that apply [`EcoLimits`]. Values are clamped into what the hardware accepts
/// (uncore) and skipped where a zone doesn't exist.
pub fn eco_writes(root: &SysRoot, eco: &EcoLimits) -> Vec<Write> {
    let mut out = Vec::new();
    for z in rapl_zones(root) {
        let limits = if z.is_psys() { eco.psys_w } else { eco.pkg_w };
        let Some((long, short)) = limits else {
            continue;
        };
        if let Some(p) = &z.long {
            out.push(w(p.clone(), uw(long), "power limit"));
        }
        if let Some(p) = &z.short {
            out.push(w(p.clone(), uw(short.max(long)), "power limit"));
        }
        if let (Some(ms), Some(l), true) = (eco.pkg_window_ms, &z.long, z.is_package()) {
            let win = window_path(l);
            if root.exists(&win) {
                out.push(w(win, (u64::from(ms) * 1000).to_string(), "power limit window"));
            }
        }
    }
    if let Some(mhz) = eco.ecap_mhz {
        for p in efficiency_clock_paths(root) {
            let max: u32 = root
                .read_parse(p.replace("scaling_max_freq", "cpuinfo_max_freq"))
                .unwrap_or(u32::MAX);
            let min: u32 = root
                .read_parse(p.replace("scaling_max_freq", "cpuinfo_min_freq"))
                .unwrap_or(0);
            out.push(w(p, (mhz * 1000).clamp(min, max).to_string(), "efficiency core clock"));
        }
    }
    if let Some(mhz) = eco.uncore_max_mhz {
        let min: u32 = root.read_parse(format!("{UNCORE}/min_freq_khz")).unwrap_or(0);
        let max: u32 = root
            .read_parse(format!("{UNCORE}/initial_max_freq_khz"))
            .unwrap_or(u32::MAX);
        if root.exists(format!("{UNCORE}/max_freq_khz")) {
            out.push(w(
                format!("{UNCORE}/max_freq_khz"),
                (mhz * 1000).clamp(min, max).to_string(),
                "uncore clock",
            ));
        }
    }
    out
}

/// Serialise a snapshot as `path=value` lines (kept in `/run/tspmo` while Eco+ is active, so a
/// daemon restart still knows the originals).
pub fn snapshot_text(s: &[Write]) -> String {
    s.iter().map(|x| format!("{}={}\n", x.path, x.value)).collect()
}

pub fn parse_snapshot(text: &str) -> Vec<Write> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(p, _)| p.starts_with("/sys/") && !p.contains(".."))
        .map(|(p, v)| w(p, v, "restore"))
        .collect()
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
        let p = plan(&f.root(), Profile::PowerSaver, true, &Tunables::default(), &[]);
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
        let p = plan(&f.root(), Profile::Performance, false, &Tunables::default(), &[]);
        assert_eq!(get(&p, "firmware thermal mode"), ["performance"]);
        assert_eq!(get(&p, "CPU energy preference"), ["performance"]);
        assert_eq!(get(&p, "CPU turbo"), ["0"]);
        assert_eq!(get(&p, "PCIe link power"), ["default"]);
    }

    #[test]
    fn apply_skips_unchanged_and_reads_bracketed() {
        let f = gu605("apply");
        let root = f.root();
        let p = plan(&root, Profile::Balanced, false, &Tunables::default(), &[]);
        let r = apply(&root, &p);
        // Already balanced / balance_performance / 6 / turbo on / [default] / 10 / 500.
        assert!(r.changed.is_empty(), "{:?}", r.changed);
        assert!(r.failed.is_empty());
        let r = apply(
            &root,
            &plan(&root, Profile::PowerSaver, true, &Tunables::default(), &[]),
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

    #[test]
    fn modes_and_ppd_mapping() {
        assert_eq!(Profile::parse("eco-plus"), Some(Profile::EcoPlus));
        assert_eq!(Profile::EcoPlus.ppd(), Profile::PowerSaver);
        assert_eq!(Profile::Balanced.ppd(), Profile::Balanced);
        let mut p = Profile::EcoPlus;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(p.as_str());
            p = p.next();
        }
        assert_eq!(seen, ["eco-plus", "power-saver", "balanced", "performance"]);
        assert_eq!(p, Profile::EcoPlus);
    }

    fn with_rapl(tag: &str) -> Fake {
        let f = gu605(tag);
        for (dir, name, long, short) in [
            ("intel-rapl:0", "package-0", "90000000", "104000000"),
            ("intel-rapl-mmio:0", "package-0", "90000000", "104000000"),
            ("intel-rapl:1", "psys", "190000000", "260000000"),
            ("intel-rapl:0:0", "core", "0", "0"),
        ] {
            let d = format!("/sys/class/powercap/{dir}");
            f.file(&format!("{d}/name"), &format!("{name}\n"))
                .file(&format!("{d}/constraint_0_name"), "long_term\n")
                .file(&format!("{d}/constraint_0_power_limit_uw"), &format!("{long}\n"))
                .file(&format!("{d}/constraint_1_name"), "short_term\n")
                .file(&format!("{d}/constraint_1_power_limit_uw"), &format!("{short}\n"));
        }
        let u = UNCORE;
        f.file(&format!("{u}/max_freq_khz"), "3400000\n")
            .file(&format!("{u}/min_freq_khz"), "400000\n")
            .file(&format!("{u}/initial_max_freq_khz"), "3400000\n");
        f
    }

    #[test]
    fn gu605_fixture_has_package_and_psys() {
        let root = SysRoot::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/asus-gu605mi"
        ));
        let z = rapl_zones(&root);
        assert_eq!(
            z.iter().map(|z| z.name.as_str()).collect::<Vec<_>>(),
            ["package-0", "psys"]
        );
        assert!(z.iter().all(|z| z.long.is_some() && z.short.is_some()));
        let w = eco_writes(&root, &EcoLimits::default());
        assert!(
            w.iter()
                .any(|w| w.path.ends_with("intel-rapl:0/constraint_0_power_limit_uw"))
        );
        assert_eq!(eco_snapshot(&root).len(), 7); // 2+2 limits, 2 windows, uncore clock
    }

    #[test]
    fn rapl_zones_skip_subzones() {
        let f = with_rapl("zones");
        let z = rapl_zones(&f.root());
        let names: Vec<_> = z
            .iter()
            .map(|z| (z.dir.rsplit('/').next().unwrap(), z.is_psys()))
            .collect();
        assert_eq!(
            names,
            [
                ("intel-rapl-mmio:0", false),
                ("intel-rapl:0", false),
                ("intel-rapl:1", true)
            ]
        );
        assert!(z.iter().all(|z| z.long.is_some() && z.short.is_some()));
    }

    #[test]
    fn eco_plus_caps_and_restores() {
        let f = with_rapl("eco");
        let root = f.root();
        let t = Tunables {
            eco: EcoLimits {
                pkg_w: Some((8, 12)),
                psys_w: None,
                uncore_max_mhz: Some(1600),
                ..EcoLimits::default()
            },
            ..Tunables::default()
        };
        let saved = eco_snapshot(&root);
        assert_eq!(saved.len(), 7); // 3 zones x 2 limits + uncore
        let p = plan(&root, Profile::EcoPlus, true, &t, &[]);
        assert_eq!(get(&p, "firmware thermal mode"), ["quiet"]);
        assert_eq!(get(&p, "CPU turbo"), ["0"]); // Eco+ keeps turbo (no_turbo=0)
        let r = apply(&root, &p);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(
            root.read("/sys/class/powercap/intel-rapl:0/constraint_0_power_limit_uw")
                .unwrap(),
            "8000000"
        );
        assert_eq!(
            root.read("/sys/class/powercap/intel-rapl-mmio:0/constraint_1_power_limit_uw")
                .unwrap(),
            "12000000"
        );
        // psys is left alone when no psys limits are set.
        assert_eq!(
            root.read("/sys/class/powercap/intel-rapl:1/constraint_0_power_limit_uw")
                .unwrap(),
            "190000000"
        );
        assert_eq!(root.read(format!("{UNCORE}/max_freq_khz")).unwrap(), "1600000");

        // Leaving Eco+ writes the snapshot back, including through the on-disk text form.
        let saved = parse_snapshot(&snapshot_text(&saved));
        let p = plan(&root, Profile::Balanced, false, &t, &saved);
        apply(&root, &p);
        assert_eq!(
            root.read("/sys/class/powercap/intel-rapl:0/constraint_0_power_limit_uw")
                .unwrap(),
            "90000000"
        );
        assert_eq!(root.read(format!("{UNCORE}/max_freq_khz")).unwrap(), "3400000");
        // ...and without a snapshot, other profiles never touch the limits.
        let p = plan(&root, Profile::Balanced, false, &t, &[]);
        assert!(
            p.iter()
                .all(|w| w.what != "power limit" && w.what != "uncore clock")
        );
    }

    #[test]
    fn eco_limits_are_clamped() {
        let f = with_rapl("clamp");
        let eco = EcoLimits {
            pkg_w: Some((10, 5)), // short-term below long-term makes no sense
            psys_w: Some((6, 8)),
            uncore_max_mhz: Some(100), // below the hardware minimum
            ..EcoLimits::default()
        };
        let w = eco_writes(&f.root(), &eco);
        let v = |what: &str| -> Vec<String> {
            w.iter()
                .filter(|x| x.what == what)
                .map(|x| x.value.clone())
                .collect()
        };
        assert!(v("power limit").contains(&"10000000".to_owned()));
        assert!(!v("power limit").contains(&"5000000".to_owned()));
        assert!(v("power limit").contains(&"6000000".to_owned()));
        assert_eq!(v("uncore clock"), ["400000"]);
    }

    #[test]
    fn snapshot_rejects_paths_outside_sys() {
        let s = parse_snapshot("/etc/passwd=x\n/sys/../etc/shadow=x\n/sys/a/b=1\ngarbage\n");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].path, "/sys/a/b");
    }
}
