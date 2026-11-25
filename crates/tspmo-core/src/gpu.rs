//! GPU modes, the way G-Helper / Armoury Crate present them:
//!
//! | Mode | Hardware | Switch |
//! |---|---|---|
//! | **Eco** | dGPU powered off and removed from the PCI bus | live, if nothing is using the dGPU |
//! | **Hybrid** (Standard) | iGPU drives the panel; apps can request the dGPU (PRIME offload); dGPU sleeps in D3cold otherwise | live |
//! | **Ultimate** | MUX routes the panel to the dGPU directly (lowest latency, no iGPU) | needs a reboot |
//! | **Optimized** | policy: Eco on battery, Hybrid on AC | live |
//!
//! This module only *reads* state. The daemon performs switches (it has to stop services and
//! load/unload modules), see `tspmod::gpu`.

use std::fmt;
use tspmo_sys::{SysRoot, fwattr, pci};

pub const ASUS_FW: &str = "asus-armoury";
pub const ASUS_LEGACY: &str = "/sys/devices/platform/asus-nb-wmi";

/// Written by `tspmod` when `rmmod nvidia` hung in the kernel (an unkillable task: the driver
/// is half unloaded). It lives in `/run`, so only a restart clears it. While it exists GPU
/// switches are refused and `tspmo doctor` says so.
pub const RESTART_MARKER: &str = "/run/tspmo/gpu-restart";

/// GpuMessage (and the refusal) while [`RESTART_MARKER`] exists.
pub const RESTART_REQUIRED: &str = "Restart to recover the NVIDIA GPU";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Eco,
    Hybrid,
    Ultimate,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eco => "eco",
            Self::Hybrid => "hybrid",
            Self::Ultimate => "ultimate",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "eco" | "integrated" | "igpu" => Some(Self::Eco),
            "hybrid" | "standard" | "optimus" => Some(Self::Hybrid),
            "ultimate" | "dgpu" | "discrete" => Some(Self::Ultimate),
            _ => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the user asked for (Optimized is a policy over Eco/Hybrid).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Fixed(Mode),
    Optimized,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fixed(m) => m.as_str(),
            Self::Optimized => "optimized",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        if s.trim() == "optimized" || s.trim() == "auto" {
            Some(Self::Optimized)
        } else {
            Mode::parse(s).map(Self::Fixed)
        }
    }
    /// The mode this policy wants for a power source.
    pub fn target(self, on_battery: bool) -> Mode {
        match self {
            Self::Fixed(m) => m,
            Self::Optimized if on_battery => Mode::Eco,
            Self::Optimized => Mode::Hybrid,
        }
    }
}

/// Which mechanism this machine has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switcher {
    /// ASUS: `dgpu_disable` (+ `gpu_mux_mode` on MUX models) via asus-armoury or legacy asus-nb-wmi.
    Asus { fw_attrs: bool, has_mux: bool },
    /// No firmware switch: Hybrid only (RTD3 keeps the dGPU asleep).
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub switcher: Switcher,
    pub mode: Mode,
    /// A MUX change is waiting for a reboot (the value it will boot into).
    pub pending: Option<Mode>,
    /// dGPU PCI address when it is on the bus.
    pub dgpu: Option<String>,
    pub dgpu_asleep: bool,
}

fn asus_read(root: &SysRoot, fw: bool, name: &str) -> Option<u8> {
    if fw {
        fwattr::read(root, ASUS_FW, name)?.current?.parse().ok()
    } else {
        root.read_parse(format!("{ASUS_LEGACY}/{name}"))
    }
}

pub fn switcher(root: &SysRoot) -> Switcher {
    let fw = fwattr::read(root, ASUS_FW, "dgpu_disable").is_some();
    if fw || root.exists(format!("{ASUS_LEGACY}/dgpu_disable")) {
        let has_mux = if fw {
            fwattr::read(root, ASUS_FW, "gpu_mux_mode").is_some()
        } else {
            root.exists(format!("{ASUS_LEGACY}/gpu_mux_mode"))
        };
        return Switcher::Asus {
            fw_attrs: fw,
            has_mux,
        };
    }
    Switcher::None
}

