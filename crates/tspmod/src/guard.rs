//! Eco+ background guard: heavy background work must not starve the desktop.
//!
//! While Eco+ is active one thread samples the CPU every few seconds (a single read of
//! `/proc/stat` while the machine is quiet, so an idle laptop costs about a wake-up every 4 s
//! and nothing else). When a process outside the interactive part of the desktop keeps a core
//! busy, the guard moves it, its whole job (the build's `make`, not the shell above it) and
//! everything it starts into `tspmobg.slice`: efficiency cores only and the lowest CPU weight.
//! The performance cores, and with them the power budget, stay free for the compositor, the
//! shell and the app in use. When Eco+ ends, or the daemon stops, everything goes back to the
//! cgroup it came from.
//!
//! Interactive means: `session.slice` (compositor, shell), and every graphical app that is not a
//! terminal. Terminals, services, login sessions and the indexer are where builds, backups and
//! updates run, so those are the ones that can be moved. Kernel threads, PID 1 and `tspmod`
//! never are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tspmo_sys::SysRoot;

pub const SLICE: &str = "tspmobg.slice";
const CGROUP: &str = "/sys/fs/cgroup";
/// Where each moved process came from, kept while Eco+ is active so a restarted daemon (or
/// `tspmod --restore-limits`) still puts everything back.
const SAVED: &str = "/run/tspmo/guard";
/// Sampling period while nothing is busy, then while something is.
const QUIET: Duration = Duration::from_secs(4);
const BUSY: Duration = Duration::from_secs(1);
/// A process is heavy at this share of one core, in two samples running.
const HEAVY: f64 = 0.5;
/// Total busy CPU (cores) below which no scan of `/proc` is made.
const SCAN_ABOVE: f64 = 0.5;

/// Terminal emulators: the apps whose scopes hold builds.
const TERMINALS: &[&str] = &[
    "konsole",
    "yakuake",
    "kitty",
    "alacritty",
    "ptyxis",
    "wezterm",
    "ghostty",
    "gnome-terminal",
    "xterm",
    "tilix",
    "terminator",
    "xfce4-terminal",
    "lxterminal",
    "foot",
    "rio",
    "tabby",
];

/// Processes of these names are never moved, wherever they run.
const NEVER: &[&str] = &[
    "tspmod",
    "kwin_wayland",
    "kwin_x11",
    "plasmashell",
    "Xwayland",
    "systemd",
    "systemd-logind",
    "dbus-broker",
    "dbus-broker-lau",
    "dbus-daemon",
    "pipewire",
    "pipewire-pulse",
    "wireplumber",
    "sddm",
    "sddm-helper",
    "gdm",
    "upowerd",
    "polkitd",
    "NetworkManager",
];

/// What a build is started from: the climb to the root of a job stops below these.
const SHELLS: &[&str] = &[
    "bash",
    "zsh",
    "fish",
    "sh",
    "dash",
    "nu",
    "xonsh",
    "tmux: server",
    "tmux",
    "screen",
    "sshd",
    "login",
    "su",
    "sudo",
];

/// Indexers and updaters that sit in a graphical app's cgroup but are background work.
const ALWAYS_BACKGROUND: &[&str] = &["baloo_file", "baloo_file_extr", "packagekitd", "tracker-miner-f"];

/// Can processes of this cgroup (`/user.slice/.../app-konsole@x.scope`) be moved?
pub fn movable(cgroup: &str) -> bool {
    if cgroup.is_empty()
        || cgroup == "/"
        || cgroup.contains("/session.slice")
        || cgroup.contains("/init.scope")
    {
        return false;
    }
    if cgroup.contains(&format!("/{SLICE}")) {
        return false;
    }
    // `app-<id>[@random|-pid].scope|service` under app.slice: a graphical app or an autostart.
    match cgroup
        .rsplit('/')
        .find(|c| c.starts_with("app-") && (c.ends_with(".scope") || c.ends_with(".service")))
    {
        Some(unit) => TERMINALS.iter().any(|t| unit.contains(t)),
        None => true,
    }
}

/// One process, from `/proc/<pid>/stat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub comm: String,
    pub ppid: u32,
    /// utime + stime, clock ticks.
    pub ticks: u64,
    pub start: u64,
}

/// Parse `/proc/<pid>/stat` (the name may hold spaces and parentheses: split at the last `)`).
pub fn parse_stat(pid: u32, text: &str) -> Option<Proc> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let comm = text.get(open + 1..close)?.to_owned();
    let f: Vec<&str> = text.get(close + 2..)?.split_whitespace().collect();
    // After the name: state(0) ppid(1) ... utime(11) stime(12) ... starttime(19).
    Some(Proc {
        pid,
        comm,
        ppid: f.get(1)?.parse().ok()?,
        ticks: f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?,
        start: f.get(19)?.parse().ok()?,
    })
}

