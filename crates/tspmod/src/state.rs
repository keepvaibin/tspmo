//! The daemon's state and the decisions it makes from it.
//!
//! Everything that changes hardware goes through [`Daemon`], under one mutex, so the D-Bus
//! handlers and the event watchers can never interleave writes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use tspmo_core::gpu::{self, Policy};
use tspmo_core::power::{self, Profile, Tunables};
use tspmo_sys::{SysRoot, power_supply};

/// Where the pre-Eco+ limits are kept while Eco+ is active (cleared at boot with /run).
pub const ECO_SAVED: &str = "/run/tspmo/rapl-orig";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub cookie: u32,
    pub profile: Profile,
    pub reason: String,
    pub app_id: String,
    /// Unique bus name of the caller; the hold is dropped when it disconnects.
    pub owner: String,
}

#[derive(Debug)]
pub struct Daemon {
    pub root: SysRoot,
    pub on_battery: bool,
    pub profile_ac: Profile,
    pub profile_battery: Profile,
    pub holds: Vec<Hold>,
    next_cookie: u32,
    pub tunables: Tunables,
    pub gpu_policy: Policy,
    /// `gpu_mux_mode` as it was when we started (a later change means a reboot is pending).
    pub boot_mux: Option<u8>,
    /// Last firmware-read GPU status (see [`Daemon::gpu_status`]).
    pub gpu_cache: gpu::Status,
    /// A GPU switch in progress (the firmware takes seconds to power the dGPU on).
    pub gpu_switching: Option<gpu::Mode>,
    /// The policy wants a different GPU mode but an app is using the dGPU; retrying.
    pub gpu_waiting: bool,
    /// Last message from a GPU switch attempt, shown in the UI.
    pub gpu_message: String,
    /// Settings the kernel refused with EPERM/EACCES (firmware-owned); never retried.
    refused: std::collections::HashSet<String>,
    /// The RAPL/uncore values from before Eco+ was applied (empty when Eco+ is not active).
    eco_saved: Vec<power::Write>,
    /// What was last applied, to avoid redundant firmware writes.
    applied: Option<(Profile, bool)>,
    state_file: PathBuf,
}

