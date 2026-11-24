//! `tspmo bench`: what does a setting cost, and what does it save?
//!
//! Conditions are tried in turn, round after round (interleaved, so drift hits every
//! condition alike). Each one runs a light, browsing-like load that logs how long its chunks
//! take, while battery draw and the RAPL energy counters are read. Everything the bench changes
//! is put back afterwards, even if it is killed: a small guard process, which waits for the
//! bench to exit (however it exits), does the restoring.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tspmo_core::SysRoot;
use tspmo_core::topology;

pub const USAGE: &str = "\
USAGE:
    sudo tspmo bench [options] [<condition>...]

A condition is `base` or settings joined with `+`:
    hz=60 | hz=max          screen refresh rate
    pkg=<long>/<short>      CPU package power limit, watts (RAPL)
    psys=<long>/<short>     platform power limit, watts (RAPL)
    uncore=<mhz>            uncore maximum clock
    noturbo | turbo         CPU turbo off | on
    epp=<preference>        energy preference of every CPU (power, balance_power, ...)
    pcap=<mhz>              maximum clock of the performance cores
    cpus=e | lpe | p | <list>   run everything on these CPUs only
    profile=<mode>          tspmo power mode (eco-plus, power-saver, ...)
    pkgwin=<ms>             package power limit (PL1) time window
    ecap=<mhz>              maximum clock of the efficiency cores
    bgcpus=e|lpe|p|<list>   CPUs the --bg load may use
    bgweight=<n>            systemd CPUWeight of the --bg load (default 100)
Example:  sudo tspmo bench base hz=60 hz=60+pkg=5/7 profile=eco-plus

OPTIONS:
    --rounds N      rounds, each trying every condition (default 3)
    --settle S      seconds to settle after a change (default 8)
    --measure S     seconds measured per condition (default 20)
    --workers N     load threads (default 2)
    --no-load       measure an idle system
    --bg N          run N busy threads as a background load (in a systemd scope; see bgcpus,
                    bgweight)
    --hold-charge   stop charging while measuring, when on AC (the battery then reads zero;
                    platform power from RAPL is used)
";

pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("--guard") => guard(args.get(1).map_or("", String::as_str)),
        Some("--burn") => {
            // Busy threads that never stop: the background load of `--bg`.
            let n = args.get(1).and_then(|n| n.parse().ok()).unwrap_or(1usize);
            let hs: Vec<_> = (0..n)
                .map(|_| {
                    std::thread::spawn(|| {
                        loop {
                            chunk()
                        }
                    })
                })
                .collect();
            for h in hs {
                let _ = h.join();
            }
            ExitCode::SUCCESS
        }
        _ => match bench(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("tspmo bench: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

// --- conditions -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    Hz(Option<u32>), // None = max
    Pkg(u32, u32),
    Psys(u32, u32),
    Uncore(u32),
    NoTurbo,
    /// Turbo forced on (undoes a profile's no-turbo).
    Turbo,
    /// Energy preference of every CPU (`power`, `balance_power`, ...).
    Epp(String),
    /// Maximum clock of the performance cores, MHz.
    Pcap(u32),
    Cpus(String),
    Profile(String),
    /// Package power limit time window, milliseconds (PL1).
    PkgWindow(u32),
    /// Maximum clock of the efficiency cores, MHz.
    Ecap(u32),
    /// CPUs the background load runs on.
    BgCpus(String),
    /// systemd CPUWeight of the background load.
    BgWeight(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub name: String,
    pub settings: Vec<Setting>,
}

fn pair(v: &str) -> Result<(u32, u32), String> {
    let (a, b) = v
        .split_once('/')
        .ok_or_else(|| format!("'{v}': use <long>/<short>"))?;
    Ok((
        a.parse().map_err(|_| format!("'{a}' is not a number"))?,
        b.parse().map_err(|_| format!("'{b}' is not a number"))?,
    ))
}

pub fn parse_condition(text: &str) -> Result<Condition, String> {
    let mut settings = Vec::new();
    for item in text.split('+').filter(|i| !i.is_empty() && *i != "base") {
        let (k, v) = item.split_once('=').unwrap_or((item, ""));
        settings.push(match k {
            "hz" if v == "max" => Setting::Hz(None),
            "hz" => Setting::Hz(Some(v.parse().map_err(|_| format!("hz={v}: not a number"))?)),
            "pkg" => {
                let (a, b) = pair(v)?;
                Setting::Pkg(a, b)
            }
            "psys" => {
                let (a, b) = pair(v)?;
                Setting::Psys(a, b)
            }
            "uncore" => Setting::Uncore(v.parse().map_err(|_| format!("uncore={v}: not a number"))?),
            "noturbo" => Setting::NoTurbo,
            "turbo" => Setting::Turbo,
            "epp" if !v.is_empty() => Setting::Epp(v.to_owned()),
            "pcap" => Setting::Pcap(v.parse().map_err(|_| format!("pcap={v}: not a number"))?),
            "pkgwin" => Setting::PkgWindow(v.parse().map_err(|_| format!("pkgwin={v}: not a number"))?),
            "ecap" => Setting::Ecap(v.parse().map_err(|_| format!("ecap={v}: not a number"))?),
            "bgcpus" if !v.is_empty() => Setting::BgCpus(v.to_owned()),
            "bgweight" => Setting::BgWeight(v.parse().map_err(|_| format!("bgweight={v}: not a number"))?),
            "cpus" if !v.is_empty() => Setting::Cpus(v.to_owned()),
            "profile" if tspmo_core::power::Profile::parse(v).is_some() => Setting::Profile(v.to_owned()),
            _ => return Err(format!("unknown setting '{item}' (see `tspmo bench help`)")),
        });
    }
    Ok(Condition {
        name: if settings.is_empty() {
            "base".into()
        } else {
            text.to_owned()
        },
        settings,
    })
}

// --- statistics -----------------------------------------------------------------------------

pub fn median(v: &[f64]) -> f64 {
    percentile(v, 50.0)
}

pub fn percentile(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let idx = ((s.len() - 1) as f64 * p / 100.0).round() as usize;
    s[idx]
}

/// Watts from two readings of a RAPL energy counter (µJ), which wraps at `range`.
pub fn watts(e0: u64, e1: u64, range: u64, secs: f64) -> f64 {
    let d = if e1 >= e0 {
        e1 - e0
    } else {
        e1 + range.saturating_sub(e0)
    };
    d as f64 / 1e6 / secs
}

// --- restoring ------------------------------------------------------------------------------

/// Everything to put back, kept in a file the guard process reads.
#[derive(Debug, Default)]
struct Restore {
    sysfs: BTreeMap<String, String>,
    profile: Option<String>,
    mode: Option<(u32, String, String)>, // uid, output, mode id
    cpus: bool,
}

impl Restore {
    fn to_text(&self) -> String {
        let mut t = String::new();
        for (p, v) in &self.sysfs {
            t.push_str(&format!("sys\t{p}\t{v}\n"));
        }
        if let Some(p) = &self.profile {
            t.push_str(&format!("profile\t{p}\n"));
        }
        if let Some((uid, o, id)) = &self.mode {
            t.push_str(&format!("mode\t{uid}\t{o}\t{id}\n"));
        }
        if self.cpus {
            t.push_str("cpus\n");
        }
        t
    }

    fn from_text(text: &str) -> Self {
        let mut r = Self::default();
        for l in text.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            match f[..] {
                ["sys", p, v] if p.starts_with("/sys/") && !p.contains("..") => {
                    r.sysfs.insert(p.into(), v.into());
                }
                ["profile", p] => r.profile = Some(p.into()),
                ["mode", uid, o, id] => r.mode = uid.parse().ok().map(|u| (u, o.into(), id.into())),
                ["cpus"] => r.cpus = true,
                _ => {}
            }
        }
        r
    }

    fn apply(&self) {
        for (p, v) in &self.sysfs {
            let _ = std::fs::write(p, v);
        }
        if self.cpus {
            set_cpus("");
        }
        if let Some((uid, out, id)) = &self.mode {
            let _ = as_user(*uid, "kscreen-doctor", &[&format!("output.{out}.mode.{id}")]);
        }
        if let Some(p) = &self.profile {
            let _ = crate::client::set_profile(p);
        }
    }
}

fn set_cpus(list: &str) {
    for unit in ["user.slice", "system.slice", "init.scope"] {
        let _ = Command::new("systemctl")
            .args(["set-property", "--runtime", unit, &format!("AllowedCPUs={list}")])
            .stderr(Stdio::null())
            .status();
    }
}

/// The user whose desktop to change (`sudo` keeps it in SUDO_UID).
fn desktop_uid() -> Option<u32> {
    std::env::var("SUDO_UID").ok()?.parse().ok()
}

fn as_user(uid: u32, program: &str, args: &[&str]) -> Option<String> {
    let dir = format!("/run/user/{uid}");
    let wayland = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .find(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))?;
    let out = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin")
        .env("XDG_RUNTIME_DIR", &dir)
        .env("WAYLAND_DISPLAY", wayland)
        .env("QT_QPA_PLATFORM", "wayland")
        .uid(uid)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn guard(file: &str) -> ExitCode {
    // Block until the bench exits (its end of our stdin closes), however it exits.
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
    if let Ok(text) = std::fs::read_to_string(file) {
        Restore::from_text(&text).apply();
        let _ = std::fs::remove_file(file);
    }
    ExitCode::SUCCESS
}

// --- the bench ------------------------------------------------------------------------------

struct Rig {
    restore: Restore,
    /// Paths changed for the whole run (they stay out of the per-condition reset).
    held: Vec<String>,
    file: String,
    uid: Option<u32>,
    topo: topology::Topology,
}

impl Rig {
    fn save(&self) {
        let _ = std::fs::write(&self.file, self.restore.to_text());
    }

    fn write(&mut self, path: &str, value: &str) {
        if !self.restore.sysfs.contains_key(path) {
            if let Ok(orig) = std::fs::read_to_string(path) {
                self.restore.sysfs.insert(path.to_owned(), orig.trim().to_owned());
                self.save();
            }
        }
        if let Err(e) = std::fs::write(path, value) {
            eprintln!("  can't write {path}: {e}");
        }
    }

    fn rapl(&mut self, zone: &str, long: u32, short: u32) {
        let root = SysRoot::default();
        for z in tspmo_core::power::rapl_zones(&root) {
            if (zone == "psys") == z.is_psys() {
                if let Some(p) = &z.long {
                    self.write(p, &(u64::from(long) * 1_000_000).to_string());
                }
                if let Some(p) = &z.short {
                    self.write(p, &(u64::from(short.max(long)) * 1_000_000).to_string());
                }
            }
        }
    }

    fn apply(&mut self, c: &Condition) {
        // Back to the starting point (except what the whole run holds), then this condition's
        // changes. Power modes first: the daemon re-applies its refresh policy when one changes.
        let held: Vec<(String, String)> = self
            .held
            .iter()
            .map(|p| (p.clone(), self.restore.sysfs.remove(p).unwrap_or_default()))
            .collect();
        self.restore.apply();
        for (p, orig) in held {
            self.restore.sysfs.insert(p, orig);
        }
        let had_profile = c.settings.iter().any(|s| matches!(s, Setting::Profile(_)));
        let mut settings: Vec<&Setting> = c.settings.iter().collect();
        settings.sort_by_key(|s| !matches!(s, Setting::Profile(_)));
        if had_profile || self.restore.profile.is_some() {
            std::thread::sleep(Duration::from_millis(1500));
        }
        for s in settings {
            match s {
                Setting::Hz(hz) => self.refresh(*hz),
                Setting::Pkg(a, b) => self.rapl("package", *a, *b),
                Setting::Psys(a, b) => self.rapl("psys", *a, *b),
                Setting::Uncore(mhz) => self.write(
                    "/sys/devices/system/cpu/intel_uncore_frequency/package_00_die_00/max_freq_khz",
                    &(mhz * 1000).to_string(),
                ),
                Setting::NoTurbo => self.write("/sys/devices/system/cpu/intel_pstate/no_turbo", "1"),
                Setting::Turbo => self.write("/sys/devices/system/cpu/intel_pstate/no_turbo", "0"),
                Setting::Epp(v) => {
                    let glob = "/sys/devices/system/cpu/cpufreq/policy*/energy_performance_preference";
                    for p in SysRoot::default().glob(glob) {
                        self.write(&p, v);
                    }
                }
                Setting::Pcap(mhz) => {
                    for cpu in self.topo.performance.clone() {
                        let p = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_max_freq");
                        self.write(&p, &(mhz * 1000).to_string());
                    }
                }
                Setting::PkgWindow(ms) => {
                    for z in tspmo_core::power::rapl_zones(&SysRoot::default()) {
                        if z.is_package() {
                            if let Some(long) = &z.long {
                                let win = long.replace("power_limit_uw", "time_window_us");
                                self.write(&win, &(u64::from(*ms) * 1000).to_string());
                            }
                        }
                    }
                }
                Setting::Ecap(mhz) => {
                    let cpus = [self.topo.efficiency.clone(), self.topo.low_power.clone()].concat();
                    for cpu in cpus {
                        let p = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_max_freq");
                        self.write(&p, &(mhz * 1000).to_string());
                    }
                }
                Setting::BgCpus(_) | Setting::BgWeight(_) => {} // used when the load starts
                Setting::Cpus(which) => {
                    let list = match which.as_str() {
                        "e" => topology::format_cpulist(
                            &[self.topo.efficiency.clone(), self.topo.low_power.clone()].concat(),
                        ),
                        "lpe" => topology::format_cpulist(&self.topo.low_power),
                        "p" => topology::format_cpulist(&self.topo.performance),
                        other => other.to_owned(),
                    };
                    self.restore.cpus = true;
                    self.save();
                    set_cpus(&list);
                }
                Setting::Profile(p) => {
                    if self.restore.profile.is_none() {
                        self.restore.profile = crate::client::state()
                            .ok()
                            .map(|s| crate::client::get_str(&s, "Profile"));
                        self.save();
                    }
                    let _ = crate::client::set_profile(p);
                }
            }
        }
    }

    fn refresh(&mut self, hz: Option<u32>) {
        let Some(uid) = self.uid else {
            eprintln!("  hz needs sudo (to find your desktop)");
            return;
        };
        let Some(text) = as_user(uid, "kscreen-doctor", &["-o"]) else {
            return;
        };
        let outs = tspmo_core::display::kscreen::parse_outputs(&text);
        let Some(o) = outs.iter().find(|o| o.is_internal()) else {
            return;
        };
        if self.restore.mode.is_none() {
            if let Some(m) = o.current_mode() {
                self.restore.mode = Some((uid, o.name.clone(), m.id.clone()));
                self.save();
            }
        }
        use tspmo_core::display::refresh::{Target, pick};
        let target = if hz == Some(60) {
            Target::Sixty
        } else {
            Target::Max
        };
        if let Some(m) = pick(o, target) {
            as_user(
                uid,
                "kscreen-doctor",
                &[&tspmo_core::display::kscreen::set_mode(&o.name, &m.id)],
            );
        }
    }
}

#[derive(Default, Clone)]
struct Sample {
    battery_w: Vec<f64>,
    pkg_w: Vec<f64>,
    psys_w: Vec<f64>,
    chunk_ms: Vec<f64>,
    /// How late a 1 ms timer fires, microseconds (the wake-up latency an input event sees).
    wake_us: Vec<f64>,
}

/// A fixed amount of arithmetic, about 5 ms on a fast core.
fn chunk() {
    let mut x = 1u64;
    for i in 0..6_000_000u64 {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
    }
    std::hint::black_box(x);
}

/// A thread asking for a 1 ms sleep over and over, logging how late it wakes.
fn start_jitter(stop: Arc<AtomicBool>, log: Arc<Mutex<Vec<(Instant, f64)>>>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let t0 = Instant::now();
            std::thread::sleep(Duration::from_millis(1));
            let late = t0.elapsed().as_secs_f64() * 1e6 - 1000.0;
            log.lock().unwrap_or_else(|p| p.into_inner()).push((t0, late));
        }
    })
}