/// The discrete GPU: a display-class device that isn't the boot VGA and isn't Intel.
pub fn dgpu(root: &SysRoot) -> Option<pci::Gpu> {
    let gpus = pci::gpus(root);
    if gpus.len() < 2 {
        return gpus.into_iter().find(|g| g.vendor == pci::VENDOR_NVIDIA);
    }
    gpus.into_iter()
        .find(|g| !g.boot_vga && g.vendor != pci::VENDOR_INTEL)
}

/// Current mode. `boot_mux` is the MUX value the daemon saw at boot (to detect a pending switch).
pub fn status(root: &SysRoot, boot_mux: Option<u8>) -> Status {
    let sw = switcher(root);
    let dg = dgpu(root);
    let (mode, pending) = match sw {
        Switcher::Asus { fw_attrs, has_mux } => {
            let mux = if has_mux {
                asus_read(root, fw_attrs, "gpu_mux_mode")
            } else {
                None
            };
            let disabled = asus_read(root, fw_attrs, "dgpu_disable") == Some(1);
            // gpu_mux_mode: 0 = dGPU direct (Ultimate), 1 = Optimus.
            let booted_ultimate = boot_mux.or(mux) == Some(0);
            let mode = if booted_ultimate {
                Mode::Ultimate
            } else if disabled {
                Mode::Eco
            } else {
                Mode::Hybrid
            };
            let pending = match (boot_mux, mux) {
                (Some(b), Some(now)) if b != now => {
                    Some(if now == 0 { Mode::Ultimate } else { Mode::Hybrid })
                }
                _ => None,
            };
            (mode, pending)
        }
        Switcher::None => (Mode::Hybrid, None),
    };
    Status {
        switcher: sw,
        mode,
        pending,
        dgpu_asleep: dg.as_ref().is_some_and(pci::Gpu::is_asleep),
        dgpu: dg.map(|g| g.address),
    }
}

/// How a process uses the dGPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseKind {
    /// Holds only the display (card) node: the compositor or the session manager watching the
    /// GPU's outputs. It goes away when the desktop stops using the dGPU (log out, or keep the
    /// compositor on the iGPU).
    Display,
    /// Renders or computes on it (`/dev/nvidia*` or the render node): an app.
    Render,
}

/// Compositors and session managers. They hold the dGPU because it has display outputs, and
/// they let go when the kernel (or tspmod) announces the GPU is being removed.
pub const DESKTOP_PROCESSES: &[&str] = &[
    "kwin_wayland",
    "Xwayland",
    "gnome-shell",
    "mutter",
    "plasmashell",
    "Hyprland",
    "sway",
    "niri",
    "cosmic-comp",
    "systemd",
    "systemd-logind",
];

/// A process holding the dGPU open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub pid: u32,
    pub name: String,
    pub kind: UseKind,
    /// Holds NVIDIA driver handles (`/dev/nvidia*`): the driver can't be unloaded until it exits.
    /// For a compositor this means it loaded NVIDIA's EGL at start-up.
    pub driver_handles: bool,
    /// Owner of the process: only ordinary users' apps are ever closed for a switch.
    pub uid: u32,
    /// A browser's GPU/decoder helper, which the browser starts again when it needs one.
    pub respawnable: bool,
    /// Holds the dGPU only because its EGL looked for a GPU (NVIDIA's EGL is mapped and the
    /// process did not ask for the dGPU): it was started without tspmo's session environment.
    pub egl_probe: bool,
}

/// First uid of ordinary users: the lowest a process can have and still be closed for a switch.
pub const FIRST_USER_UID: u32 = 1000;

impl User {
    /// May the daemon close this process to free the dGPU: an app of an ordinary user, never
    /// the desktop (the compositor and session processes release the GPU when told it is going)
    /// and never NVIDIA's own helpers.
    pub fn closable(&self) -> bool {
        self.kind == UseKind::Render
            && self.uid >= FIRST_USER_UID
            && !DESKTOP_PROCESSES.contains(&app_name(self))
            && !NVIDIA_SERVICES.iter().any(|(comm, _)| self.name == *comm)
    }
}

