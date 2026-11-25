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
    /// The switch in progress was started by the policy (not asked for by the user): it is
    /// withdrawn when the policy stops wanting it (see [`crate::gpu::switch_still_wanted`]).
    pub gpu_switch_auto: bool,
    /// The policy wants a different GPU mode but an app is using the dGPU; retrying.
    pub gpu_waiting: bool,
    /// Last message from a GPU switch attempt, shown in the UI.
    pub gpu_message: String,
    /// Settings the kernel refused with EPERM/EACCES (firmware-owned); never retried.
    refused: std::collections::HashSet<String>,
    /// The RAPL/uncore values from before Eco+ was applied (empty when Eco+ is not active).
    eco_saved: Vec<power::Write>,
    /// Eco+'s `platform_profile` write is held back until the GPU switch to Eco is over (see
    /// [`crate::gpu::eco_switch_ahead`]). Only a flag: what is written then is worked out from
    /// the mode in force at that time, so a stale write is impossible.
    thermal_held: bool,
    /// The last `platform_profile` value seen by the watcher (a change is a thermal write).
    last_platform: Option<String>,
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
            gpu_switch_auto: false,
            gpu_waiting: false,
            gpu_message: String::new(),
            refused: std::collections::HashSet::new(),
            eco_saved: Vec::new(),
            thermal_held: false,
            last_platform: None,
            applied: None,
            state_file: state_dir.join("state"),
        };
        d.load();
        // A daemon restart while Eco+ was active: the originals are still on disk.
        if let Some(text) = d.root.read_opt(ECO_SAVED) {
            d.eco_saved = power::parse_snapshot(&text);
        }
        d.gpu_cache = gpu::status(&d.root, d.boot_mux);
        // A switch is never in progress at start: whatever a previous daemon's clients last saw
        // (`GpuSwitching`) is stale. A hung driver unload outlives the daemon, though.
        d.gpu_switching = None;
        if d.root.exists(gpu::RESTART_MARKER) {
            d.gpu_message = gpu::RESTART_REQUIRED.to_owned();
        }
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
                let _ = tspmo_sys::write_atomic(
                    &self.root.path(ECO_SAVED),
                    &power::snapshot_text(&self.eco_saved),
                );
            }
            Vec::new()
        } else {
            std::mem::take(&mut self.eco_saved)
        };
        let mut plan = power::plan(&self.root, key.0, key.1, &self.tunables, &restore);
        plan.retain(|w| !self.refused.contains(&w.path));
        // Entering Eco+ with the dGPU on: the switch to Eco comes before the thermal-mode
        // write, whose EC notification would otherwise meet the driver unload.
        let entering = self.applied.is_some_and(|(p, _)| !p.is_eco_plus());
        let defer =
            key.0.is_eco_plus() && (entering || self.thermal_held) && crate::gpu::eco_switch_ahead(self);
        // Everything but the thermal mode goes in now, on entering and on leaving alike.
        self.thermal_held = false;
        if defer {
            if let Some(i) = plan.iter().position(|w| w.path == power::PLATFORM_PROFILE) {
                plan.remove(i);
                self.thermal_held = true;
            }
        }
        let r = power::apply(&self.root, &plan);
        if r.changed.iter().any(|w| w.path == power::PLATFORM_PROFILE) {
            crate::quiet::note_profile_write();
        }
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

    /// Write the thermal mode Eco+ held back while the GPU switched (no-op when none is held).
    /// It is the thermal mode of the mode in force NOW: when Eco+ has been left, leaving already
    /// wrote the new mode and nothing is written here.
    pub fn release_thermal(&mut self) {
        if !std::mem::take(&mut self.thermal_held) || !self.effective().is_eco_plus() {
            return;
        }
        let plan = power::plan(&self.root, self.effective(), self.on_battery, &self.tunables, &[]);
        let Some(w) = plan.into_iter().find(|w| w.path == power::PLATFORM_PROFILE) else {
            return;
        };
        let r = power::apply(&self.root, std::slice::from_ref(&w));
        if !r.changed.is_empty() {
            crate::quiet::note_profile_write();
            crate::events::publish(&crate::events::Event::ThermalModeWritten {
                profile: self.effective().ppd(),
                on_battery: self.on_battery,
            });
        }
        for (w, e) in &r.failed {
            eprintln!("tspmod: {} ({} = {}) failed: {e}", w.what, w.path, w.value);
        }
    }

    /// Whether Eco+'s thermal-mode write is still held back.
    #[cfg(test)]
    pub fn thermal_held(&self) -> bool {
        self.thermal_held
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
        // Any change, ours or the firmware's, makes the EC notify the NVIDIA driver.
        if self.last_platform.as_deref().is_some_and(|p| p != value) {
            crate::quiet::note_profile_write();
        }
        self.last_platform = Some(value.to_owned());
        // Eco+'s own write is still held back: what the firmware shows is not a choice.
        if self.thermal_held {
            return false;
        }
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

    /// Holds one client may keep, and the longest reason or application id kept: a hold is
    /// cheap to ask for, and a message may carry megabytes.
    pub const MAX_HOLDS_PER_CLIENT: usize = 32;
    pub const MAX_HOLD_TEXT: usize = 256;

    pub fn hold(
        &mut self,
        profile: Profile,
        reason: String,
        app_id: String,
        owner: String,
    ) -> Result<u32, String> {
        if self.holds.iter().filter(|h| h.owner == owner).count() >= Self::MAX_HOLDS_PER_CLIENT {
            return Err(format!("too many holds ({})", Self::MAX_HOLDS_PER_CLIENT));
        }
        // Cookies are unique among live holds even after the counter wraps.
        let mut cookie = self.next_cookie;
        while self.holds.iter().any(|h| h.cookie == cookie) {
            cookie = cookie.wrapping_add(1).max(1);
        }
        self.next_cookie = cookie.wrapping_add(1).max(1);
        let clip = |s: String| -> String { s.chars().take(Self::MAX_HOLD_TEXT).collect() };
        self.holds.push(Hold {
            cookie,
            profile,
            reason: clip(reason),
            app_id: clip(app_id),
            owner,
        });
        self.apply(false);
        Ok(cookie)
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
        if let Err(e) = tspmo_sys::write_atomic(&self.root.path(FILE), &text) {
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
        d.hold(p, "test".into(), "test".into(), ":1.1".into()).unwrap()
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

    /// A settled system with the dGPU on in Hybrid and the ASUS switch present.
    fn hybrid_with_dgpu(env: &Env) -> Daemon {
        let f = |p: &str, c: &str| {
            let p = env.0.join(p.trim_start_matches('/'));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        let dev = "/sys/bus/pci/devices/0000:01:00.0";
        f(&format!("{dev}/class"), "0x030200\n");
        f(&format!("{dev}/vendor"), "0x10de\n");
        f(&format!("{dev}/power/runtime_status"), "suspended\n");
        crate::boot::TEST_ALLOWED.with(|c| c.set(true));
        let mut d = env.daemon();
        d.gpu_cache = gpu::status(&d.root, None);
        d.gpu_cache.switcher = gpu::Switcher::Asus {
            fw_attrs: true,
            has_mux: false,
        };
        d.gpu_cache.mode = gpu::Mode::Hybrid;
        d.gpu_cache.pending = None;
        d.gpu_policy = Policy::Fixed(gpu::Mode::Hybrid);
        d
    }

    #[test]
    fn eco_plus_switches_the_gpu_before_it_writes_the_thermal_mode() {
        let env = Env::new("order");
        let mut d = hybrid_with_dgpu(&env);
        d.choose(Profile::Balanced);
        d.choose(Profile::EcoPlus);
        // Everything else is applied; the firmware thermal mode waits for the GPU switch.
        assert_eq!(env.read(PKG), "12000000");
        assert_eq!(env.read(power::PLATFORM_PROFILE), "balanced");
        assert!(d.thermal_held());
        // The firmware's own echo of the old mode is not a choice that cancels Eco+.
        assert!(!d.platform_changed("balanced"));
        assert_eq!(d.effective(), Profile::EcoPlus);
        // The GPU switch is over (done, failed or never started): the mode goes in.
        d.release_thermal();
        assert_eq!(env.read(power::PLATFORM_PROFILE), "quiet");
        assert!(!d.thermal_held());
        assert!(!d.platform_changed("quiet"));
        assert_eq!(d.effective(), Profile::EcoPlus);
    }

    #[test]
    fn eco_plus_writes_the_thermal_mode_at_once_when_no_gpu_switch_is_ahead() {
        // Already in Eco: no driver to unload.
        let env = Env::new("order-eco");
        let mut d = hybrid_with_dgpu(&env);
        d.gpu_cache.mode = gpu::Mode::Eco;
        d.choose(Profile::Balanced);
        d.choose(Profile::EcoPlus);
        assert_eq!(env.read(power::PLATFORM_PROFILE), "quiet");
        assert!(!d.thermal_held());
        // Ultimate is a reboot-only choice that Eco+ leaves alone.
        let env = Env::new("order-ult");
        let mut d = hybrid_with_dgpu(&env);
        d.gpu_policy = Policy::Fixed(gpu::Mode::Ultimate);
        d.choose(Profile::Balanced);
        d.choose(Profile::EcoPlus);
        assert_eq!(env.read(power::PLATFORM_PROFILE), "quiet");
        // Before the boot has settled nothing about the GPU is touched.
        let env = Env::new("order-boot");
        let mut d = hybrid_with_dgpu(&env);
        crate::boot::TEST_ALLOWED.with(|c| c.set(false));
        d.choose(Profile::Balanced);
        d.choose(Profile::EcoPlus);
        assert_eq!(env.read(power::PLATFORM_PROFILE), "quiet");
    }

    #[test]
    fn leaving_eco_plus_before_the_gpu_switch_drops_the_held_write() {
        let env = Env::new("order-leave");
        let mut d = hybrid_with_dgpu(&env);
        d.choose(Profile::Balanced);
        d.choose(Profile::EcoPlus);
        assert!(d.thermal_held());
        d.choose(Profile::Performance);
        assert!(!d.thermal_held());
        assert_eq!(env.read(power::PLATFORM_PROFILE), "performance");
        d.release_thermal(); // nothing left to write
        assert_eq!(env.read(power::PLATFORM_PROFILE), "performance");
    }

    #[test]
    fn a_stale_release_never_writes_an_eco_plus_thermal_mode() {
        let env = Env::new("stale");
        let mut d = hybrid_with_dgpu(&env);
        d.choose(Profile::Balanced);
        d.choose(Profile::EcoPlus);
        assert!(d.thermal_held());
        d.choose(Profile::Balanced);
        assert_eq!(env.read(power::PLATFORM_PROFILE), "balanced");
        // A late release from the switch that was started for Eco+ (whatever state it finds).
        d.thermal_held = true;
        d.release_thermal();
        assert_eq!(env.read(power::PLATFORM_PROFILE), "balanced");
        assert!(!d.thermal_held());
        // Likewise after Eco+ -> Performance via a hold.
        d.choose(Profile::EcoPlus);
        hold(&mut d, Profile::Performance);
        d.release_thermal();
        assert_eq!(env.read(power::PLATFORM_PROFILE), "performance");
    }

    #[test]
    fn a_release_writes_the_mode_in_force_now() {
        let env = Env::new("current");
        let mut d = hybrid_with_dgpu(&env);
        // The late write tells whoever owns fan curves and limits (the EC drops them).
        let seen = Arc::new(Mutex::new(0u32));
        let n = seen.clone();
        crate::events::subscribe(move |e| {
            if matches!(e, crate::events::Event::ThermalModeWritten { .. }) {
                *n.lock().unwrap() += 1;
            }
        });
        d.choose(Profile::Balanced);
        d.choose(Profile::EcoPlus);
        assert_eq!(env.read(power::PLATFORM_PROFILE), "balanced");
        d.release_thermal();
        assert_eq!(env.read(power::PLATFORM_PROFILE), "quiet");
        assert!(*seen.lock().unwrap() >= 1);
        // Released once: a second call writes nothing.
        std::fs::write(
            env.0.join(power::PLATFORM_PROFILE.trim_start_matches('/')),
            "balanced\n",
        )
        .unwrap();
        d.release_thermal();
        assert_eq!(env.read(power::PLATFORM_PROFILE), "balanced");
    }

    #[test]
    fn eco_plus_takes_all_but_the_thermal_mode_at_once_and_leaving_restores_it_all_during_a_switch() {
        let env = Env::new("inflight");
        let mut d = hybrid_with_dgpu(&env);
        d.choose(Profile::Balanced);
        d.choose(Profile::EcoPlus);
        // The switch to Eco is running (waiting for the GPU to settle).
        d.gpu_switching = Some(gpu::Mode::Eco);
        d.gpu_switch_auto = true;
        assert_eq!(env.read(PKG), "12000000");
        assert!(d.thermal_held());
        assert_eq!(env.read(power::PLATFORM_PROFILE), "balanced");
        assert!(crate::gpu::switch_still_wanted(&d, gpu::Mode::Eco));
        // Leaving Eco+ now: the limits and the thermal mode come back at once, the switch
        // is told it is no longer wanted, and nothing is left to release.
        d.choose(Profile::Performance);
        assert_eq!(env.read(PKG), "90000000");
        assert_eq!(env.read(power::PLATFORM_PROFILE), "performance");
        assert!(!d.thermal_held());
        assert!(!crate::gpu::switch_still_wanted(&d, gpu::Mode::Eco));
        assert!(!env.0.join(ECO_SAVED.trim_start_matches('/')).exists());
        d.release_thermal();
        assert_eq!(env.read(power::PLATFORM_PROFILE), "performance");
    }

    #[test]
    fn a_users_own_gpu_switch_is_never_withdrawn() {
        let env = Env::new("manual");
        let mut d = hybrid_with_dgpu(&env);
        d.gpu_switching = Some(gpu::Mode::Eco);
        d.gpu_switch_auto = false;
        assert!(crate::gpu::switch_still_wanted(&d, gpu::Mode::Eco));
        d.gpu_policy = Policy::Fixed(gpu::Mode::Hybrid);
        assert!(crate::gpu::switch_still_wanted(&d, gpu::Mode::Eco));
        d.gpu_switch_auto = true;
        assert!(!crate::gpu::switch_still_wanted(&d, gpu::Mode::Eco));
    }

    #[test]
    fn a_thermal_write_is_recorded_for_the_unload_wait() {
        let env = Env::new("stamp");
        let mut d = env.daemon();
        d.choose(Profile::Balanced);
        let before = crate::quiet::last_profile_write();
        d.choose(Profile::Performance); // balanced -> performance: a write
        let after = crate::quiet::last_profile_write();
        assert!(after.is_some() && after >= before);
        assert!(
            crate::quiet::thermal_remaining(crate::quiet::Clock::now(&crate::quiet::Real), after).is_some()
        );
    }

    #[test]
    fn start_clears_a_stale_switch_and_remembers_a_hung_unload() {
        let env = Env::new("start");
        let d = env.daemon();
        assert_eq!(d.gpu_switching, None);
        assert!(d.gpu_message.is_empty());
        let marker = env.0.join(gpu::RESTART_MARKER.trim_start_matches('/'));
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(marker, "rmmod hung\n").unwrap();
        let d = env.daemon();
        assert_eq!(d.gpu_switching, None);
        assert_eq!(d.gpu_message, "Restart to recover the NVIDIA GPU");
    }

    #[test]
    fn holds_are_bounded_per_client_and_clipped() {
        let env = Env::new("holdcap");
        let mut d = env.daemon();
        let long = "x".repeat(10_000);
        let c = d
            .hold(Profile::PowerSaver, long.clone(), long, ":1.9".into())
            .unwrap();
        assert_eq!(d.holds[0].reason.len(), Daemon::MAX_HOLD_TEXT);
        assert_eq!(d.holds[0].cookie, c);
        for _ in 1..Daemon::MAX_HOLDS_PER_CLIENT {
            d.hold(Profile::PowerSaver, "r".into(), "a".into(), ":1.9".into())
                .unwrap();
        }
        assert!(
            d.hold(Profile::PowerSaver, "r".into(), "a".into(), ":1.9".into())
                .is_err()
        );
        // Another client is unaffected.
        assert!(
            d.hold(Profile::PowerSaver, "r".into(), "a".into(), ":1.10".into())
                .is_ok()
        );
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