/// `cpu` line of `/proc/stat`: busy and total jiffies.
pub fn parse_cpu(stat: &str) -> Option<(u64, u64)> {
    let f: Vec<u64> = stat
        .lines()
        .next()?
        .strip_prefix("cpu ")?
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    let total: u64 = f.iter().take(8).sum();
    let idle = f.get(3)? + f.get(4).unwrap_or(&0);
    Some((total.saturating_sub(idle), total))
}

/// The cgroup of a process from `/proc/<pid>/cgroup` (`0::/path`).
pub fn parse_cgroup(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(str::to_owned)
}

/// The top of a job: climb from a heavy process while the parent is ordinary work in a
/// movable cgroup (not a shell, a terminal, or a protected name).
fn job_root(pid: u32, procs: &BTreeMap<u32, Proc>, cgroups: &BTreeMap<u32, String>) -> u32 {
    let mut p = pid;
    for _ in 0..8 {
        let Some(parent) = procs.get(&p).and_then(|x| procs.get(&x.ppid)) else {
            break;
        };
        let ok = parent.pid > 1
            && !SHELLS.contains(&parent.comm.as_str())
            && !NEVER.contains(&parent.comm.as_str())
            && !TERMINALS.iter().any(|t| parent.comm.contains(t))
            && cgroups.get(&parent.pid).is_some_and(|c| movable(c));
        if !ok {
            break;
        }
        p = parent.pid;
    }
    p
}

/// `root` and all its descendants.
fn subtree(root: u32, procs: &BTreeMap<u32, Proc>) -> Vec<u32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let at = out[i];
        out.extend(
            procs
                .values()
                .filter(|p| p.ppid == at && p.pid != at)
                .map(|p| p.pid),
        );
        i += 1;
    }
    out
}

struct Guard {
    root: SysRoot,
    slice: PathBuf,
    /// `pid` → the cgroup it came from.
    moved: BTreeMap<u32, String>,
    last: BTreeMap<u32, (u64, u64)>,
    streak: BTreeMap<u32, u32>,
}

impl Guard {
    fn procs(&self) -> BTreeMap<u32, Proc> {
        self.root
            .list("/proc")
            .into_iter()
            .filter_map(|n| {
                let pid: u32 = n.parse().ok()?;
                parse_stat(pid, &self.root.read_opt(format!("/proc/{pid}/stat"))?).map(|p| (pid, p))
            })
            .collect()
    }

    fn cgroup(&self, pid: u32) -> Option<String> {
        parse_cgroup(&self.root.read_opt(format!("/proc/{pid}/cgroup"))?)
    }

    /// One look at the machine. Returns whether the next one should come soon.
    fn scan(&mut self, dt: f64) -> bool {
        let procs = self.procs();
        let hz = 100.0;
        let mut heavy = Vec::new();
        let mut live = BTreeSet::new();
        for p in procs.values() {
            // Kernel threads have no parent but 2; they and PID 1 are not ours.
            if p.pid <= 1 || p.ppid == 2 || p.pid == 2 || p.pid == std::process::id() {
                continue;
            }
            live.insert(p.pid);
            let prev = self.last.insert(p.pid, (p.ticks, p.start));
            let Some((t0, s0)) = prev else { continue };
            if s0 != p.start || p.ticks < t0 {
                continue;
            }
            let share = (p.ticks - t0) as f64 / hz / dt;
            if share >= HEAVY {
                heavy.push((p.pid, share));
            } else {
                self.streak.remove(&p.pid);
            }
        }
        self.last.retain(|pid, _| live.contains(pid));
        self.streak.retain(|pid, _| live.contains(pid));
        let before = self.moved.len();
        let mut soon = !heavy.is_empty();
        let mut cgroups: Option<BTreeMap<u32, String>> = None;
        for (pid, _) in heavy {
            let n = self.streak.entry(pid).or_insert(0);
            *n += 1;
            if *n < 2 {
                continue;
            }
            let Some(p) = procs.get(&pid) else { continue };
            if NEVER.contains(&p.comm.as_str()) {
                continue;
            }
            // Only now is the whole table worth reading.
            let cg = cgroups.get_or_insert_with(|| {
                procs
                    .keys()
                    .map(|q| (*q, self.cgroup(*q).unwrap_or_default()))
                    .collect()
            });
            let own = cg.get(&pid).map_or("", String::as_str);
            if !(ALWAYS_BACKGROUND.contains(&p.comm.as_str()) || movable(own)) {
                continue;
            }
            let top = job_root(pid, &procs, cg);
            for q in subtree(top, &procs) {
                let from = cg.get(&q).cloned().unwrap_or_default();
                self.demote(q, from);
            }
            soon = true;
        }
        if self.moved.len() != before {
            self.save();
        }
        soon
    }