/// Does this process have NVIDIA's EGL loaded without having asked for the NVIDIA GPU (no PRIME
/// offload, no NVIDIA EGL vendor chosen)? `false` when its environment can't be read.
fn probes_egl(root: &SysRoot, pid: u32) -> bool {
    let maps = root.read_opt(format!("/proc/{pid}/maps")).unwrap_or_default();
    if !maps.contains("libEGL_nvidia") {
        return false;
    }
    let Ok(env) = std::fs::read(root.path(format!("/proc/{pid}/environ"))) else {
        return false;
    };
    let env = String::from_utf8_lossy(&env);
    !env.split('\0').any(|v| {
        v == "__NV_PRIME_RENDER_OFFLOAD=1"
            || v.strip_prefix("__EGL_VENDOR_LIBRARY_FILENAMES=")
                .is_some_and(|f| f.contains("nvidia"))
    })
}

/// Is this process a browser helper that is started again when the browser needs it: Firefox's
/// `RDD Process`, `GPU Process` and `Utility Process`, Chromium's and Electron's
/// `--type=gpu-process`? `cmdline` is `/proc/<pid>/cmdline` (NUL separated).
pub fn is_respawnable_helper(comm: &str, cmdline: &str) -> bool {
    const FIREFOX: &[&str] = &["RDD Process", "GPU Process", "Utility Process"];
    (FIREFOX.contains(&comm) && cmdline.contains("-contentproc"))
        || cmdline.split('\0').any(|a| a == "--type=gpu-process")
}

/// What to close so that a switch to Eco can go ahead, or `None` when something in the way
/// can't be closed (or, without `all`, isn't a helper that comes back by itself).
/// `all`: every app holding the GPU; else only the respawnable browser helpers.
pub fn plan_close(users: &[User], all: bool) -> Option<Vec<User>> {
    let blockers: Vec<&User> = users.iter().filter(|u| u.kind == UseKind::Render).collect();
    if blockers.is_empty() || !blockers.iter().all(|u| u.closable() && (all || u.respawnable)) {
        return None;
    }
    Some(blockers.into_iter().cloned().collect())
}

