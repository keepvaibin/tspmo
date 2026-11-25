//! Closing the apps that hold the NVIDIA GPU so that Eco can switch it off.
//!
//! Only what `tspmo_core::gpu::User::closable` allows reaches this module: ordinary users'
//! apps, never the compositor or session processes. The sequence is SIGTERM, a grace period,
//! SIGKILL for what is left, then a short wait for the kernel to tear the processes down (their
//! device files are closed when they exit).

use std::time::Duration;
use tspmo_core::gpu::User;
use tspmo_sys::SysRoot;

/// Grace before SIGKILL for a browser helper (it restarts by itself) and for any other app.
pub const GRACE_HELPER: Duration = Duration::from_secs(2);
pub const GRACE_APP: Duration = Duration::from_secs(3);
/// How long to wait for a killed process to be gone.
const AFTER_KILL: Duration = Duration::from_secs(2);
const STEP: Duration = Duration::from_millis(100);

/// A process to close: the pid is only trusted together with its start time (pids are reused).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Victim {
    pub pid: u32,
    pub name: String,
    pub started: u64,
}

impl Victim {
    pub fn of(root: &SysRoot, u: &User) -> Option<Self> {
        Some(Self {
            pid: u.pid,
            name: u.name.clone(),
            started: start_time(root, u.pid)?,
        })
    }
}

/// `starttime` and state of a process from `/proc/<pid>/stat` (the name in it may contain
/// spaces and parentheses: everything after the last `)` is plain fields).
pub fn stat(root: &SysRoot, pid: u32) -> Option<(char, u64)> {
    let text = root.read_opt(format!("/proc/{pid}/stat"))?;
    let rest = &text[text.rfind(')')? + 1..];
    let mut f = rest.split_whitespace();
    let state = f.next()?.chars().next()?;
    // After the state: ppid(4) ... starttime(22) is the 19th field following it.
    let started = f.nth(18)?.parse().ok()?;
    Some((state, started))
}

fn start_time(root: &SysRoot, pid: u32) -> Option<u64> {
    stat(root, pid).map(|(_, t)| t)
}

/// What the sequence needs from the system (a fake in the tests).
pub trait Procs {
    fn term(&self, pid: u32);
    fn kill(&self, pid: u32);
    /// Is this process still running (not exited, not a zombie, not a reused pid)?
    fn alive(&self, v: &Victim) -> bool;
    fn sleep(&self, d: Duration);
}

pub struct Real(pub SysRoot);