/// The background load: `n` busy threads in a transient systemd service, so their CPUs and weight
/// can be set like a build's would be. Stopped with the slice.
fn start_background(n: usize, c: &Condition, topo: &topology::Topology) -> Option<std::process::Child> {
    if n == 0 {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let mut cmd = Command::new("systemd-run");
    // A service, not a scope: its parent is systemd, as for a build started from a terminal's
    // job, and unlike the bench's own foreground threads it is not mistaken for their child.
    cmd.args([
        "--quiet",
        "--collect",
        "--slice=tspmo-bench.slice",
        "--unit=tspmo-bench-load",
    ]);
    for s in &c.settings {
        match s {
            Setting::BgWeight(w) => {
                cmd.arg(format!("-pCPUWeight={w}"));
            }
            Setting::BgCpus(which) => {
                let list = match which.as_str() {
                    "e" => {
                        topology::format_cpulist(&[topo.efficiency.clone(), topo.low_power.clone()].concat())
                    }
                    "lpe" => topology::format_cpulist(&topo.low_power),
                    "p" => topology::format_cpulist(&topo.performance),
                    other => other.to_owned(),
                };
                cmd.arg(format!("-pAllowedCPUs={list}"));
            }
            _ => {}
        }
    }
    cmd.arg(exe).args(["bench", "--burn", &n.to_string()]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().ok()
}

fn start_load(
    workers: usize,
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<(Instant, f64)>>>,
) -> Vec<std::thread::JoinHandle<()>> {
    (0..workers)
        .map(|_| {
            let (stop, log) = (stop.clone(), log.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let t0 = Instant::now();
                    chunk();
                    let dt = t0.elapsed();
                    log.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push((t0, dt.as_secs_f64() * 1000.0));
                    std::thread::sleep(Duration::from_millis(100).saturating_sub(dt));
                }
            })
        })
        .collect()
}

