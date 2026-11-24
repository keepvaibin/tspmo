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

/// A process holding the dGPU open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub pid: u32,
    pub name: String,
    pub kind: UseKind,
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
        for e in fds.filter_map(Result::ok) {
            let Ok(t) = std::fs::read_link(e.path()) else {
                continue;
            };
            let Some(t) = t.to_str() else { continue };
            if t.starts_with("/dev/nvidia") || renders.iter().any(|n| n == t) {
                kind = Some(UseKind::Render);
                break;
            }
            if cards.iter().any(|n| n == t) {
                kind = Some(UseKind::Display);
            }
        }
        if let Some(kind) = kind {
            let name = root.read_opt(format!("/proc/{pid}/comm")).unwrap_or_default();
            out.push(User { pid, name, kind });
        }
    }
    out
}

/// NVIDIA helper daemons that hold the GPU but can be stopped safely for a switch.
pub const NVIDIA_SERVICES: &[(&str, &str)] = &[
    ("nvidia-powerd", "nvidia-powerd.service"),
    ("nvidia-persistenced", "nvidia-persistenced.service"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies() {
        assert_eq!(Policy::Optimized.target(true), Mode::Eco);
        assert_eq!(Policy::Optimized.target(false), Mode::Hybrid);
        assert_eq!(Policy::parse("ultimate"), Some(Policy::Fixed(Mode::Ultimate)));
        assert_eq!(Policy::parse("optimized"), Some(Policy::Optimized));
    }
}
