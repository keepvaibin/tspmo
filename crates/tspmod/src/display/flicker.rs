//! Flicker-free dimming.
//!
//! The panel's backlight never goes below the floor; the rest of the way down, the image is
//! darkened in the compositor (a `vcgt` ramp in the output's colour profile, see
//! [`super::color::apply_profile`]).
//!
//! KDE's brightness keys and slider write the backlight directly, so the daemon follows the
//! backlight: when something other than the daemon changes it, that is the new level. Below the
//! floor the daemon holds the backlight at the floor, so KDE's next step starts from the floor:
//! steps below it are read as relative to the level (see [`follow`]).

use super::{Shared, State, Tri, color, lock};
use crate::session;
use std::io::{Read, Seek, SeekFrom};
use tspmo_core::display::dimming;
use tspmo_sys::{SysRoot, backlight};

/// The largest backlight move, in percent, read as a key step rather than a slider jump.
const MAX_STEP_PCT: f64 = 10.0;

/// The daemon's view of brightness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Runtime {
    /// What the user sees: 1..=100, the product of backlight and dimming.
    pub level: f64,
    /// Backlight percent the daemon last wrote (to recognise its own writes).
    pub written_pct: f64,
    /// Dimming percent in force (100 = none).
    pub dimming_pct: f64,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            level: 100.0,
            written_pct: 100.0,
            dimming_pct: 100.0,
        }
    }
}

/// `(backlight %, dimming %)` as last applied.
pub fn current_split(s: &State) -> (f64, f64) {
    (s.flicker_state.written_pct, s.flicker_state.dimming_pct)
}

/// The panel's backlight device (the iGPU's; the NVIDIA one controls nothing).
fn device(root: &SysRoot, connector: Option<&str>) -> Option<backlight::Backlight> {
    let all = backlight::list(root);
    // A backlight under the connector's own sysfs directory is the panel's.
    if let Some(c) = connector {
        for b in &all {
            if !root
                .glob(&format!("/sys/class/drm/card*-{c}/{}", b.name))
                .is_empty()
            {
                return Some(b.clone());
            }
        }
    }
    all.into_iter()
        .filter(|b| !b.is_nvidia())
        .max_by_key(|b| b.kind == "raw")
}

fn backlight_pct(b: &backlight::Backlight) -> f64 {
    f64::from(b.actual.unwrap_or(b.brightness)) * 100.0 / f64::from(b.max.max(1))
}

fn write_backlight(root: &SysRoot, b: &backlight::Backlight, pct: f64) -> Result<(), String> {
    let raw = (pct / 100.0 * f64::from(b.max))
        .round()
        .clamp(1.0, f64::from(b.max)) as u32;
    root.write(
        format!("{}/{}/brightness", backlight::CLASS, b.name),
        &raw.to_string(),
    )
    .map_err(|e| format!("backlight: {e}"))
}

/// Apply `level`: split it, write the backlight, load the dimming.
fn apply_level(shared: &Shared, level: f64) -> Result<(), String> {
    let (root, connector, on, floor) = {
        let s = lock(shared);
        (
            s.root.clone(),
            s.connector().map(str::to_owned),
            s.flicker_free(),
            s.floor_pct,
        )
    };
    let dev = device(&root, connector.as_deref()).ok_or("no backlight")?;
    let split = if on {
        dimming::split(level, floor)
    } else {
        dimming::Split {
            backlight_pct: level.clamp(1.0, 100.0),
            dimming_pct: 100.0,
        }
    };
    {
        let mut s = lock(shared);
        s.flicker_state = Runtime {
            level: level.clamp(1.0, 100.0),
            written_pct: split.backlight_pct,
            dimming_pct: split.dimming_pct,
        };
    }
    write_backlight(&root, &dev, split.backlight_pct)?;
    // The dimming goes through the compositor; skip it when nothing is (or was) dimmed.
    let needs_profile = split.dimming_pct < 99.5 || lock(shared).color.dimmed;
    if needs_profile {
        color::apply_profile(shared)?;
    }
    Ok(())
}

/// A brightness the user asked for through tspmo.
pub fn set_level(shared: &Shared, level: f64) -> Result<String, String> {
    apply_level(shared, level)?;
    let l = lock(shared).flicker_state.level.round() as u8;
    session::osd("video-display-brightness", "Brightness", Some(l));
    Ok(format!("Brightness {l}%"))
}

pub fn set_mode(shared: &Shared, t: Tri) -> Result<String, String> {
    {
        let mut s = lock(shared);
        s.flicker = t;
        s.save();
    }
    reapply(shared)?;
    let s = lock(shared);
    Ok(format!(
        "Flicker-free dimming {}",
        if s.flicker_free() { "on" } else { "off" }
    ))
}

pub fn set_floor(shared: &Shared, pct: u32) -> Result<String, String> {
    {
        let mut s = lock(shared);
        s.floor_pct = pct;
        s.save();
    }
    reapply(shared)?;
    Ok(format!("Floor {pct}%"))
}

/// Re-establish the current level (after resume, login or a setting change). The level is
/// whatever the backlight and dimming add up to now.
pub fn reapply(shared: &Shared) -> Result<(), String> {
    let (root, connector, known) = {
        let s = lock(shared);
        (s.root.clone(), s.connector().map(str::to_owned), s.flicker_state)
    };
    let dev = device(&root, connector.as_deref()).ok_or("no backlight")?;
    let level = if known.dimming_pct < 99.5 || known.level != 100.0 {
        known.level
    } else {
        backlight_pct(&dev)
    };
    apply_level(shared, level)
}