    fn new(root: SysRoot) -> Self {
        let mut g = Self {
            slice: PathBuf::from(CGROUP).join(SLICE),
            root,
            moved: BTreeMap::new(),
            last: BTreeMap::new(),
            streak: BTreeMap::new(),
        };
        if let Some(text) = g.root.read_opt(SAVED) {
            g.moved = text
                .lines()
                .filter_map(|l| {
                    let (pid, cg) = l.split_once('\t')?;
                    Some((pid.parse().ok()?, cg.to_owned()))
                })
                .collect();
        }
        g
    }

    fn save(&self) {
        let path = self.root.path(SAVED);
        if self.moved.is_empty() {
            let _ = std::fs::remove_file(path);
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text: String = self.moved.iter().map(|(p, c)| format!("{p}\t{c}\n")).collect();
        let _ = std::fs::write(path, text);
    }

    fn demote(&mut self, pid: u32, from: String) {
        if from.is_empty() || from.contains(SLICE) || self.moved.contains_key(&pid) {
            return;
        }
        if self
            .root
            .write(self.slice.join("cgroup.procs"), &pid.to_string())
            .is_ok()
        {
            self.moved.insert(pid, from);
        }
    }

    /// Everything back to where it was. Children that started in the slice go where their
    /// parent came from.
    fn restore(&mut self) {
        let procs = self.procs();
        let in_slice: Vec<u32> = self
            .root
            .read_opt(self.slice.join("cgroup.procs"))
            .map(|t| t.lines().filter_map(|l| l.parse().ok()).collect())
            .unwrap_or_default();
        for pid in in_slice {
            let mut at = pid;
            let mut dest = None;
            for _ in 0..16 {
                if let Some(c) = self.moved.get(&at) {
                    dest = Some(c.clone());
                    break;
                }
                match procs.get(&at) {
                    Some(p) if p.ppid > 1 => at = p.ppid,
                    _ => break,
                }
            }
            let dest = dest.unwrap_or_else(|| "/system.slice".to_owned());
            let mut dir = PathBuf::from(CGROUP).join(dest.trim_start_matches('/'));
            // A cgroup that is gone: the nearest one above it that still exists.
            while dir.as_path() != std::path::Path::new(CGROUP) && !self.root.exists(&dir) {
                dir.pop();
            }
            let _ = self.root.write(dir.join("cgroup.procs"), &pid.to_string());
        }
        self.moved.clear();
        self.save();
    }
}

fn systemctl(args: &[&str]) {
    let _ = std::process::Command::new("systemctl")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// The efficiency cores, as a systemd/kernel CPU list (empty without a hybrid CPU).
fn background_cpus(root: &SysRoot) -> String {
    let t = tspmo_core::topology::detect(root);
    if !t.is_hybrid() {
        return String::new();
    }
    tspmo_core::topology::format_cpulist(&[t.efficiency, t.low_power].concat())
}

/// Held for the whole life of a guard thread: a new one waits for the old one to finish putting
/// everything back (and stopping the slice) before it starts the slice again.
static LIFE: Mutex<()> = Mutex::new(());

fn run(root: SysRoot, stop: &mpsc::Receiver<()>) {
    let _life = LIFE.lock().unwrap_or_else(|p| p.into_inner());
    // Eco+ ended while this thread was waiting for its predecessor.
    if matches!(stop.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
        return;
    }
    let cpus = background_cpus(&root);
    systemctl(&["start", SLICE]);
    if !cpus.is_empty() {
        systemctl(&["set-property", "--runtime", SLICE, &format!("AllowedCPUs={cpus}")]);
    }
    let mut g = Guard::new(root);
    let mut period = QUIET;
    let mut cpu0 = g.root.read_opt("/proc/stat").and_then(|t| parse_cpu(&t));
    let mut t0 = Instant::now();
    while stop.recv_timeout(period) == Err(RecvTimeoutError::Timeout) {
        let dt = t0.elapsed().as_secs_f64();
        t0 = Instant::now();
        let cpu1 = g.root.read_opt("/proc/stat").and_then(|t| parse_cpu(&t));
        // Busy jiffies of all CPUs per second, over 100: cores kept busy.
        let busy = match (cpu0, cpu1) {
            (Some((b0, _)), Some((b1, _))) => b1.saturating_sub(b0) as f64 / 100.0 / dt,
            _ => 0.0,
        };
        cpu0 = cpu1;
        let soon = if busy >= SCAN_ABOVE || !g.moved.is_empty() {
            g.scan(dt)
        } else {
            // Nothing to see: forget what the last scan knew, so a later one starts clean.
            g.last.clear();
            g.streak.clear();
            false
        };
        period = if soon { BUSY } else { QUIET };
    }
    g.restore();
    systemctl(&["stop", SLICE]);
}

static RUNNING: OnceLock<Mutex<Option<Sender<()>>>> = OnceLock::new();

fn running() -> std::sync::MutexGuard<'static, Option<Sender<()>>> {
    RUNNING
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Eco+ starts or ends (called from the event bus, so it must return at once).
pub fn set_active(root: &SysRoot, active: bool) {
    let mut r = running();
    match (active, r.is_some()) {
        (true, false) => {
            let (tx, rx) = mpsc::channel();
            let root = root.clone();
            if std::thread::Builder::new()
                .name("eco-guard".into())
                .stack_size(256 * 1024)
                .spawn(move || run(root, &rx))
                .is_ok()
            {
                *r = Some(tx);
            }
        }
        (false, true) => {
            // Dropping the sender ends the thread, which puts everything back.
            *r = None;
        }
        _ => {}
    }
}

/// The daemon stops: put everything back before leaving (blocks briefly).
pub fn shutdown(root: &SysRoot) {
    let _life = LIFE.lock().unwrap_or_else(|p| p.into_inner());
    let mut g = Guard::new(root.clone());
    let used = !g.moved.is_empty() || root.exists(g.slice.join("cgroup.procs"));
    g.restore();
    if used {
        systemctl(&["stop", SLICE]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "/user.slice/user-1000.slice/user@1000.service";

    #[test]
    fn builds_in_terminals_and_services_can_move_the_desktop_cannot() {
        assert!(movable(&format!(
            "{USER}/app.slice/app-org.kde.konsole@3f2a.scope"
        )));
        assert!(movable(&format!("{USER}/app.slice/app-kitty-4242.scope")));
        assert!(movable("/system.slice/cargo-build.service"));
        assert!(movable("/user.slice/user-1000.slice/session-3.scope"));
        assert!(movable("/tspmo-bench.slice/run-r1.service"));
        assert!(movable(&format!("{USER}/background.slice/backup.service")));
        assert!(!movable(&format!(
            "{USER}/session.slice/plasma-kwin_wayland.service"
        )));
        assert!(!movable(&format!("{USER}/app.slice/app-firefox@abc.scope")));
        assert!(!movable(&format!(
            "{USER}/app.slice/app-org.kde.plasmashell@autostart.service"
        )));
        assert!(!movable("/init.scope"));
        assert!(!movable("/"));
        assert!(!movable(&format!("/{SLICE}")));
    }

    #[test]
    fn stat_line_with_awkward_names() {
        let t = "4242 (tmux: server (1)) S 1 4242 4242 0 -1 4194560 100 0 0 0 150 50 0 0 20 0 1 0 987654 0 0 \
                 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";
        let p = parse_stat(4242, t).unwrap();
        assert_eq!(p.comm, "tmux: server (1)");
        assert_eq!((p.ppid, p.ticks, p.start), (1, 200, 987654));
        assert!(parse_stat(1, "garbage").is_none());
    }

    #[test]
    fn busy_share_of_the_machine() {
        let (busy, total) = parse_cpu("cpu  100 0 50 800 40 0 10 0 0 0\ncpu0 1 2 3\n").unwrap();
        assert_eq!((busy, total), (160, 1000));
        assert_eq!(
            parse_cgroup("0::/system.slice/x.service\n").as_deref(),
            Some("/system.slice/x.service")
        );
    }

    #[test]
    fn a_job_is_moved_from_its_root_not_from_the_shell() {
        let p = |pid, comm: &str, ppid| {
            (
                pid,
                Proc {
                    pid,
                    comm: comm.into(),
                    ppid,
                    ticks: 0,
                    start: 1,
                },
            )
        };
        let procs: BTreeMap<u32, Proc> = [
            p(100, "konsole", 1),
            p(101, "fish", 100),
            p(102, "cargo", 101),
            p(103, "rustc", 102),
            p(104, "cc1", 103),
        ]
        .into_iter()
        .collect();
        let cg = format!("{USER}/app.slice/app-org.kde.konsole@1.scope");
        let cgroups: BTreeMap<u32, String> = procs.keys().map(|k| (*k, cg.clone())).collect();
        assert_eq!(job_root(104, &procs, &cgroups), 102);
        assert_eq!(subtree(102, &procs), [102, 103, 104]);
        // A job under an interactive app stays put.
        let mut ui = cgroups.clone();
        ui.insert(102, format!("{USER}/app.slice/app-firefox@1.scope"));
        assert_eq!(job_root(104, &procs, &ui), 103);
    }
}