impl Procs for Real {
    fn term(&self, pid: u32) {
        send(pid, rustix::process::Signal::TERM);
    }
    fn kill(&self, pid: u32) {
        send(pid, rustix::process::Signal::KILL);
    }
    fn alive(&self, v: &Victim) -> bool {
        stat(&self.0, v.pid).is_some_and(|(state, t)| t == v.started && state != 'Z' && state != 'X')
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

fn send(pid: u32, sig: rustix::process::Signal) {
    let pid = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw);
    if let Some(pid) = pid {
        // The process may be gone already; nothing to report.
        let _ = rustix::process::kill_process(pid, sig);
    }
}

fn wait_gone<P: Procs + ?Sized>(p: &P, victims: &[Victim], limit: Duration) -> Vec<Victim> {
    let mut left: Vec<Victim> = victims.to_vec();
    let mut waited = Duration::ZERO;
    loop {
        left.retain(|v| p.alive(v));
        if left.is_empty() || waited >= limit {
            return left;
        }
        p.sleep(STEP);
        waited += STEP;
    }
}

/// SIGTERM, `grace`, SIGKILL for what is left, then wait. Returns the victims still running.
pub fn terminate<P: Procs + ?Sized>(p: &P, victims: &[Victim], grace: Duration) -> Vec<Victim> {
    for v in victims {
        p.term(v.pid);
    }
    let left = wait_gone(p, victims, grace);
    for v in &left {
        p.kill(v.pid);
    }
    wait_gone(p, &left, AFTER_KILL)
}

/// The grace for a set of apps: a short one when all are browser helpers.
pub fn grace_for(users: &[User]) -> Duration {
    if users.iter().all(|u| u.respawnable) {
        GRACE_HELPER
    } else {
        GRACE_APP
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A pretend process table: how each pid reacts to the signals, and what was sent.
    struct Fake {
        /// pid -> (dies on TERM, dies on KILL, alive)
        procs: RefCell<Vec<(u32, bool, bool, bool)>>,
        sent: RefCell<Vec<(&'static str, u32)>>,
        slept: RefCell<Duration>,
    }

    impl Fake {
        fn new(procs: &[(u32, bool, bool)]) -> Self {
            Self {
                procs: RefCell::new(procs.iter().map(|&(p, t, k)| (p, t, k, true)).collect()),
                sent: RefCell::default(),
                slept: RefCell::default(),
            }
        }
    }

    impl Procs for Fake {
        fn term(&self, pid: u32) {
            self.sent.borrow_mut().push(("TERM", pid));
            for p in self.procs.borrow_mut().iter_mut().filter(|p| p.0 == pid && p.1) {
                p.3 = false;
            }
        }
        fn kill(&self, pid: u32) {
            self.sent.borrow_mut().push(("KILL", pid));
            for p in self.procs.borrow_mut().iter_mut().filter(|p| p.0 == pid && p.2) {
                p.3 = false;
            }
        }
        fn alive(&self, v: &Victim) -> bool {
            self.procs.borrow().iter().any(|p| p.0 == v.pid && p.3)
        }
        fn sleep(&self, d: Duration) {
            *self.slept.borrow_mut() += d;
        }
    }

    fn victims(pids: &[u32]) -> Vec<Victim> {
        pids.iter()
            .map(|&pid| Victim {
                pid,
                name: format!("app{pid}"),
                started: 1,
            })
            .collect()
    }

    #[test]
    fn apps_that_exit_on_term_are_not_killed() {
        let f = Fake::new(&[(10, true, true), (11, true, true)]);
        let left = terminate(&f, &victims(&[10, 11]), GRACE_APP);
        assert!(left.is_empty());
        assert_eq!(*f.sent.borrow(), [("TERM", 10), ("TERM", 11)]);
        assert_eq!(*f.slept.borrow(), Duration::ZERO);
    }

    #[test]
    fn a_stubborn_app_gets_the_grace_and_then_sigkill() {
        // 20 ignores TERM; 21 exits on TERM.
        let f = Fake::new(&[(20, false, true), (21, true, true)]);
        let left = terminate(&f, &victims(&[20, 21]), GRACE_APP);
        assert!(left.is_empty());
        assert_eq!(*f.sent.borrow(), [("TERM", 20), ("TERM", 21), ("KILL", 20)]);
        assert_eq!(*f.slept.borrow(), GRACE_APP);
    }

    #[test]
    fn an_unkillable_process_is_reported() {
        let f = Fake::new(&[(30, false, false)]);
        let left = terminate(&f, &victims(&[30]), GRACE_HELPER);
        assert_eq!(left.len(), 1);
        assert_eq!(*f.slept.borrow(), GRACE_HELPER + AFTER_KILL);
    }

    #[test]
    fn the_grace_is_short_for_helpers_only() {
        let mut u = User {
            pid: 1,
            name: "x".into(),
            kind: tspmo_core::gpu::UseKind::Render,
            driver_handles: true,
            uid: 1000,
            respawnable: true,
            egl_probe: false,
        };
        assert_eq!(grace_for(&[u.clone()]), GRACE_HELPER);
        u.respawnable = false;
        assert_eq!(grace_for(&[u]), GRACE_APP);
    }

    #[test]
    fn stat_parses_names_with_spaces_and_parentheses() {
        let dir = std::env::temp_dir().join(format!("tspmod-closer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proc/7")).unwrap();
        // pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt
        // utime stime cutime cstime priority nice threads itrealvalue starttime ...
        std::fs::write(
            dir.join("proc/7/stat"),
            "7 (RDD (Process) x) S 1 7 7 0 -1 4194560 1 0 0 0 5 6 0 0 20 0 3 0 123456 99 99\n",
        )
        .unwrap();
        let root = SysRoot::new(&dir);
        assert_eq!(stat(&root, 7), Some(('S', 123_456)));
        assert_eq!(stat(&root, 8), None);
        let u = User {
            pid: 7,
            name: "firefox".into(),
            kind: tspmo_core::gpu::UseKind::Render,
            driver_handles: true,
            uid: 1000,
            respawnable: false,
            egl_probe: false,
        };
        let v = Victim::of(&root, &u).unwrap();
        assert!(Real(root.clone()).alive(&v));
        // A zombie holds no files: it counts as gone.
        std::fs::write(
            dir.join("proc/7/stat"),
            "7 (x) Z 1 7 7 0 -1 4194560 1 0 0 0 5 6 0 0 20 0 3 0 123456 99 99\n",
        )
        .unwrap();
        assert!(!Real(root).alive(&v));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