impl Daemon {
    pub fn new(root: SysRoot) -> Self {
        let state_dir = std::env::var_os("STATE_DIRECTORY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/tspmo"));
        Self::with_state_dir(root, state_dir)
    }

    pub fn with_state_dir(root: SysRoot, state_dir: PathBuf) -> Self {
        let mut d = Self {
            on_battery: power_supply::on_ac(&root) == Some(false),
            boot_mux: tspmo_sys::fwattr::read(&root, gpu::ASUS_FW, "gpu_mux_mode")
                .and_then(|a| a.current?.parse().ok())
                .or_else(|| root.read_parse(format!("{}/gpu_mux_mode", gpu::ASUS_LEGACY))),
            root,
            profile_ac: Profile::Balanced,
            profile_battery: Profile::PowerSaver,
            holds: Vec::new(),
            next_cookie: 1,
            tunables: Tunables::default(),
            // Eco on battery also sidesteps the NVIDIA "wake on every 1% of battery" bug.
            gpu_policy: Policy::Optimized,
            gpu_cache: gpu::Status {
                switcher: gpu::Switcher::None,
                mode: gpu::Mode::Hybrid,
                pending: None,
                dgpu: None,
                dgpu_asleep: false,
            },
            gpu_switching: None,
            gpu_waiting: false,
            gpu_message: String::new(),
            refused: std::collections::HashSet::new(),
            eco_saved: Vec::new(),
            applied: None,
            state_file: state_dir.join("state"),
        };
        d.load();
        // A daemon restart while Eco+ was active: the originals are still on disk.
        if let Some(text) = d.root.read_opt(ECO_SAVED) {
            d.eco_saved = power::parse_snapshot(&text);
        }
        d.gpu_cache = gpu::status(&d.root, d.boot_mux);
        d
    }

    /// The profile the user chose for the current power source.
    pub fn chosen(&self) -> Profile {
        if self.on_battery {
            self.profile_battery
        } else {
            self.profile_ac
        }
    }

    /// The mode in force: holds win (performance over power-saver), as in power-profiles-daemon.
    /// A power-saver hold never lowers Eco+, which already saves more.
    pub fn effective(&self) -> Profile {
        if self.holds.iter().any(|h| h.profile == Profile::Performance) {
            Profile::Performance
        } else if self.holds.iter().any(|h| h.profile == Profile::PowerSaver) {
            if self.chosen() == Profile::EcoPlus {
                Profile::EcoPlus
            } else {
                Profile::PowerSaver
            }
        } else {
            self.chosen()
        }
    }

    /// Apply the effective profile if it (or the power source) changed since last time.
    pub fn apply(&mut self, force: bool) {
        let key = (self.effective(), self.on_battery);
        if !force && self.applied == Some(key) {
            return;
        }
        // Eco+ changes limits the firmware owns: remember them on entering, put them back on leaving.
        let restore = if key.0.is_eco_plus() {
            if self.eco_saved.is_empty() {
                self.eco_saved = power::eco_snapshot(&self.root);
                let path = self.root.path(ECO_SAVED);
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, power::snapshot_text(&self.eco_saved));
            }
            Vec::new()
        } else {
            std::mem::take(&mut self.eco_saved)
        };
        let mut plan = power::plan(&self.root, key.0, key.1, &self.tunables, &restore);
        plan.retain(|w| !self.refused.contains(&w.path));
        let r = power::apply(&self.root, &plan);
        for (w, e) in &r.failed {
            // The firmware owns some settings (e.g. PCIe ASPM when the ACPI FADT forbids OS
            // control): note it once and stop asking.
            if e.contains("os error 1)") || e.contains("os error 13)") {
                eprintln!(
                    "tspmod: {} is controlled by the firmware on this laptop; leaving it alone",
                    w.what
                );
                self.refused.insert(w.path.clone());
            } else {
                eprintln!("tspmod: {} ({} = {}) failed: {e}", w.what, w.path, w.value);
            }
        }
        if !r.changed.is_empty() {
            eprintln!(
                "tspmod: {} on {}: {} settings changed",
                key.0.as_str(),
                if key.1 { "battery" } else { "AC" },
                r.changed.len()
            );
        }
        if !key.0.is_eco_plus() {
            let _ = std::fs::remove_file(self.root.path(ECO_SAVED));
        }
        self.applied = Some(key);
        // Eco+ is a power-saver with extras: listeners keyed on the three profiles see
        // `power-saver` and can look at `eco_plus` for the rest.
        crate::events::publish(&crate::events::Event::ProfileApplied {
            profile: key.0.ppd(),
            eco_plus: key.0.is_eco_plus(),
            on_battery: key.1,
        });
    }

    /// The user picked a profile (from KDE/GNOME, the GUI, the CLI or a hotkey).
    pub fn choose(&mut self, p: Profile) {
        if self.on_battery {
            self.profile_battery = p;
        } else {
            self.profile_ac = p;
        }
        // An explicit choice cancels holds asking for something else (power-profiles-daemon does the same).
        self.holds.retain(|h| h.profile == p);
        self.save();
        self.apply(false);
    }

    /// KDE/GNOME chose a power-profiles profile. Choosing `power-saver` while Eco+ is active
    /// keeps Eco+ (it already is a power-saver); anything else leaves it.
    pub fn choose_ppd(&mut self, p: Profile) {
        if p == Profile::PowerSaver && self.chosen() == Profile::EcoPlus {
            return;
        }
        self.choose(p);
    }

    /// The firmware changed `platform_profile` itself (Fn+F5 on ASUS, Fn+Q on Lenovo). Our own
    /// writes come back here too: they match the mode in force and change nothing.
    pub fn platform_changed(&mut self, value: &str) -> bool {
        match Profile::from_platform(value) {
            Some(p) if p != self.effective().ppd() => {
                self.choose(p);
                true
            }
            _ => false,
        }
    }

    pub fn set_on_battery(&mut self, on_battery: bool) -> bool {
        if self.on_battery == on_battery {
            return false;
        }
        self.on_battery = on_battery;
        crate::events::publish(&crate::events::Event::PowerSource { on_battery });
        self.apply(false);
        true
    }

    pub fn hold(&mut self, profile: Profile, reason: String, app_id: String, owner: String) -> u32 {
        let cookie = self.next_cookie;
        self.next_cookie = self.next_cookie.wrapping_add(1).max(1);
        self.holds.push(Hold {
            cookie,
            profile,
            reason,
            app_id,
            owner,
        });
        self.apply(false);
        cookie
    }

    pub fn release(&mut self, cookie: u32) -> bool {
        let before = self.holds.len();
        self.holds.retain(|h| h.cookie != cookie);
        let changed = self.holds.len() != before;
        if changed {
            self.apply(false);
        }
        changed
    }

    /// Drop holds whose owner left the bus; returns the released cookies.
    pub fn owner_vanished(&mut self, owner: &str) -> Vec<u32> {
        let gone: Vec<u32> = self
            .holds
            .iter()
            .filter(|h| h.owner == owner)
            .map(|h| h.cookie)
            .collect();
        if !gone.is_empty() {
            self.holds.retain(|h| h.owner != owner);
            self.apply(false);
        }
        gone
    }

    /// Environment for the compositor (read by tspmo's systemd drop-in for KWin at session
    /// start). Outside Ultimate mode KWin loads only Mesa's EGL: it then holds nothing on the
    /// NVIDIA GPU but its display device, which it releases when told the GPU is leaving, so
    /// Eco can switch live. Apps still get the dGPU through PRIME offload.
    pub fn write_compositor_env(&self) {
        const FILE: &str = "/run/tspmo/compositor.env";
        let mesa = self
            .root
            .glob("/usr/share/glvnd/egl_vendor.d/*mesa*.json")
            .into_iter()
            .next();
        let ultimate = self.gpu_status().mode == gpu::Mode::Ultimate;
        let text = match (ultimate, mesa) {
            (false, Some(m)) => format!(
                "# Managed by tspmod: the desktop composites on the iGPU (apps can still use the dGPU).\n\
                 __EGL_VENDOR_LIBRARY_FILENAMES={m}\n"
            ),
            _ => "# Managed by tspmod: no compositor overrides in this GPU mode.\n".to_owned(),
        };
        let path = self.root.path(FILE);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("tspmod: can't write {FILE}: {e}");
        }
    }

    /// GPU status without touching the firmware: the mode and pending MUX state only change
    /// when tspmod changes them (cached in `gpu_cache`), while the dGPU's presence and sleep
    /// state are read from PCI sysfs, which is instant. Firmware reads block for seconds while
    /// the firmware is powering the dGPU up, so they must never happen on a query path.
    pub fn gpu_status(&self) -> gpu::Status {
        let mut st = self.gpu_cache.clone();
        let dg = gpu::dgpu(&self.root);
        st.dgpu_asleep = dg.as_ref().is_some_and(tspmo_sys::pci::Gpu::is_asleep);
        st.dgpu = dg.map(|g| g.address);
        st
    }

    /// Re-read the GPU mode from the firmware (start-up and after a switch, off any lock).
    pub fn read_gpu_status(root: &SysRoot, boot_mux: Option<u8>) -> gpu::Status {
        gpu::status(root, boot_mux)
    }

    // --- persistence: a tiny key=value file, no serde needed --------------------------------

    fn load(&mut self) {
        let Ok(text) = std::fs::read_to_string(&self.state_file) else {
            return;
        };
        let kv: BTreeMap<&str, &str> = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        let get_p = |k: &str| kv.get(k).and_then(|v| Profile::parse(v));
        let get_b = |k: &str| kv.get(k).map(|v| *v == "true");
        if let Some(p) = get_p("profile_ac") {
            self.profile_ac = p;
        }
        if let Some(p) = get_p("profile_battery") {
            self.profile_battery = p;
        }
        if let Some(p) = kv.get("gpu_policy").and_then(|v| Policy::parse(v)) {
            self.gpu_policy = p;
        }
        let t = &mut self.tunables;
        // Eco+ was redesigned (version 2): the values an older daemon saved (a flat 5/7 W cap)
        // are not read back.
        if kv.get("eco_version") == Some(&"2") {
            if let Some(pair) = kv.get("eco_pkg_w").and_then(|v| parse_pair(v)) {
                t.eco.pkg_w = pair;
            }
            if let Some(v) = kv.get("eco_pkg_window_ms") {
                t.eco.pkg_window_ms = v.parse().ok();
            }
            if let Some(pair) = kv.get("eco_psys_w").and_then(|v| parse_pair(v)) {
                t.eco.psys_w = pair;
            }
            if let Some(v) = kv.get("eco_uncore_mhz") {
                t.eco.uncore_max_mhz = v.parse().ok();
            }
            if let Some(v) = kv.get("eco_ecap_mhz") {
                t.eco.ecap_mhz = v.parse().ok();
            }
            if let Some(v) = ["performance", "balance_performance", "balance_power", "power"]
                .into_iter()
                .find(|n| kv.get("eco_epp") == Some(n))
            {
                t.eco.epp = v;
            }
        }
        if let Some(b) = get_b("saver_no_turbo_on_battery") {
            t.saver_no_turbo_on_battery = b;
        }
        if let Some(b) = get_b("aspm_on_battery") {
            t.aspm_on_battery = b;
        }
        if let Some(b) = get_b("audio_power_save_on_battery") {
            t.audio_power_save_on_battery = b;
        }
        if let Some(b) = get_b("lazy_writeback_on_battery") {
            t.lazy_writeback_on_battery = b;
        }
    }

    pub fn save(&self) {
        let t = &self.tunables;
        let text = format!(
            "# tspmod state (managed by tspmod; safe to edit while it is stopped)\n\
             profile_ac={}\nprofile_battery={}\ngpu_policy={}\n\
             saver_no_turbo_on_battery={}\naspm_on_battery={}\n\
             audio_power_save_on_battery={}\nlazy_writeback_on_battery={}\n\
             eco_version=2\neco_pkg_w={}\neco_pkg_window_ms={}\neco_psys_w={}\neco_uncore_mhz={}\n\
             eco_ecap_mhz={}\neco_epp={}\n",
            self.profile_ac.as_str(),
            self.profile_battery.as_str(),
            self.gpu_policy.as_str(),
            t.saver_no_turbo_on_battery,
            t.aspm_on_battery,
            t.audio_power_save_on_battery,
            t.lazy_writeback_on_battery,
            fmt_pair(t.eco.pkg_w),
            fmt_opt(t.eco.pkg_window_ms),
            fmt_pair(t.eco.psys_w),
            fmt_opt(t.eco.uncore_max_mhz),
            fmt_opt(t.eco.ecap_mhz),
            t.eco.epp,
        );
        if let Some(dir) = self.state_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Write-then-rename so a power cut can't leave a truncated file.
        let tmp = self.state_file.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.state_file);
        }
    }
}