fn read_u64(p: &str) -> Option<u64> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

fn battery_watts() -> Option<f64> {
    let base = std::fs::read_dir("/sys/class/power_supply")
        .ok()?
        .filter_map(|e| e.ok())
        .find(|e| std::fs::read_to_string(e.path().join("type")).is_ok_and(|t| t.trim() == "Battery"))?;
    let dir = base.path();
    if std::fs::read_to_string(dir.join("status")).ok()?.trim() != "Discharging" {
        return None;
    }
    let p = dir.join("power_now");
    if let Some(uw) = read_u64(p.to_str()?) {
        return Some(uw as f64 / 1e6);
    }
    let i = read_u64(dir.join("current_now").to_str()?)?;
    let v = read_u64(dir.join("voltage_now").to_str()?)?;
    Some(i as f64 * v as f64 / 1e12)
}

fn zone_energy(psys: bool) -> Option<(String, u64)> {
    let root = SysRoot::default();
    let z = tspmo_core::power::rapl_zones(&root)
        .into_iter()
        .find(|z| z.is_psys() == psys && (psys || z.dir.contains("intel-rapl:")))?;
    let range = read_u64(&format!("{}/max_energy_range_uj", z.dir)).unwrap_or(u64::MAX);
    Some((z.dir, range))
}

