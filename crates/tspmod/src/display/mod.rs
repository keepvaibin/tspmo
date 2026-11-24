//! Display control for the laptop panel: refresh rate, flicker-free dimming, colour.
//!
//! State lives in its own mutex (never the power daemon's): changes go through the user's
//! compositor, which takes tens of milliseconds, and a query must never wait for that.
//! Nothing here polls. Work starts from a D-Bus call, a power event, or a session event.

pub mod color;
pub mod dbus;
pub mod events;
pub mod flicker;

pub use events::start;

use crate::session;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tspmo_core::display::{self, kscreen, refresh};
use tspmo_sys::SysRoot;

pub type Shared = Arc<Mutex<State>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tri {
    /// Decide from the panel: on for OLED, off for LCD.
    #[default]
    Auto,
    On,
    Off,
}

impl Tri {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "auto" => Some(Self::Auto),
            "on" | "true" | "1" => Some(Self::On),
            "off" | "false" | "0" => Some(Self::Off),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct State {
    pub root: SysRoot,
    pub panel: Option<display::Panel>,
    pub oled: bool,
    pub policy: refresh::Policy,
    /// Rates at the current resolution, as last read from the compositor.
    pub rates: Vec<u32>,
    pub hz: f64,
    /// What the power daemon last told us (see [`power_changed`]).
    pub on_battery: bool,
    pub eco_plus: bool,
    pub flicker: Tri,
    pub floor_pct: u32,
    pub color: color::Settings,
    pub flicker_state: flicker::Runtime,
    /// A message for the user after a failed change.
    pub message: String,
    file: PathBuf,
    pub state_dir: PathBuf,
    /// Talk to a compositor (off in tests, where there is no desktop).
    pub compositor: bool,
}

impl State {
    pub fn new(root: SysRoot, on_battery: bool) -> Self {
        let dir = std::env::var_os("STATE_DIRECTORY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/tspmo"));
        Self::with_state_dir(root, on_battery, dir)
    }

    pub fn with_state_dir(root: SysRoot, on_battery: bool, dir: PathBuf) -> Self {
        let panel = display::internal_panel(&root);
        let dmi = tspmo_sys::dmi::Dmi::read(&root);
        let oled = display::edid::is_oled(panel.as_ref().and_then(|p| p.edid.as_ref()), dmi.model());
        let mut s = Self {
            root,
            panel,
            oled,
            policy: refresh::Policy::Auto,
            rates: Vec::new(),
            hz: 0.0,
            on_battery,
            eco_plus: false,
            flicker: Tri::Auto,
            floor_pct: display::dimming::DEFAULT_FLOOR_PCT,
            color: color::Settings::default(),
            flicker_state: flicker::Runtime::default(),
            message: String::new(),
            file: dir.join("display"),
            state_dir: dir,
            compositor: true,
        };
        s.load();
        s
    }

    /// Is flicker-free dimming in force?
    pub fn flicker_free(&self) -> bool {
        match self.flicker {
            Tri::Auto => self.oled,
            Tri::On => true,
            Tri::Off => false,
        }
    }

    pub fn connector(&self) -> Option<&str> {
        self.panel.as_ref().map(|p| p.connector.as_str())
    }

    fn load(&mut self) {
        let Ok(text) = std::fs::read_to_string(&self.file) else {
            return;
        };
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let v = v.trim();
            match k.trim() {
                "refresh" => self.policy = refresh::Policy::parse(v).unwrap_or(self.policy),
                "flicker_free" => self.flicker = Tri::parse(v).unwrap_or(self.flicker),
                "level" => {
                    if let Ok(n) = v.parse::<f64>() {
                        self.flicker_state.level = n.clamp(1.0, 100.0);
                    }
                }
                "flicker_floor" => {
                    self.floor_pct = v.parse().map_or(self.floor_pct, |n: u32| n.clamp(1, 100));
                }
                _ => self.color.load(k.trim(), v),
            }
        }
        // What the backlight and dimming were at that level, to recognise it after a restart.
        let split = if self.flicker_free() {
            display::dimming::split(self.flicker_state.level, self.floor_pct)
        } else {
            display::dimming::split(self.flicker_state.level, 1)
        };
        self.flicker_state.written_pct = split.backlight_pct;
        self.flicker_state.dimming_pct = split.dimming_pct;
    }

    pub fn save(&self) {
        let text = format!(
            "# tspmod display settings\nrefresh={}\nflicker_free={}\nflicker_floor={}\nlevel={:.1}\n{}",
            self.policy.as_str(),
            self.flicker.as_str(),
            self.floor_pct,
            self.flicker_state.level,
            self.color.save(),
        );
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.file.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }
}

pub fn lock(s: &Shared) -> std::sync::MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

// --- compositor access ----------------------------------------------------------------------

/// Run `kscreen-doctor args...` in the user's session.
pub fn kscreen_doctor(root: &SysRoot, args: &[&str]) -> Result<String, String> {
    let out = session::run_in_session(root, "kscreen-doctor", args)
        .ok_or_else(|| "no desktop session".to_owned())?;
    if out.ok {
        Ok(out.stdout)
    } else {
        Err(format!(
            "kscreen-doctor: {}",
            out.stderr.lines().next().unwrap_or("failed")
        ))
    }
}

/// The compositor's view of the internal panel.
pub fn read_output(root: &SysRoot, connector: Option<&str>) -> Result<kscreen::Output, String> {
    let text = kscreen_doctor(root, &["-o"])?;
    let outs = kscreen::parse_outputs(&text);
    outs.into_iter()
        .find(|o| connector.map_or(o.is_internal(), |c| o.name == c))
        .ok_or_else(|| "panel not found".to_owned())
}

// --- refresh rate ---------------------------------------------------------------------------

/// Bring the panel to the refresh rate its policy wants now. Safe to call at any time and any
/// number of times: it does nothing when the panel is already there.
pub fn apply_refresh(shared: &Shared) -> Result<(), String> {
    let (root, connector, target) = {
        let s = lock(shared);
        (
            s.root.clone(),
            s.connector().map(str::to_owned),
            s.policy.target(s.on_battery, s.eco_plus),
        )
    };
    let out = read_output(&root, connector.as_deref())?;
    let want = refresh::pick(&out, target).map(|m| m.id.clone());
    if let Some(id) = want {
        kscreen_doctor(&root, &[&kscreen::set_mode(&out.name, &id)])?;
    }
    let out = if want_changed(&out, target) {
        read_output(&root, connector.as_deref())?
    } else {
        out
    };
    let mut s = lock(shared);
    s.rates = refresh::rates(&out);
    s.hz = out.current_mode().map_or(0.0, |m| m.hz);
    Ok(())
}

fn want_changed(out: &kscreen::Output, target: refresh::Target) -> bool {
    refresh::pick(out, target).is_some()
}
