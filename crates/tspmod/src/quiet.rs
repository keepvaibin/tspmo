//! Keeping the NVIDIA driver unload away from the firmware's ACPI notifications.
//!
//! **The deadlock.** On an NVIDIA open driver (620.x) `rmmod nvidia` (and any PCI unbind or
//! removal of the device) runs `nv_acpi_methods_uninit` while holding the RM API lock, which
//! flushes the ACPI notify workqueue. A pending NVPCF notification (`rm_acpi_nvpcf_notify`,
//! sent by the firmware about every 22 s in Hybrid and right after every `platform_profile`
//! write) sits in that queue and waits for the RM API lock: ABBA, `rmmod` hangs in D state for
//! good and cannot be killed.
//!
//! **What we do about it**, all of it here so it can be tested without hardware:
//!
//! 1. never unload within [`THERMAL_SETTLE`] of a `platform_profile` write ([`note_profile_write`]);
//! 2. unload only while the dGPU's `power/runtime_status` is `suspended`: a notification
//!    resumes it, so a suspended GPU has none pending ([`wait_quiet`] polls that one attribute);
//! 3. run `rmmod` as a child under a watchdog ([`run_watchdog`]): if it does not return, the
//!    daemon gives up on it (the kernel task cannot be killed) and the caller marks the
//!    machine as needing a restart.

use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tspmo_sys::SysRoot;

/// No driver unload this soon after a `platform_profile` write: the EC notifies the driver then.
pub const THERMAL_SETTLE: Duration = Duration::from_secs(30);
/// The longest we wait for the dGPU to be suspended before giving up.
pub const SUSPEND_BOUND: Duration = Duration::from_secs(45);
/// How often the one attribute is read while waiting.
pub const POLL: Duration = Duration::from_millis(150);
/// `rmmod` that takes longer than this is stuck for good.
pub const RMMOD_LIMIT: Duration = Duration::from_secs(8);

/// GpuMessage when the dGPU never went quiet.
pub const BUSY: &str = "NVIDIA GPU busy; try again";

/// Not a failure: the policy stopped wanting the switch while it waited. Never shown.
pub const CANCELLED: &str = "switch cancelled";

/// Longest sleep between looks at the cancel flag while waiting out the thermal window.
const THERMAL_STEP: Duration = Duration::from_millis(250);

/// Monotonic time and sleeping, replaceable in tests.
pub trait Clock {
    /// Time since an arbitrary fixed point.
    fn now(&self) -> Duration;
    fn sleep(&self, d: Duration);
}

pub struct Real;

fn epoch() -> Instant {
    static E: OnceLock<Instant> = OnceLock::new();
    *E.get_or_init(Instant::now)
}

