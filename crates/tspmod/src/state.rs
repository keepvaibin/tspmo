//! The daemon's state and the decisions it makes from it.
//!
//! Everything that changes hardware goes through [`Daemon`], under one mutex, so the D-Bus
//! handlers and the event watchers can never interleave writes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use tspmo_core::gpu::{self, Policy};
use tspmo_core::power::{self, Profile, Tunables};
use tspmo_sys::{SysRoot, power_supply};

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
    /// Keep the compositor on the iGPU (KWIN_DRM_DEVICES), so Eco/Hybrid switch instantly.
    /// Costs the outputs wired to the dGPU (HDMI and one USB-C on a G16).
    pub compositor_igpu_only: bool,
    /// Last message from a GPU switch attempt, shown in the UI.
    pub gpu_message: String,
    /// What was last applied, to avoid redundant firmware writes.
    applied: Option<(Profile, bool)>,
    state_file: PathBuf,
}

impl Daemon {
    pub fn new(root: SysRoot) -> Self {
        let state_dir = std::env::var_os("STATE_DIRECTORY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/tspmo"));
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
            compositor_igpu_only: false,
            gpu_message: String::new(),
            applied: None,
            state_file: state_dir.join("state"),
        };
        d.load();
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

    /// The profile in force: holds win (performance over power-saver), as in power-profiles-daemon.
    pub fn effective(&self) -> Profile {
        if self.holds.iter().any(|h| h.profile == Profile::Performance) {
            Profile::Performance
        } else if self.holds.iter().any(|h| h.profile == Profile::PowerSaver) {
            Profile::PowerSaver
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
        let plan = power::plan(&self.root, key.0, key.1, &self.tunables);
        let r = power::apply(&self.root, &plan);
        for (w, e) in &r.failed {
            eprintln!("tspmod: {} ({} = {}) failed: {e}", w.what, w.path, w.value);
        }
        if !r.changed.is_empty() {
            eprintln!(
                "tspmod: {} on {}: {} settings changed",
                key.0.as_str(),
                if key.1 { "battery" } else { "AC" },
                r.changed.len()
            );
        }
        self.applied = Some(key);
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

    /// The firmware changed `platform_profile` itself (Fn+F5 on ASUS, Fn+Q on Lenovo).
    pub fn platform_changed(&mut self, value: &str) -> bool {
        match Profile::from_platform(value) {
            Some(p) if p != self.effective() => {
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

    /// Write (or remove) the session environment that keeps KWin off the dGPU. Takes effect at
    /// the next login. Only written when the stable iGPU symlink exists: pointing KWin at a
    /// missing device would leave the session without a display.
    pub fn sync_compositor_env(&self) {
        const FILE: &str = "/etc/environment.d/50-tspmo-kwin.conf";
        const IGPU: &str = "/dev/dri/tspmo-igpu";
        let path = self.root.path(FILE);
        let wanted = self.compositor_igpu_only && self.root.exists(IGPU);
        if wanted {
            let text = format!(
                "# Managed by tspmod (\"desktop on iGPU only\"). Keeps KWin off the NVIDIA GPU so it can\n\
                 # be powered off instantly. Turn off with: tspmo gpu desktop-all-gpus\nKWIN_DRM_DEVICES={IGPU}\n"
            );
            if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(&path, text) {
                    eprintln!("tspmod: can't write {FILE}: {e}");
                }
            }
        } else if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
    }

    pub fn gpu_status(&self) -> gpu::Status {
        gpu::status(&self.root, self.boot_mux)
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
        if let Some(b) = get_b("compositor_igpu_only") {
            self.compositor_igpu_only = b;
        }
        let t = &mut self.tunables;
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
             profile_ac={}\nprofile_battery={}\ngpu_policy={}\ncompositor_igpu_only={}\n\
             saver_no_turbo_on_battery={}\naspm_on_battery={}\n\
             audio_power_save_on_battery={}\nlazy_writeback_on_battery={}\n",
            self.profile_ac.as_str(),
            self.profile_battery.as_str(),
            self.gpu_policy.as_str(),
            self.compositor_igpu_only,
            t.saver_no_turbo_on_battery,
            t.aspm_on_battery,
            t.audio_power_save_on_battery,
            t.lazy_writeback_on_battery,
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