/// Something else changed the backlight (KDE's slider or keys, power management): work out
/// the new level. Returns it when it differs from what the daemon last wrote.
pub fn follow(shared: &Shared) -> Result<(), String> {
    let (root, connector, k, floor, on) = {
        let s = lock(shared);
        (
            s.root.clone(),
            s.connector().map(str::to_owned),
            s.flicker_state,
            s.floor_pct,
            s.flicker_free(),
        )
    };
    let dev = device(&root, connector.as_deref()).ok_or("no backlight")?;
    let now = backlight_pct(&dev);
    if (now - k.written_pct).abs() < 0.3 {
        return Ok(()); // our own write
    }
    // Normally the backlight is the level. While dimmed it sits at the floor, so KDE's next
    // key step starts from the floor, not from the level: small moves are steps from the level.
    let delta = now - k.written_pct;
    let level = if on && k.level < f64::from(floor) && delta.abs() <= MAX_STEP_PCT {
        (k.level + delta).clamp(dimming::MIN_LEVEL_PCT, 100.0)
    } else {
        now
    };
    apply_level(shared, level)
}

/// Block until the backlight changes, call [`follow`] each time. The kernel signals changes of
/// `actual_brightness` with POLLPRI, so the thread sleeps in `poll(2)` and costs nothing idle.
pub fn watch(conn: zbus::Connection, shared: Shared) {
    let (root, connector) = {
        let s = lock(&shared);
        (s.root.clone(), s.connector().map(str::to_owned))
    };
    let Some(dev) = device(&root, connector.as_deref()) else {
        return;
    };
    let path = root.path(format!("{}/{}/actual_brightness", backlight::CLASS, dev.name));
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("backlight".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            use rustix::event::{PollFd, PollFlags, poll};
            let mut buf = String::with_capacity(16);
            loop {
                buf.clear();
                if file.seek(SeekFrom::Start(0)).is_err() || file.read_to_string(&mut buf).is_err() {
                    return;
                }
                if lock(&shared).flicker_free() {
                    let _g = super::events::COMPOSITOR
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    if let Err(e) = follow(&shared) {
                        eprintln!("tspmod: brightness: {e}");
                    }
                    super::dbus::emit_changed_blocking(&conn, &shared);
                }
                let mut fds = [PollFd::new(&file, PollFlags::PRI | PollFlags::ERR)];
                if poll(&mut fds, None).is_err() {
                    return;
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn fixture(tag: &str) -> (PathBuf, Shared) {
        let d = std::env::temp_dir().join(format!("tspmo-flicker-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let b = d.join("sys/class/backlight/intel_backlight");
        std::fs::create_dir_all(&b).unwrap();
        for (f, v) in [
            ("brightness", "512"),
            ("actual_brightness", "512"),
            ("max_brightness", "512"),
            ("type", "raw"),
        ] {
            std::fs::write(b.join(f), v).unwrap();
        }
        let mut s = State::with_state_dir(SysRoot::new(&d), false, d.join("state"));
        s.flicker = Tri::On;
        s.floor_pct = 30;
        (d, Arc::new(Mutex::new(s)))
    }

    fn raw(d: &std::path::Path) -> u32 {
        std::fs::read_to_string(d.join("sys/class/backlight/intel_backlight/brightness"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn external(d: &std::path::Path, raw: u32) {
        for f in ["brightness", "actual_brightness"] {
            std::fs::write(
                d.join("sys/class/backlight/intel_backlight").join(f),
                raw.to_string(),
            )
            .unwrap();
        }
    }

    #[test]
    fn above_the_floor_follows_the_backlight() {
        let (d, s) = fixture("above");
        external(&d, 256); // KDE sets 50%
        follow(&s).unwrap();
        assert_eq!(lock(&s).flicker_state.level.round(), 50.0);
        assert_eq!(raw(&d), 256);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn below_the_floor_the_backlight_is_held_and_steps_accumulate() {
        let (d, s) = fixture("below");
        // Sidestep the compositor: no desktop in tests, so profile loading fails after the
        // backlight is already written and the state recorded, which is what is checked here.
        external(&d, 77); // KDE: 15%
        let _ = follow(&s);
        let k = lock(&s).flicker_state;
        assert_eq!(k.level.round(), 15.0);
        assert_eq!(raw(&d), (0.30f64 * 512.0).round() as u32); // held at the floor
        assert!((k.dimming_pct - 50.0).abs() < 1.0);
        // KDE reads the floor back and steps down by 5%: the level follows.
        let floor_raw = raw(&d);
        external(&d, floor_raw - 26);
        let _ = follow(&s);
        assert_eq!(lock(&s).flicker_state.level.round(), 10.0);
        // And back up.
        external(&d, raw(&d) + 26);
        let _ = follow(&s);
        assert_eq!(lock(&s).flicker_state.level.round(), 15.0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn own_writes_are_ignored_and_lcd_is_untouched() {
        let (d, s) = fixture("own");
        lock(&s).flicker = Tri::Off;
        external(&d, 100);
        let _ = follow(&s);
        // Off: the level just tracks the backlight, which is left alone.
        assert_eq!(raw(&d), 100);
        assert!((lock(&s).flicker_state.dimming_pct - 100.0).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&d);
    }
}
