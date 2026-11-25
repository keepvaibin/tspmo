//! Fn+F5 has exactly one owner: tspmod.
//!
//! The kernel (asus-wmi) also steps `platform_profile` on the fan key. That step is not a
//! user's choice: tspmod computes the next mode from its own, applies it once, shows one OSD,
//! and makes the hardware end in the state it chose. The watcher in `watch.rs` ignores
//! `platform_profile` changes that fall near a key press.
//!
//! [`Gate`] is the timing rule (pure, tested with plain durations). [`Hub`] holds it with the
//! kernel-step bookkeeping that lets the handler wait for the kernel's step (so that, usually,
//! the hardware is already where tspmod wants it and nothing is written twice).

use crate::dbus::{self, Shared};
use crate::quiet::{Clock, Real};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// Two presses closer than this are one (a key reported by two devices).
pub const DUPLICATE: Duration = Duration::from_millis(50);
/// A `platform_profile` change this long before a press is the press's.
pub const BEFORE: Duration = Duration::from_millis(500);
/// ... and this long after it.
pub const AFTER: Duration = Duration::from_millis(500);
/// How long the handler waits for the kernel's step before it applies by itself (the Fn-lock
/// variant of the key is not stepped by the kernel at all).
pub const KERNEL_WAIT: Duration = Duration::from_millis(250);
/// How long the watcher holds a change back to see whether a key press explains it (the key
/// event can arrive after the sysfs notification).
pub const LOOKAHEAD: Duration = Duration::from_millis(200);

/// When the last accepted press happened, and what it covers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    last: Option<Duration>,
}

impl Gate {
    /// A press at `now`. `false` when it duplicates the previous one.
    pub fn press(&mut self, now: Duration) -> bool {
        if self.last.is_some_and(|l| now.saturating_sub(l) < DUPLICATE) {
            return false;
        }
        self.last = Some(now);
        true
    }

    /// Does a `platform_profile` change at `at` belong to a press?
    pub fn covers(&self, at: Duration) -> bool {
        self.last
            .is_some_and(|l| at >= l.saturating_sub(BEFORE) && at <= l + AFTER)
    }

    /// Is `press` (the time `press` returned true for) still the latest one?
    pub fn is_latest(&self, press: Duration) -> bool {
        self.last == Some(press)
    }
}

#[derive(Debug, Default)]
struct Hub {
    gate: Gate,
    /// Changes of `platform_profile` the daemon did not make, counted, and when the last was.
    steps: u64,
    last_step: Option<Duration>,
}

static HUB: Mutex<Hub> = Mutex::new(Hub {
    gate: Gate { last: None },
    steps: 0,
    last_step: None,
});
static STEPPED: Condvar = Condvar::new();

fn hub() -> std::sync::MutexGuard<'static, Hub> {
    HUB.lock().unwrap_or_else(|p| p.into_inner())
}

/// The watcher saw `platform_profile` move away from the mode in force, at `at`.
pub fn kernel_stepped(at: Duration) {
    let mut h = hub();
    h.steps += 1;
    h.last_step = Some(at);
    STEPPED.notify_all();
}

/// Does a change at `at` belong to a fan-key press?
pub fn covers(at: Duration) -> bool {
    hub().gate.covers(at)
}

/// The fan key was pressed (any device, or `tspmo debug fan-key`). Runs the one handler.
pub fn press(conn: zbus::Connection, shared: Shared, source: &str) {
    let now = Real.now();
    let (press_at, steps, stepped) = {
        let mut h = hub();
        if !h.gate.press(now) {
            eprintln!("tspmod: fan key from {source}: duplicate, ignored");
            return;
        }
        let stepped = h.last_step.is_some_and(|t| now.saturating_sub(t) < BEFORE && t <= now);
        (now, h.steps, stepped)
    };
    eprintln!("tspmod: fan key from {source} at {} ms", now.as_millis());
    let source = source.to_owned();
    let _ = std::thread::Builder::new().name("fan-key".into()).spawn(move || {
        // The kernel's step comes from the same key: let it land first, so that the mode
        // tspmod then writes is usually the one the hardware already has.
        if !stepped {
            let h = hub();
            let _ = STEPPED.wait_timeout_while(h, KERNEL_WAIT, |h| h.steps == steps);
        }
        let next = {
            let mut d = shared.lock().unwrap_or_else(|p| p.into_inner());
            d.cycle()
        };
        eprintln!(
            "tspmod: fan key from {source}: {} (+{} ms)",
            next.as_str(),
            Real.now().saturating_sub(press_at).as_millis()
        );
        dbus::osd_mode(next);
        zbus::block_on(dbus::emit_changed(&conn, &shared));

        // Once the kernel's step (suppressed by the watcher) is surely over, the hardware must
        // be where tspmod put it. A later press owns this check, not this one.
        Real.sleep(AFTER + Duration::from_millis(50));
        if !hub().gate.is_latest(press_at) {
            return;
        }
        let wrote = shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .reassert_thermal();
        if wrote {
            eprintln!("tspmod: fan key: the kernel's step differed, rewrote the thermal mode");
            zbus::block_on(dbus::emit_changed(&conn, &shared));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_second_report_of_the_same_press_is_dropped_but_a_real_second_press_is_not() {
        let mut g = Gate::default();
        assert!(g.press(ms(1000)));
        assert!(!g.press(ms(1049))); // same key seen by two devices
        assert!(g.press(ms(1050))); // a fast but distinct press
        assert!(g.press(ms(1300)));
    }

    #[test]
    fn the_window_is_half_a_second_either_side_of_a_press() {
        let mut g = Gate::default();
        assert!(!g.covers(ms(100)), "no press yet");
        assert!(g.press(ms(10_000)));
        assert!(g.covers(ms(9_500)), "the sysfs notification can come first");
        assert!(g.covers(ms(10_000)));
        assert!(g.covers(ms(10_500)));
        assert!(!g.covers(ms(9_499)));
        assert!(!g.covers(ms(10_501)), "an external write later is a real change");
    }

    #[test]
    fn rapid_presses_each_extend_the_window_and_only_the_last_one_verifies() {
        let mut g = Gate::default();
        assert!(g.press(ms(1_000)));
        assert!(g.press(ms(1_200)));
        assert!(g.press(ms(1_400)));
        assert!(g.covers(ms(1_900)), "covered by the last press");
        assert!(!g.is_latest(ms(1_000)));
        assert!(g.is_latest(ms(1_400)));
        assert!(!g.covers(ms(2_000)));
    }

    #[test]
    fn a_window_near_the_start_of_the_clock_does_not_underflow() {
        let mut g = Gate::default();
        assert!(g.press(ms(100)));
        assert!(g.covers(ms(0)));
    }
}