impl Clock for Real {
    fn now(&self) -> Duration {
        epoch().elapsed()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

static LAST_PROFILE_WRITE: Mutex<Option<Duration>> = Mutex::new(None);

/// A `platform_profile` write happened (ours, or the firmware's: Fn+F5).
pub fn note_profile_write() {
    *LAST_PROFILE_WRITE.lock().unwrap_or_else(|p| p.into_inner()) = Some(Real.now());
}

/// When the last `platform_profile` write happened, on [`Real`]'s clock.
pub fn last_profile_write() -> Option<Duration> {
    *LAST_PROFILE_WRITE.lock().unwrap_or_else(|p| p.into_inner())
}

/// How much longer to wait after a thermal-mode write, if at all.
pub fn thermal_remaining(now: Duration, last_write: Option<Duration>) -> Option<Duration> {
    let since = now.saturating_sub(last_write?);
    (since < THERMAL_SETTLE).then(|| THERMAL_SETTLE - since)
}

/// May the driver be unloaded with the dGPU in this runtime-PM state? Only a suspended GPU
/// has no notification being handled. Where the kernel cannot say (`unsupported`, `error`, no
/// attribute) there is nothing to wait for.
pub fn may_unload(runtime_status: Option<&str>) -> bool {
    !matches!(runtime_status, Some("active" | "resuming" | "suspending"))
}

/// Wait out the thermal-mode write only (no GPU involved): used before the switch starts
/// touching the desktop. `Err(CANCELLED)` the moment `cancelled` says so (checked at every step).
pub fn wait_thermal(
    clock: &dyn Clock,
    last_write: &dyn Fn() -> Option<Duration>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    loop {
        if cancelled() {
            return Err(CANCELLED.into());
        }
        let Some(rem) = thermal_remaining(clock.now(), last_write()) else {
            return Ok(());
        };
        clock.sleep(rem.min(THERMAL_STEP));
    }
}

/// Block until it is safe to unload the driver: [`THERMAL_SETTLE`] after the last thermal
/// write, then the dGPU `suspended`. While it is awake only its `power/runtime_status` is read,
/// every [`POLL`], and the unload follows at once. `Err(BUSY)` when it stays awake for
/// [`SUSPEND_BOUND`]; `Err(CANCELLED)` as soon as `cancelled` says so (checked at every step).
pub fn wait_quiet(
    root: &SysRoot,
    addr: &str,
    clock: &dyn Clock,
    last_write: &dyn Fn() -> Option<Duration>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    let status = format!("{}/{addr}/power/runtime_status", tspmo_sys::pci::DEVICES);
    let mut waited = Duration::ZERO;
    loop {
        if cancelled() {
            return Err(CANCELLED.into());
        }
        if let Some(rem) = thermal_remaining(clock.now(), last_write()) {
            clock.sleep(rem.min(THERMAL_STEP));
            continue; // a write during the wait starts the window again
        }
        if may_unload(root.read_opt(&status).as_deref()) {
            return Ok(());
        }
        if waited >= SUSPEND_BOUND {
            return Err(BUSY.into());
        }
        clock.sleep(POLL);
        waited += POLL;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Ran {
    /// The command returned; `true` when it succeeded.
    Exited(bool),
    /// Still running after the limit: given up on, not killed.
    Hung,
}

/// Run a command, quietly, for at most `limit`. A command still running then is left alone (a
/// task stuck in the kernel cannot be killed) and a thread reaps it should it ever return.
pub fn run_watchdog(cmd: &str, args: &[&str], limit: Duration) -> Ran {
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Ran::Exited(false);
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Ran::Exited(s.success()),
            Ok(None) => {}
            Err(_) => return Ran::Exited(false),
        }
        if start.elapsed() >= limit {
            let _ = std::thread::Builder::new()
                .name("rmmod-reaper".into())
                .stack_size(64 * 1024)
                .spawn(move || {
                    let _ = child.wait();
                });
            return Ran::Hung;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    const ADDR: &str = "0000:01:00.0";

    /// A sysroot with one dGPU whose runtime status follows a script on a fake clock.
    struct Fake {
        dir: PathBuf,
        now: Cell<Duration>,
        /// (time it takes effect, status)
        script: RefCell<Vec<(Duration, &'static str)>>,
        reads: Cell<u32>,
    }

    impl Fake {
        fn new(tag: &str, script: Vec<(Duration, &'static str)>) -> Self {
            let dir = std::env::temp_dir().join(format!("tspmod-quiet-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join(format!("sys/bus/pci/devices/{ADDR}/power"))).unwrap();
            let f = Self {
                dir,
                now: Cell::new(Duration::ZERO),
                script: RefCell::new(script),
                reads: Cell::new(0),
            };
            f.apply_script();
            f
        }
        fn status_path(&self) -> PathBuf {
            self.dir
                .join(format!("sys/bus/pci/devices/{ADDR}/power/runtime_status"))
        }
        fn apply_script(&self) {
            let now = self.now.get();
            let s = self
                .script
                .borrow()
                .iter()
                .rev()
                .find(|(t, _)| *t <= now)
                .map(|(_, s)| *s);
            if let Some(s) = s {
                std::fs::write(self.status_path(), format!("{s}\n")).unwrap();
            }
        }
        fn root(&self) -> SysRoot {
            SysRoot::new(&self.dir)
        }
    }
    impl Clock for Fake {
        fn now(&self) -> Duration {
            self.now.get()
        }
        fn sleep(&self, d: Duration) {
            self.now.set(self.now.get() + d);
            self.reads.set(self.reads.get() + 1);
            self.apply_script();
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn thermal_window_is_30_seconds_from_the_last_write() {
        assert_eq!(thermal_remaining(S(100), None), None);
        assert_eq!(thermal_remaining(S(100), Some(S(100))), Some(S(30)));
        assert_eq!(thermal_remaining(S(100), Some(S(80))), Some(S(10)));
        assert_eq!(thermal_remaining(S(100), Some(S(70))), None);
        assert_eq!(thermal_remaining(S(100), Some(S(10))), None);
    }

    #[test]
    fn only_a_settled_state_may_unload() {
        assert!(may_unload(Some("suspended")));
        for s in ["active", "resuming", "suspending"] {
            assert!(!may_unload(Some(s)), "{s}");
        }
        // The kernel cannot say: nothing to wait for.
        for s in [None, Some("unsupported"), Some("error")] {
            assert!(may_unload(s));
        }
    }

    #[test]
    fn a_suspended_gpu_is_unloaded_at_once() {
        let f = Fake::new("susp", vec![(S(0), "suspended")]);
        wait_quiet(&f.root(), ADDR, &f, &|| None, &|| false).unwrap();
        assert_eq!(f.now(), Duration::ZERO);
    }

    #[test]
    fn an_awake_gpu_is_polled_every_150ms_and_unloaded_the_moment_it_suspends() {
        let f = Fake::new(
            "wake",
            vec![(S(0), "active"), (Duration::from_millis(2_100), "suspended")],
        );
        wait_quiet(&f.root(), ADDR, &f, &|| None, &|| false).unwrap();
        // The first poll at or after 2.1 s; not a poll later.
        assert_eq!(f.now(), Duration::from_millis(2_100));
        assert_eq!(f.reads.get(), 14); // 14 x 150 ms
    }

    #[test]
    fn a_gpu_that_never_suspends_is_busy_after_45_seconds() {
        let f = Fake::new("busy", vec![(S(0), "active")]);
        let err = wait_quiet(&f.root(), ADDR, &f, &|| None, &|| false).unwrap_err();
        assert_eq!(err, BUSY);
        assert_eq!(err, "NVIDIA GPU busy; try again");
        assert_eq!(f.now(), SUSPEND_BOUND);
    }

    #[test]
    fn a_thermal_write_is_waited_out_first_and_not_counted_against_the_bound() {
        // Written 10 s ago: 20 s to go. The GPU is suspended meanwhile.
        let f = Fake::new("thermal", vec![(S(0), "suspended")]);
        f.now.set(S(110));
        wait_quiet(&f.root(), ADDR, &f, &|| Some(S(100)), &|| false).unwrap();
        assert_eq!(f.now(), S(130));
        // Awake until 60 s: 40 s after the 30 s window, within the 45 s poll bound.
        let f = Fake::new("thermal2", vec![(S(0), "active"), (S(60), "suspended")]);
        f.now.set(S(10));
        wait_quiet(&f.root(), ADDR, &f, &|| Some(S(10)), &|| false).unwrap();
        assert!(f.now() >= S(60) && f.now() < S(61), "{:?}", f.now());
    }

    #[test]
    fn a_thermal_write_during_the_wait_restarts_the_window() {
        let f = Fake::new("again", vec![(S(0), "suspended")]);
        let second = Cell::new(false);
        // The first write was at 0 s; a second lands at 20 s.
        let last = || {
            if f.now() >= S(20) {
                second.set(true);
            }
            Some(if second.get() { S(20) } else { S(0) })
        };
        wait_quiet(&f.root(), ADDR, &f, &last, &|| false).unwrap();
        assert!(f.now() >= S(50), "window restarted at 20 s: {:?}", f.now());
    }

    #[test]
    fn wait_thermal_ignores_the_gpu() {
        let f = Fake::new("wt", vec![(S(0), "active")]);
        f.now.set(S(5));
        wait_thermal(&f, &|| Some(S(0)), &|| false).unwrap();
        assert_eq!(f.now(), S(30));
    }

    #[test]
    fn a_cancelled_wait_stops_at_the_next_step_in_either_wait() {
        // Before any wait.
        let f = Fake::new("c0", vec![(S(0), "active")]);
        assert_eq!(
            wait_quiet(&f.root(), ADDR, &f, &|| None, &|| true),
            Err(CANCELLED.to_owned())
        );
        assert_eq!(f.now(), Duration::ZERO);
        // During the 30 s thermal window: cancelled 5 s in.
        let f = Fake::new("c1", vec![(S(0), "suspended")]);
        let cancel = || f.now() >= S(5);
        assert_eq!(
            wait_quiet(&f.root(), ADDR, &f, &|| Some(S(0)), &cancel),
            Err(CANCELLED.to_owned())
        );
        assert!(f.now() >= S(5) && f.now() < S(6), "{:?}", f.now());
        // While the GPU stays awake.
        let f = Fake::new("c2", vec![(S(0), "active")]);
        let cancel = || f.now() >= S(10);
        assert_eq!(
            wait_quiet(&f.root(), ADDR, &f, &|| None, &cancel),
            Err(CANCELLED.to_owned())
        );
        assert!(f.now() >= S(10) && f.now() < S(11), "{:?}", f.now());
        // The thermal-only wait.
        let f = Fake::new("c3", vec![(S(0), "active")]);
        let cancel = || f.now() >= S(7);
        assert_eq!(
            wait_thermal(&f, &|| Some(S(0)), &cancel),
            Err(CANCELLED.to_owned())
        );
        assert!(f.now() >= S(7) && f.now() < S(8), "{:?}", f.now());
    }

    #[test]
    fn the_watchdog_returns_for_quick_commands_and_gives_up_on_stuck_ones() {
        assert_eq!(run_watchdog("true", &[], S(5)), Ran::Exited(true));
        assert_eq!(run_watchdog("false", &[], S(5)), Ran::Exited(false));
        assert_eq!(run_watchdog("/nonexistent/cmd", &[], S(5)), Ran::Exited(false));
        let t = Instant::now();
        assert_eq!(
            run_watchdog("sleep", &["3"], Duration::from_millis(200)),
            Ran::Hung
        );
        assert!(t.elapsed() < S(2), "must not wait for the child");
    }
}
