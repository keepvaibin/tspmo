#![allow(dead_code)] // wired up by the work packages in docs/plan.md
//! In-process event bus: features react to each other without sharing state.
//! Subscribers run synchronously on the publishing thread and must be quick
//! (spawn a thread for slow work). No event is ever lost.

use std::sync::{Mutex, OnceLock};
use tspmo_core::power::Profile;

#[derive(Debug, Clone)]
pub enum Event {
    /// A power profile was (re)applied. `eco_plus` is set when Eco+ is the active mode.
    ProfileApplied {
        profile: Profile,
        eco_plus: bool,
        on_battery: bool,
    },
    /// The firmware thermal mode was written later than the profile was applied (Eco+ holds it
    /// back until the GPU switch is over). The EC reloads its mode on a write and drops the fan
    /// curve and power limits written before it: whoever owns such settings writes them again.
    ThermalModeWritten { profile: Profile, on_battery: bool },
    /// AC plugged / unplugged.
    PowerSource { on_battery: bool },
    /// The system resumed from sleep.
    Resumed,
    /// A vendor hotkey (Linux input key code) was pressed.
    Key(u16),
    /// The thermal-mode key (Fn+F5) was pressed on the keyboard.
    FanKey,
}

type Handler = Box<dyn Fn(&Event) + Send + Sync>;
static HANDLERS: OnceLock<Mutex<Vec<Handler>>> = OnceLock::new();

pub fn subscribe(f: impl Fn(&Event) + Send + Sync + 'static) {
    HANDLERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Box::new(f));
}

pub fn publish(e: &Event) {
    if let Some(h) = HANDLERS.get() {
        for f in h.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            f(e);
        }
    }
}