/// Processes with the dGPU's device nodes open (`/dev/nvidia*`, its `/dev/dri` card/render nodes).
/// Needs root to see other users' processes.
pub fn users(root: &SysRoot, dgpu_addr: Option<&str>) -> Vec<User> {
    let mut cards: Vec<String> = Vec::new();
    let mut renders: Vec<String> = Vec::new();
    if let Some(addr) = dgpu_addr {
        for n in root.list(format!("{}/{addr}/drm", pci::DEVICES)) {
            if n.starts_with("card") {
                cards.push(format!("/dev/dri/{n}"));
            } else if n.starts_with("renderD") {
                renders.push(format!("/dev/dri/{n}"));
            }
        }
    }
    let me = std::process::id();
    let mut out = Vec::new();
    for pid_dir in root.list("/proc") {
        let Ok(pid) = pid_dir.parse::<u32>() else { continue };
        if pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(root.path(format!("/proc/{pid}/fd"))) else {
            continue;
        };
        let mut kind = None;
        let mut driver_handles = false;
        for e in fds.filter_map(Result::ok) {
            let Ok(t) = std::fs::read_link(e.path()) else {
                continue;
            };
            let Some(t) = t.to_str() else { continue };
            if t.starts_with("/dev/nvidia") {
                driver_handles = true;
                kind = Some(UseKind::Render);
            } else if renders.iter().any(|n| n == t) {
                kind = Some(UseKind::Render);
            } else if cards.iter().any(|n| n == t) && kind.is_none() {
                kind = Some(UseKind::Display);
            }
        }
        if let Some(mut kind) = kind {
            let comm = root.read_opt(format!("/proc/{pid}/comm")).unwrap_or_default();
            // Helper processes ("RDD Process", "GPU Process") are named after their app.
            let exe = std::fs::read_link(root.path(format!("/proc/{pid}/exe")))
                .ok()
                .and_then(|p| Some(p.file_name()?.to_str()?.trim_end_matches("-bin").to_owned()));
            let name = match exe {
                Some(e) if !e.is_empty() && !comm.starts_with(e.as_str()) && comm.contains(' ') => {
                    format!("{e} ({comm})")
                }
                _ => comm,
            };
            if DESKTOP_PROCESSES.contains(&name.as_str()) {
                kind = UseKind::Display;
            }
            let cmdline = root.read_opt(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let uid = std::fs::metadata(root.path(format!("/proc/{pid}")))
                .map_or(0, |m| std::os::unix::fs::MetadataExt::uid(&m));
            out.push(User {
                pid,
                respawnable: is_respawnable_helper(comm_of(&name), &cmdline),
                egl_probe: driver_handles && probes_egl(root, pid),
                name,
                kind,
                driver_handles,
                uid,
            });
        }
    }
    out
}

/// The process name inside `firefox (RDD Process)`; a plain name is its own.
fn comm_of(name: &str) -> &str {
    name.split_once(" (")
        .map_or(name, |(_, rest)| rest.trim_end_matches(')'))
}

/// The name of the app behind a user: helper processes are listed as `firefox (RDD Process)`,
/// the app is `firefox`.
pub fn app_name(u: &User) -> &str {
    u.name.split(" (").next().unwrap_or(&u.name)
}

/// The apps among `users` that render on the dGPU, once each, first letter capitalised
/// (`Firefox`), in the order found.
pub fn render_apps(users: &[User]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in users.iter().filter(|u| u.kind == UseKind::Render) {
        let mut chars = app_name(u).chars();
        let Some(first) = chars.next() else { continue };
        let name: String = first.to_uppercase().chain(chars).collect();
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// "Firefox is using the NVIDIA GPU", "Firefox and Steam are using ...", at most three named.
pub fn using_text(apps: &[String]) -> String {
    match apps {
        [] => String::new(),
        [a] => format!("{a} is using the NVIDIA GPU"),
        [a, b] => format!("{a} and {b} are using the NVIDIA GPU"),
        [a, b, c] => format!("{a}, {b} and {c} are using the NVIDIA GPU"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more are using the NVIDIA GPU", rest.len()),
    }
}

/// NVIDIA helper daemons that hold the GPU but can be stopped safely for a switch.
pub const NVIDIA_SERVICES: &[(&str, &str)] = &[
    ("nvidia-powerd", "nvidia-powerd.service"),
    ("nvidia-persistenced", "nvidia-persistenced.service"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn user(name: &str, kind: UseKind) -> User {
        User {
            pid: 1,
            name: name.into(),
            kind,
            driver_handles: true,
            uid: 1000,
            respawnable: false,
            egl_probe: false,
        }
    }

    #[test]
    fn browser_helpers_are_respawnable() {
        assert!(is_respawnable_helper(
            "RDD Process",
            "/usr/lib/firefox/firefox\0-contentproc\0rdd"
        ));
        assert!(is_respawnable_helper("GPU Process", "firefox\0-contentproc"));
        assert!(is_respawnable_helper(
            "chrome",
            "/opt/google/chrome/chrome\0--type=gpu-process\0--x"
        ));
        assert!(is_respawnable_helper("electron", "electron\0--type=gpu-process"));
        // The browser itself, a renderer and a game are not.
        assert!(!is_respawnable_helper("firefox", "/usr/lib/firefox/firefox"));
        assert!(!is_respawnable_helper("RDD Process", "something else"));
        assert!(!is_respawnable_helper("chrome", "chrome\0--type=renderer"));
        assert!(!is_respawnable_helper("steam", "steam"));
    }

    #[test]
    fn only_ordinary_apps_are_closable() {
        let mut u = user("steam", UseKind::Render);
        assert!(u.closable());
        u.uid = 0;
        assert!(!u.closable());
        for desktop in [
            "kwin_wayland",
            "Xwayland",
            "plasmashell",
            "systemd",
            "systemd-logind",
        ] {
            let d = user(desktop, UseKind::Render);
            assert!(!d.closable(), "{desktop}");
        }
        assert!(!user("kwin_wayland", UseKind::Display).closable());
        assert!(!user("nvidia-powerd", UseKind::Render).closable());
    }

    /// A process with its fds, comm and command line in a fake /proc.
    fn fake_proc(dir: &std::path::Path, pid: u32, comm: &str, cmdline: &str, fds: &[&str]) {
        let p = dir.join(format!("proc/{pid}"));
        std::fs::create_dir_all(p.join("fd")).unwrap();
        std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(p.join("cmdline"), cmdline).unwrap();
        for (i, t) in fds.iter().enumerate() {
            std::os::unix::fs::symlink(t, p.join("fd").join(i.to_string())).unwrap();
        }
    }

    #[test]
    fn users_are_classified_from_proc() {
        let dir = std::env::temp_dir().join(format!("tspmo-core-users-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        fake_proc(
            &dir,
            100,
            "RDD Process",
            "/usr/lib/firefox/firefox\0-contentproc\0rdd\0",
            &["/dev/nvidia0", "/dev/nvidiactl", "/dev/null"],
        );
        fake_proc(&dir, 101, "steam", "steam\0", &["/dev/nvidiactl"]);
        fake_proc(
            &dir,
            102,
            "chrome",
            "chrome\0--type=gpu-process\0",
            &["/dev/nvidia0"],
        );
        fake_proc(&dir, 103, "bash", "bash\0", &["/dev/null"]);
        let users = super::users(&SysRoot::new(&dir), None);
        let by = |pid: u32| users.iter().find(|u| u.pid == pid);
        assert!(by(100).unwrap().respawnable && by(100).unwrap().driver_handles);
        assert!(!by(101).unwrap().respawnable);
        assert!(by(102).unwrap().respawnable);
        assert!(by(103).is_none());
        // 100 has NVIDIA's EGL mapped and no offload variable: it only looked for a GPU.
        std::fs::write(
            dir.join("proc/100/maps"),
            "7f00 r-xp /usr/lib/libEGL_nvidia.so.0\n",
        )
        .unwrap();
        std::fs::write(dir.join("proc/100/environ"), "HOME=/home/x\0").unwrap();
        std::fs::write(
            dir.join("proc/101/maps"),
            "7f00 r-xp /usr/lib/libEGL_nvidia.so.0\n",
        )
        .unwrap();
        std::fs::write(dir.join("proc/101/environ"), "__NV_PRIME_RENDER_OFFLOAD=1\0").unwrap();
        let users = super::users(&SysRoot::new(&dir), None);
        let by = |pid: u32| users.iter().find(|u| u.pid == pid);
        assert!(by(100).unwrap().egl_probe);
        assert!(!by(101).unwrap().egl_probe, "a deliberate offload is not a probe");
        assert!(!by(102).unwrap().egl_probe);
        // The owner is the owner of the /proc entry (this test's user).
        assert_eq!(
            by(101).unwrap().uid,
            std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&dir).unwrap())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_plans() {
        let mut rdd = user("firefox (RDD Process)", UseKind::Render);
        rdd.respawnable = true;
        let steam = user("steam", UseKind::Render);
        let mut root_app = user("miner", UseKind::Render);
        root_app.uid = 0;
        // Helpers alone: closed without asking.
        assert_eq!(plan_close(&[rdd.clone()], false).unwrap().len(), 1);
        // An app in the way: nothing is closed unless everything may be.
        assert!(plan_close(&[rdd.clone(), steam.clone()], false).is_none());
        assert_eq!(plan_close(&[rdd.clone(), steam.clone()], true).unwrap().len(), 2);
        // Something that can't be closed stops the plan.
        assert!(plan_close(&[rdd, steam, root_app], true).is_none());
        assert!(plan_close(&[], true).is_none());
    }

    #[test]
    fn apps_are_named_once_and_by_their_executable() {
        let users = [
            user("firefox (RDD Process)", UseKind::Render),
            user("firefox (GPU Process)", UseKind::Render),
            user("kwin_wayland", UseKind::Display),
            user("steam", UseKind::Render),
        ];
        let apps = render_apps(&users);
        assert_eq!(apps, ["Firefox", "Steam"]);
        assert_eq!(using_text(&apps[..1]), "Firefox is using the NVIDIA GPU");
        assert_eq!(using_text(&apps), "Firefox and Steam are using the NVIDIA GPU");
        let many: Vec<String> = ["A", "B", "C", "D", "E"].map(String::from).to_vec();
        assert_eq!(using_text(&many), "A, B and 3 more are using the NVIDIA GPU");
    }

    #[test]
    fn policies() {
        assert_eq!(Policy::Optimized.target(true), Mode::Eco);
        assert_eq!(Policy::Optimized.target(false), Mode::Hybrid);
        assert_eq!(Policy::parse("ultimate"), Some(Policy::Fixed(Mode::Ultimate)));
        assert_eq!(Policy::parse("optimized"), Some(Policy::Optimized));
    }
}