/// Put back the limits Eco+ changed (the file survives a daemon crash, so this also works
/// after one). Used when the daemon stops.
pub fn restore_saved(root: &SysRoot) {
    let Some(text) = root.read_opt(ECO_SAVED) else {
        return;
    };
    let r = power::apply(root, &power::parse_snapshot(&text));
    for (w, e) in &r.failed {
        eprintln!("tspmod: can't restore {}: {e}", w.path);
    }
    let _ = std::fs::remove_file(root.path(ECO_SAVED));
}

/// `10,15` (long-term, short-term watts) or `off`.
fn parse_pair(v: &str) -> Option<Option<(u32, u32)>> {
    if v == "off" {
        return Some(None);
    }
    let (a, b) = v.split_once(',')?;
    Some(Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
}

fn fmt_opt(v: Option<u32>) -> String {
    v.map_or_else(|| "off".into(), |m| m.to_string())
}

fn fmt_pair(p: Option<(u32, u32)>) -> String {
    p.map_or_else(|| "off".into(), |(a, b)| format!("{a},{b}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Env(PathBuf);
    impl Env {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("tspmod-state-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            let f = |p: &str, c: &str| {
                let p = d.join(p.trim_start_matches('/'));
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, c).unwrap();
            };
            f(power::PLATFORM_PROFILE, "balanced\n");
            f(
                &format!("{}_choices", power::PLATFORM_PROFILE),
                "quiet balanced performance\n",
            );
            f("/sys/devices/system/cpu/intel_pstate/no_turbo", "0\n");
            for (dir, name) in [("intel-rapl:0", "package-0"), ("intel-rapl:1", "psys")] {
                let b = format!("/sys/class/powercap/{dir}");
                f(&format!("{b}/name"), &format!("{name}\n"));
                f(&format!("{b}/constraint_0_name"), "long_term\n");
                f(&format!("{b}/constraint_0_power_limit_uw"), "90000000\n");
                f(&format!("{b}/constraint_1_name"), "short_term\n");
                f(&format!("{b}/constraint_1_power_limit_uw"), "104000000\n");
            }
            f("/sys/class/power_supply/ACAD/type", "Mains\n");
            f("/sys/class/power_supply/ACAD/online", "1\n");
            Self(d)
        }
        fn daemon(&self) -> Daemon {
            Daemon::with_state_dir(SysRoot::new(&self.0), self.0.join("var"))
        }
        fn read(&self, p: &str) -> String {
            std::fs::read_to_string(self.0.join(p.trim_start_matches('/')))
                .unwrap()
                .trim()
                .to_owned()
        }
    }
    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const PKG: &str = "/sys/class/powercap/intel-rapl:0/constraint_0_power_limit_uw";

    fn hold(d: &mut Daemon, p: Profile) -> u32 {
        d.hold(p, "test".into(), "test".into(), ":1.1".into())
    }

    #[test]
    fn eco_plus_applies_caps_and_reports_itself() {
        let env = Env::new("eco");
        let mut d = env.daemon();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        crate::events::subscribe(move |e| {
            if let crate::events::Event::ProfileApplied {
                profile, eco_plus, ..
            } = e
            {
                log.lock().unwrap().push((*profile, *eco_plus));
            }
        });
        d.choose(Profile::EcoPlus);
        assert_eq!(d.effective(), Profile::EcoPlus);
        assert_eq!(env.read(PKG), "12000000");
        assert_eq!(env.read(power::PLATFORM_PROFILE), "quiet");
        assert_eq!(env.read("/sys/devices/system/cpu/intel_pstate/no_turbo"), "0");
        assert!(
            env.read(ECO_SAVED)
                .contains("constraint_0_power_limit_uw=90000000")
        );
        assert!(seen.lock().unwrap().contains(&(Profile::PowerSaver, true)));

        // Leaving Eco+ restores what was there before, and forgets the saved copy.
        d.choose(Profile::Balanced);
        assert_eq!(env.read(PKG), "90000000");
        assert_eq!(env.read("/sys/devices/system/cpu/intel_pstate/no_turbo"), "0");
        assert!(!env.0.join(ECO_SAVED.trim_start_matches('/')).exists());
        assert!(seen.lock().unwrap().contains(&(Profile::Balanced, false)));
    }

    #[test]
    fn kde_power_saver_keeps_eco_plus_and_anything_else_leaves_it() {
        let env = Env::new("kde");
        let mut d = env.daemon();
        d.choose(Profile::EcoPlus);
        d.choose_ppd(Profile::PowerSaver);
        assert_eq!(d.effective(), Profile::EcoPlus);
        d.choose_ppd(Profile::Performance);
        assert_eq!(d.effective(), Profile::Performance);
        assert_eq!(env.read(PKG), "90000000");
    }

    #[test]
    fn holds_never_lower_eco_plus_but_performance_holds_win() {
        let env = Env::new("holds");
        let mut d = env.daemon();
        d.choose(Profile::EcoPlus);
        let c = hold(&mut d, Profile::PowerSaver);
        assert_eq!(d.effective(), Profile::EcoPlus);
        d.release(c);
        hold(&mut d, Profile::Performance);
        assert_eq!(d.effective(), Profile::Performance);
        assert_eq!(env.read(PKG), "90000000");
    }

    #[test]
    fn our_own_platform_profile_write_does_not_cancel_eco_plus() {
        let env = Env::new("platform");
        let mut d = env.daemon();
        d.choose(Profile::EcoPlus);
        // The kernel reports the `quiet` we just wrote.
        assert!(!d.platform_changed("low-power"));
        assert_eq!(d.effective(), Profile::EcoPlus);
        // Fn+F5 to balanced is a real change.
        assert!(d.platform_changed("balanced"));
        assert_eq!(d.effective(), Profile::Balanced);
    }

    #[test]
    fn restart_while_eco_plus_still_knows_the_originals() {
        let env = Env::new("restart");
        let mut d = env.daemon();
        d.choose(Profile::EcoPlus);
        drop(d);
        // The caps are on the hardware; a new daemon starts from the saved copy, not from them.
        let mut d2 = env.daemon();
        d2.apply(true);
        assert_eq!(env.read(PKG), "12000000");
        d2.choose(Profile::Balanced);
        assert_eq!(env.read(PKG), "90000000");
    }

    #[test]
    fn stopping_restores_the_limits() {
        let env = Env::new("stop");
        let mut d = env.daemon();
        d.choose(Profile::EcoPlus);
        restore_saved(&SysRoot::new(&env.0));
        assert_eq!(env.read(PKG), "90000000");
        assert!(!env.0.join(ECO_SAVED.trim_start_matches('/')).exists());
    }

    #[test]
    fn eco_plus_is_persisted_per_power_source() {
        let env = Env::new("persist");
        let mut d = env.daemon();
        d.choose(Profile::EcoPlus); // on AC
        d.on_battery = true;
        d.choose(Profile::PowerSaver);
        drop(d);
        let d = env.daemon();
        assert_eq!(
            (d.profile_ac, d.profile_battery),
            (Profile::EcoPlus, Profile::PowerSaver)
        );
    }
}