fn bench(args: &[String]) -> Result<(), String> {
    let (mut rounds, mut settle, mut measure, mut workers) = (3usize, 8u64, 20u64, 2usize);
    let (mut load, mut hold) = (true, false);
    let mut bg = 0usize;
    let mut conds = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut num = |name: &str| -> Result<u64, String> {
            it.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{name} needs a number"))
        };
        match a.as_str() {
            "--rounds" => rounds = num("--rounds")? as usize,
            "--settle" => settle = num("--settle")?,
            "--measure" => measure = num("--measure")?.max(2),
            "--workers" => workers = num("--workers")? as usize,
            "--no-load" => load = false,
            "--bg" => bg = num("--bg")? as usize,
            "--hold-charge" => hold = true,
            c => conds.push(parse_condition(c)?),
        }
    }
    if conds.is_empty() {
        conds = ["base", "hz=60", "profile=eco-plus"]
            .iter()
            .map(|c| parse_condition(c))
            .collect::<Result<_, _>>()?;
    }
    if !std::fs::metadata("/proc/self").is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0) {
        return Err("needs root to change power limits: run it with sudo".into());
    }

    let file = format!("/run/tspmo-bench-{}", std::process::id());
    let mut rig = Rig {
        restore: Restore::default(),
        held: Vec::new(),
        file: file.clone(),
        uid: desktop_uid(),
        topo: topology::detect(&SysRoot::default()),
    };
    rig.save();
    // The guard restores everything when this process ends, however it ends.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut guard = Command::new(exe)
        .args(["bench", "--guard", &file])
        .stdin(Stdio::piped())
        .process_group(0) // Ctrl-C reaches only us; the guard outlives us and cleans up
        .spawn()
        .map_err(|e| e.to_string())?;
    let _keep = guard.stdin.take();

    let charge = "/sys/class/power_supply/BAT1/charge_control_end_threshold";
    let on_ac = tspmo_sys::power_supply::on_ac(&SysRoot::default()) == Some(true);
    if hold && on_ac {
        // The firmware honours only some limits (60, 65, 70, 75, 80 on ASUS): take the highest
        // one the battery is above, so charging stops.
        let cap = read_u64("/sys/class/power_supply/BAT1/capacity").unwrap_or(50);
        let orig = std::fs::read_to_string(charge).ok().map(|v| v.trim().to_owned());
        let status = || std::fs::read_to_string("/sys/class/power_supply/BAT1/status").unwrap_or_default();
        let chosen = (20..=cap).rev().find(|n| {
            if std::fs::write(charge, n.to_string()).is_err() || read_u64(charge) != Some(*n) {
                return false;
            }
            // Some firmware takes any number but honours only a few: charging must really stop.
            (0..6).any(|_| {
                std::thread::sleep(Duration::from_millis(500));
                status().trim() != "Charging"
            })
        });
        match (orig, chosen) {
            (Some(orig), Some(n)) => {
                rig.restore.sysfs.insert(charge.to_owned(), orig);
                rig.held.push(charge.to_owned());
                rig.save();
                eprintln!("charging held at {n}% (battery {cap}%)");
            }
            (Some(orig), None) => {
                let _ = std::fs::write(charge, orig);
                eprintln!("can't stop charging: the battery limit won't go below {cap}%");
            }
            _ => eprintln!("can't stop charging: no charge limit control"),
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut threads = if load {
        start_load(workers, stop.clone(), log.clone())
    } else {
        Vec::new()
    };
    let wake_log = Arc::new(Mutex::new(Vec::new()));
    threads.push(start_jitter(stop.clone(), wake_log.clone()));
    let pkg = zone_energy(false);
    let psys = zone_energy(true);

    let mut results: Vec<Vec<Sample>> = vec![Vec::new(); conds.len()];
    for round in 1..=rounds {
        for (i, c) in conds.iter().enumerate() {
            eprint!("round {round}/{rounds}  {:<28}\r", c.name);
            let _ = std::io::stderr().flush();
            rig.apply(c);
            let mut bg_proc = start_background(bg, c, &rig.topo);
            std::thread::sleep(Duration::from_secs(settle));
            let t0 = Instant::now();
            let e0 = (
                pkg.as_ref()
                    .and_then(|(d, _)| read_u64(&format!("{d}/energy_uj"))),
                psys.as_ref()
                    .and_then(|(d, _)| read_u64(&format!("{d}/energy_uj"))),
            );
            let mut s = Sample::default();
            while t0.elapsed() < Duration::from_secs(measure) {
                std::thread::sleep(Duration::from_secs(1));
                s.battery_w.extend(battery_watts());
            }
            let secs = t0.elapsed().as_secs_f64();
            if let (Some((d, r)), Some(a)) = (&pkg, e0.0) {
                s.pkg_w
                    .extend(read_u64(&format!("{d}/energy_uj")).map(|b| watts(a, b, *r, secs)));
            }
            if let (Some((d, r)), Some(a)) = (&psys, e0.1) {
                s.psys_w
                    .extend(read_u64(&format!("{d}/energy_uj")).map(|b| watts(a, b, *r, secs)));
            }
            let end = Instant::now();
            s.chunk_ms = log
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .filter(|(t, _)| *t >= t0 && *t <= end)
                .map(|(_, ms)| *ms)
                .collect();
            s.wake_us = wake_log
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .filter(|(t, _)| *t >= t0 && *t <= end)
                .map(|(_, us)| *us)
                .collect();
            if let Some(mut p) = bg_proc.take() {
                let _ = p.kill();
                let _ = p.wait();
                // The scope's own processes die with the unit.
                let _ = Command::new("systemctl")
                    .args(["stop", "--quiet", "tspmo-bench.slice"])
                    .stderr(Stdio::null())
                    .status();
                // Eco+'s guard may have moved the load out of the unit: stop it by name.
                let _ = Command::new("pkill")
                    .args(["-f", "bench --burn"])
                    .stderr(Stdio::null())
                    .status();
            }
            results[i].push(s);
        }
    }
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        let _ = t.join();
    }
    rig.restore.apply();
    let _ = std::fs::remove_file(&file);
    eprintln!("{:60}", "");

    let med = |rs: &[Sample], f: fn(&Sample) -> &Vec<f64>| -> f64 {
        median(
            &rs.iter()
                .map(|s| median(f(s)))
                .filter(|x| x.is_finite())
                .collect::<Vec<_>>(),
        )
    };
    let p95_of = |rs: &[Sample], f: fn(&Sample) -> &Vec<f64>| -> f64 {
        median(
            &rs.iter()
                .map(|s| percentile(f(s), 95.0))
                .filter(|x| x.is_finite())
                .collect::<Vec<_>>(),
        )
    };
    let base_p95 = p95_of(&results[0], |s| &s.chunk_ms);
    println!(
        "{:<34} {:>9} {:>7} {:>6} {:>8} {:>8} {:>7} {:>9}",
        "condition", "battery W", "psys W", "pkg W", "chunk ms", "p95 ms", "p95 x", "wake p95us"
    );
    for (c, rs) in conds.iter().zip(&results) {
        let f = |x: f64| {
            if x.is_finite() {
                format!("{x:.2}")
            } else {
                "-".to_owned()
            }
        };
        let ms = med(rs, |s| &s.chunk_ms);
        let p95 = median(
            &rs.iter()
                .map(|s| percentile(&s.chunk_ms, 95.0))
                .filter(|x| x.is_finite())
                .collect::<Vec<_>>(),
        );
        println!(
            "{:<34} {:>9} {:>7} {:>6} {:>8} {:>8} {:>7} {:>9}",
            c.name,
            f(med(rs, |s| &s.battery_w)),
            f(med(rs, |s| &s.psys_w)),
            f(med(rs, |s| &s.pkg_w)),
            f(ms),
            f(p95),
            if p95.is_finite() && base_p95.is_finite() {
                format!("{:.2}x", p95 / base_p95)
            } else {
                "-".into()
            },
            f(p95_of(rs, |s| &s.wake_us)),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions() {
        let c = parse_condition("hz=60+pkg=5/7+noturbo").unwrap();
        assert_eq!(
            c.settings,
            [Setting::Hz(Some(60)), Setting::Pkg(5, 7), Setting::NoTurbo]
        );
        assert_eq!(parse_condition("base").unwrap().settings, []);
        assert_eq!(parse_condition("hz=max").unwrap().settings, [Setting::Hz(None)]);
        assert!(parse_condition("pkg=5").is_err());
        assert!(parse_condition("profile=turbo-ultra").is_err());
        assert!(parse_condition("warp=9").is_err());
    }

    #[test]
    fn stats() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 100.0], 95.0), 100.0);
        assert!(median(&[]).is_nan());
    }

    #[test]
    fn rapl_watts_handle_wraparound() {
        assert!((watts(1_000_000, 11_000_000, u64::MAX, 2.0) - 5.0).abs() < 1e-9);
        // Counter wrapped at 100 J: 90 J -> 10 J is 20 J over 4 s.
        assert!((watts(90_000_000, 10_000_000, 100_000_000, 4.0) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn restore_file_round_trips_and_rejects_bad_paths() {
        let mut r = Restore::default();
        r.sysfs.insert("/sys/a/b".into(), "7".into());
        r.profile = Some("balanced".into());
        r.mode = Some((1000, "eDP-1".into(), "1".into()));
        r.cpus = true;
        let t = r.to_text();
        let back = Restore::from_text(&format!("{t}sys\t/etc/passwd\tx\nsys\t/sys/../etc/x\ty\n"));
        assert_eq!(back.sysfs.len(), 1);
        assert_eq!(back.profile.as_deref(), Some("balanced"));
        assert_eq!(back.mode, Some((1000, "eDP-1".into(), "1".into())));
        assert!(back.cpus);
    }
}
