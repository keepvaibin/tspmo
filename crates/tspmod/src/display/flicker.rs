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
use std::sync::OnceLock;
use std::time::Instant;
use tspmo_core::display::dimming;
use tspmo_sys::{SysRoot, backlight};

/// `TSPMOD_TRACE_DIM=1`: log every backlight read and write and profile load with a timestamp
/// (the sequence one key press produces; see docs/display.md).
pub fn trace(what: &str) {
    static ON: OnceLock<Option<Instant>> = OnceLock::new();
    let start = ON.get_or_init(|| std::env::var_os("TSPMOD_TRACE_DIM").map(|_| Instant::now()));
    if let Some(t) = start {
        eprintln!(
            "tspmod: dim {:>8.1} ms  {what}",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
}

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
        for b in all.iter().filter(|b| !b.is_nvidia()) {
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

/// The last backlight value (percent) the daemon wrote and when: for a moment after the write,
/// the backlight showing it was us. Later, the same value is a change by someone else that
/// lands exactly there (KDE choosing the floor itself from below it).
static OWN: std::sync::Mutex<(f64, Option<Instant>)> = std::sync::Mutex::new((-1.0, None));

/// How long after our own write the backlight showing that value is taken as our echo.
const ECHO: std::time::Duration = std::time::Duration::from_millis(250);

fn write_backlight(root: &SysRoot, b: &backlight::Backlight, pct: f64) -> Result<(), String> {
    let raw = (pct / 100.0 * f64::from(b.max))
        .round()
        .clamp(1.0, f64::from(b.max)) as u32;
    *OWN.lock().unwrap_or_else(|p| p.into_inner()) = (
        f64::from(raw) * 100.0 / f64::from(b.max.max(1)),
        Some(Instant::now()),
    );
    trace(&format!("write backlight {raw}"));
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
    let before = lock(shared).flicker_state;
    let record = |r: Runtime| lock(shared).flicker_state = r;
    let next = Runtime {
        level: level.clamp(1.0, 100.0),
        written_pct: split.backlight_pct,
        dimming_pct: split.dimming_pct,
    };
    // The dimming goes through the compositor; skip it when nothing is (or was) dimmed. It
    // goes first: if the compositor refuses, the backlight must stay where it is, or the
    // screen would be left brighter than asked with nothing to darken it.
    record(next);
    if before.level != next.level {
        lock(shared).save();
    }
    // The profile for this dimming is already in force (KDE's steps often end on the same one):
    // loading it again is 50 ms of compositor work for nothing.
    let in_force = {
        let s = lock(shared);
        s.color.icc_loaded
            && s.color.dimmed
            && before.dimming_pct.round() == next.dimming_pct.round()
            && split.dimming_pct < 99.5
    };
    if (split.dimming_pct < 99.5 || lock(shared).color.dimmed) && !in_force {
        trace(&format!("load profile, dimming {:.0}%", split.dimming_pct));
        if let Err(e) = color::apply_profile(shared) {
            record(before);
            return Err(e);
        }
        trace("profile loaded");
    }
    write_backlight(&root, &dev, split.backlight_pct)?;
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
    // The backlight where the daemon left it: still the level it remembers. Anywhere else:
    // somebody changed it meanwhile (KDE restoring its own value at login), and that wins.
    let hw = backlight_pct(&dev);
    let level = if (hw - known.written_pct).abs() < 0.5 {
        known.level
    } else {
        hw
    };
    apply_level(shared, level)
}

/// Something else changed the backlight (KDE's slider or keys, power management): that is the
/// new level. KDE keeps its own value rather than re-reading the backlight the daemon holds at
/// the floor, so its next key step starts from the level, not from the floor.
#[cfg(test)]
pub fn follow(shared: &Shared) -> Result<(), String> {
    let (root, connector, written, on) = {
        let s = lock(shared);
        (
            s.root.clone(),
            s.connector().map(str::to_owned),
            s.flicker_state.written_pct,
            s.flicker_free(),
        )
    };
    let dev = device(&root, connector.as_deref()).ok_or("no backlight")?;
    let now = backlight_pct(&dev);
    if (now - written).abs() < 0.3 {
        return Ok(()); // our own write
    }
    if on {
        trace(&format!("backlight moved to {now:.1}% (we wrote {written:.1}%)"));
        // KDE just wrote a value below the floor: the screen is darker than asked, flickering,
        // and stays so until the compositor call below returns. Put the backlight back at
        // once (a plain write, microseconds); the picture is darkened right after, so the
        // screen only ever gets darker, never darker and then back.
        let floor = lock(shared).floor_pct;
        if let Some(pin) = pin_for(now, floor) {
            write_backlight(&root, &dev, pin)?;
        }
        return apply_level(shared, now);
    }
    // Plain backlight: just keep track.
    lock(shared).flicker_state = Runtime {
        level: now.clamp(1.0, 100.0),
        written_pct: now,
        dimming_pct: 100.0,
    };
    Ok(())
}

/// How long KDE's steps must pause before the level is checked against KDE's own.
const QUIET: std::time::Duration = std::time::Duration::from_millis(350);

/// KDE's brightness service for the internal panel: its object path and its maximum, found once.
fn kde_display(root: &SysRoot) -> Option<&'static (String, f64)> {
    static DISPLAY: OnceLock<Option<(String, f64)>> = OnceLock::new();
    DISPLAY
        .get_or_init(|| {
            const SVC: &str = "org.kde.ScreenBrightness";
            let iface = "org.kde.ScreenBrightness.Display";
            let call = |args: &[&str]| -> Option<String> {
                let mut a = vec!["--user", "--timeout=2", "get-property", SVC];
                a.extend_from_slice(args);
                let out = session::run_in_session(root, "busctl", &a)?;
                out.ok.then_some(out.stdout)
            };
            let names = call(&["/org/kde/ScreenBrightness", SVC, "DisplaysDBusNames"])?;
            for name in names.split('"').skip(1).step_by(2) {
                let path = format!("/org/kde/ScreenBrightness/{name}");
                if !call(&[&path, iface, "IsInternal"])?.contains("true") {
                    continue;
                }
                let max: f64 = call(&[&path, iface, "MaxBrightness"])?
                    .split_whitespace()
                    .nth(1)?
                    .parse()
                    .ok()?;
                return (max > 0.0).then_some((path, max));
            }
            None
        })
        .as_ref()
}

/// What KDE's brightness service holds for the internal panel, percent: the level the user chose
/// with the keys or the slider. KDE sets it to the *target* at once and then eases the backlight
/// towards it in steps, so it is known before the steps arrive (and the steps themselves, which
/// KDE shapes by what it sees us write, are not a reliable way to learn it).
fn kde_level(root: &SysRoot) -> Option<f64> {
    let (path, max) = kde_display(root)?;
    let out = session::run_in_session(
        root,
        "busctl",
        &[
            "--user",
            "--timeout=2",
            "get-property",
            "org.kde.ScreenBrightness",
            path,
            "org.kde.ScreenBrightness.Display",
            "Brightness",
        ],
    )?;
    let v: f64 = out
        .ok
        .then_some(out.stdout)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((v * 100.0 / max).clamp(1.0, 100.0))
}

/// Where the backlight must be put back to when something wrote `now` percent: the floor when
/// `now` is below it, nothing otherwise.
fn pin_for(now: f64, floor: u32) -> Option<f64> {
    let pin = dimming::split(now, floor).backlight_pct;
    (now < pin - 0.3).then_some(pin)
}

/// Follow the backlight. Two threads, because the two jobs have different deadlines:
///
/// * `backlight` sleeps in `poll(2)` (the kernel signals changes of `actual_brightness` with
///   POLLPRI, so it costs nothing idle). When something other than the daemon moved the
///   backlight, it records the value as the new level and, if that is below the floor, writes
///   the floor back at once. KDE eases in steps, so this happens for every step, and the screen
///   spends microseconds, not the length of a compositor call, below the floor.
/// * `backlight-apply` then loads the dimming for the newest level (about 50 ms), and the
///   screen only ever gets darker on the way down.
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
    // Where the backlight is at start is not a change: whoever left it there (the daemon, at
    // the floor, or KDE at login) is handled by `reapply`.
    {
        let mut own = OWN.lock().unwrap_or_else(|p| p.into_inner());
        if own.0 < 0.0 {
            own.0 = backlight_pct(&dev);
        }
    }
    let pending: std::sync::Arc<std::sync::Mutex<Option<f64>>> = Default::default();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    {
        let (shared, conn, pending) = (shared.clone(), conn.clone(), pending.clone());
        let root_for_kde = root.clone();
        let _ = std::thread::Builder::new()
            .name("backlight-apply".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let apply = |level: f64| {
                    let _g = super::events::COMPOSITOR
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    if let Err(e) = apply_level(&shared, level) {
                        eprintln!("tspmod: brightness: {e}");
                    }
                    super::dbus::emit_changed_blocking(&conn, &shared);
                };
                while rx.recv().is_ok() {
                    // Keep applying the newest level while KDE's steps keep coming.
                    loop {
                        if let Some(seen) = pending.lock().unwrap_or_else(|p| p.into_inner()).take() {
                            // The target KDE is easing towards, not the step it is at.
                            apply(kde_level(&root_for_kde).unwrap_or(seen));
                        }
                        if rx.recv_timeout(QUIET).is_err() {
                            break;
                        }
                    }
                    // KDE's last write can land exactly on the pinned floor, where the backlight
                    // does not change and nothing is seen: ask KDE what it thinks the level is.
                    let level = lock(&shared).flicker_state.level;
                    if let Some(kde) = kde_level(&root_for_kde) {
                        if (kde - level).abs() >= 1.0 {
                            trace(&format!("KDE says {kde:.0}%, we had {level:.0}%"));
                            apply(kde);
                        }
                    }
                }
            });
    }
    let _ = std::thread::Builder::new()
        .name("backlight".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            use rustix::event::{PollFd, PollFlags, poll};
            let mut buf = String::with_capacity(16);
            let mut known = {
                let s = lock(&shared);
                (s.flicker_free(), s.floor_pct)
            };
            loop {
                buf.clear();
                if file.seek(SeekFrom::Start(0)).is_err() || file.read_to_string(&mut buf).is_err() {
                    return;
                }
                let now = buf
                    .trim()
                    .parse::<f64>()
                    .map_or(-1.0, |v| v * 100.0 / f64::from(dev.max.max(1)));
                let (own, at) = *OWN.lock().unwrap_or_else(|p| p.into_inner());
                let echo = (now - own).abs() < 0.3 && at.is_none_or(|t| t.elapsed() < ECHO);
                // The state lock can be held for a while by the thread that loads profiles (it
                // builds a new one under it): never wait for it here, the pin is time-critical.
                if let Ok(s) = shared.try_lock() {
                    known = (s.flicker_free(), s.floor_pct);
                }
                if known.0 && now >= 0.0 && !echo {
                    trace(&format!("backlight moved to {now:.1}% (we wrote {own:.1}%)"));
                    if let Some(pin) = pin_for(now, known.1) {
                        if let Err(e) = write_backlight(&root, &dev, pin) {
                            eprintln!("tspmod: brightness: {e}");
                        }
                    }
                    *pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(now);
                    let _ = tx.send(());
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

    #[test]
    fn below_the_floor_the_backlight_is_pinned_at_the_floor() {
        assert_eq!(pin_for(25.0, 30), Some(30.0));
        assert_eq!(pin_for(1.0, 30), Some(30.0));
        assert_eq!(pin_for(30.0, 30), None);
        assert_eq!(pin_for(55.0, 30), None);
    }
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
        s.compositor = false;
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
    fn below_the_floor_the_backlight_is_held_at_the_floor() {
        let (d, s) = fixture("below");
        external(&d, 77); // KDE: 15%
        follow(&s).unwrap();
        let k = lock(&s).flicker_state;
        assert_eq!(k.level.round(), 15.0);
        assert_eq!(raw(&d), (0.30f64 * 512.0).round() as u32); // held at the floor
        assert!((k.dimming_pct - 50.0).abs() < 1.0);
        // Our own write coming back as an event changes nothing.
        follow(&s).unwrap();
        assert_eq!(lock(&s).flicker_state, k);
        // KDE steps from its own value (15%), down to 10%: the level follows.
        external(&d, 51);
        follow(&s).unwrap();
        assert_eq!(lock(&s).flicker_state.level.round(), 10.0);
        // Back above the floor: no dimming, the backlight is the level.
        external(&d, 256);
        follow(&s).unwrap();
        let k = lock(&s).flicker_state;
        assert_eq!((k.level.round(), k.dimming_pct), (50.0, 100.0));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn level_survives_a_restart() {
        let (d, s) = fixture("restart");
        external(&d, 77);
        follow(&s).unwrap();
        lock(&s).save();
        // A new daemon finds the backlight where the old one left it: the saved level stands.
        let mut s2 = State::with_state_dir(SysRoot::new(&d), false, d.join("state"));
        s2.compositor = false;
        s2.flicker = Tri::On;
        assert_eq!(s2.flicker_state.level.round(), 15.0);
        let s2 = Arc::new(Mutex::new(s2));
        reapply(&s2).unwrap();
        assert_eq!(lock(&s2).flicker_state.level.round(), 15.0);
        // Changed behind its back (KDE restoring 80% at login): that wins.
        external(&d, 410);
        reapply(&s2).unwrap();
        assert_eq!(lock(&s2).flicker_state.level.round(), 80.0);
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
