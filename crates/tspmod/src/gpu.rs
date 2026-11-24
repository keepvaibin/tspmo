//! Performing GPU mode switches.
//!
//! **Eco ↔ Hybrid is live.** To power the dGPU off we must first make sure nothing holds it:
//! stop NVIDIA's helper daemons, unload the driver, then ask the firmware to cut power
//! (`dgpu_disable=1`), which also removes the device from the PCI bus. Powering it back on is the
//! reverse: `dgpu_disable=0`, PCI rescan, load the driver, restart the helpers.
//!
//! **Ultimate (MUX) needs a reboot:** the panel is physically rewired. We write
//! `gpu_mux_mode` and report the pending reboot; the firmware applies it at the next boot.

use crate::state::Daemon;
use std::process::Command;
use std::time::{Duration, Instant};
use tspmo_core::gpu::{self, Mode, Switcher};
use tspmo_sys::{SysRoot, fwattr};

/// Kernel modules of the NVIDIA driver stack, in unload order.
const NVIDIA_MODULES: &[&str] = &["nvidia_drm", "nvidia_modeset", "nvidia_uvm", "nvidia"];

#[derive(Debug)]
pub enum Outcome {
    Done(String),
    NeedsReboot(String),
}

impl Outcome {
    pub fn message(&self) -> &str {
        match self {
            Self::Done(m) | Self::NeedsReboot(m) => m,
        }
    }
}

fn set_asus(root: &SysRoot, fw_attrs: bool, name: &str, value: &str) -> Result<(), String> {
    if fw_attrs {
        let attr =
            fwattr::read(root, gpu::ASUS_FW, name).ok_or_else(|| format!("{name} is not available"))?;
        attr.set(root, value).map_err(|e| format!("{name}: {e}"))
    } else {
        root.write(format!("{}/{name}", gpu::ASUS_LEGACY), value)
            .map_err(|e| format!("{name}: {e}"))
    }
}

fn run(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).status().is_ok_and(|s| s.success())
}

fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    cond()
}

/// Processes (other than NVIDIA's own helpers) that would lose the GPU.
pub fn blocking_users(root: &SysRoot) -> Vec<gpu::User> {
    let addr = gpu::dgpu(root).map(|g| g.address);
    gpu::users(root, addr.as_deref())
        .into_iter()
        .filter(|u| !gpu::NVIDIA_SERVICES.iter().any(|(comm, _)| u.name == *comm))
        .collect()
}

fn power_off(root: &SysRoot, fw_attrs: bool) -> Result<(), String> {
    let users = blocking_users(root);
    if !users.is_empty() {
        let list: Vec<String> = users.iter().map(|u| format!("{} ({})", u.name, u.pid)).collect();
        return Err(format!(
            "the NVIDIA GPU is in use by {}; close them and try again",
            list.join(", ")
        ));
    }
    let mut stopped = Vec::new();
    for (_, unit) in gpu::NVIDIA_SERVICES {
        if run("systemctl", &["is-active", "--quiet", unit]) && run("systemctl", &["stop", unit]) {
            stopped.push(*unit);
        }
    }
    let restore = |msg: String| {
        for unit in &stopped {
            run("systemctl", &["start", unit]);
        }
        Err(msg)
    };
    for m in NVIDIA_MODULES {
        if root.exists(format!("/sys/module/{m}")) && !run("modprobe", &["-r", m]) {
            return restore(format!(
                "could not unload the {m} driver (something still uses it)"
            ));
        }
    }
    if let Err(e) = set_asus(root, fw_attrs, "dgpu_disable", "1") {
        run("modprobe", &["nvidia_drm"]);
        return restore(e);
    }
    wait_for(Duration::from_secs(5), || gpu::dgpu(root).is_none());
    Ok(())
}

fn power_on(root: &SysRoot, fw_attrs: bool) -> Result<(), String> {
    set_asus(root, fw_attrs, "dgpu_disable", "0")?;
    let _ = root.write("/sys/bus/pci/rescan", "1");
    if !wait_for(Duration::from_secs(5), || gpu::dgpu(root).is_some()) {
        return Err("the NVIDIA GPU did not come back on the PCI bus".into());
    }
    run("modprobe", &["nvidia_drm"]);
    for (_, unit) in gpu::NVIDIA_SERVICES {
        if run("systemctl", &["is-enabled", "--quiet", unit]) {
            run("systemctl", &["start", unit]);
        }
    }
    Ok(())
}

/// Switch to `target`. Blocking (module unload/load takes ~1 s); call off the D-Bus executor.
pub fn switch(d: &mut Daemon, target: Mode) -> Result<Outcome, String> {
    let st = d.gpu_status();
    let Switcher::Asus { fw_attrs, has_mux } = st.switcher else {
        return match target {
            Mode::Hybrid => Ok(Outcome::Done("Hybrid is the only mode on this laptop".into())),
            _ => Err("this laptop has no firmware GPU switch".into()),
        };
    };
    let root = d.root.clone();

    // Undo a pending MUX change if the user goes back to what is running now.
    if st.pending.is_some() && target == st.mode {
        set_asus(
            &root,
            fw_attrs,
            "gpu_mux_mode",
            if st.mode == Mode::Ultimate { "0" } else { "1" },
        )?;
        return Ok(Outcome::Done(format!(
            "Cancelled the pending switch; staying in {}",
            target
        )));
    }

    match (st.mode, target) {
        (a, b) if a == b && st.pending.is_none() => Ok(Outcome::Done(format!("Already in {b} mode"))),
        (Mode::Hybrid, Mode::Eco) => {
            power_off(&root, fw_attrs)?;
            Ok(Outcome::Done("Eco: the NVIDIA GPU is powered off".into()))
        }
        (Mode::Eco, Mode::Hybrid) => {
            power_on(&root, fw_attrs)?;
            Ok(Outcome::Done(
                "Hybrid: the NVIDIA GPU is available to apps that ask for it".into(),
            ))
        }
        (_, Mode::Ultimate) => {
            if !has_mux {
                return Err("this laptop has no MUX switch".into());
            }
            if st.mode == Mode::Eco {
                power_on(&root, fw_attrs)?;
            }
            set_asus(&root, fw_attrs, "gpu_mux_mode", "0")?;
            Ok(Outcome::NeedsReboot(
                "Ultimate takes effect after a reboot: the panel will be wired to the NVIDIA GPU".into(),
            ))
        }
        (Mode::Ultimate, b) => {
            set_asus(&root, fw_attrs, "gpu_mux_mode", "1")?;
            let extra = if b == Mode::Eco {
                "; Eco will be applied after it"
            } else {
                ""
            };
            Ok(Outcome::NeedsReboot(format!(
                "{b} takes effect after a reboot{extra}"
            )))
        }
        (_, b) => Ok(Outcome::Done(format!("Already in {b} mode"))),
    }
}

/// Bring the hardware in line with the policy for the current power source.
/// Used at start-up and on AC/battery changes (Optimized). Never forces: if apps are using
/// the dGPU, Eco waits for the next event.
pub fn enforce_policy(d: &mut Daemon) -> Option<String> {
    let target = d.gpu_policy.target(d.on_battery);
    let st = d.gpu_status();
    if st.mode == target || st.pending.is_some() || st.mode == Mode::Ultimate || target == Mode::Ultimate {
        return None;
    }
    let msg = match switch(d, target) {
        Ok(o) => o.message().to_owned(),
        Err(e) => format!("Couldn't switch to {target}: {e}"),
    };
    eprintln!("tspmod: gpu: {msg}");
    d.gpu_message = msg.clone();
    Some(msg)
}
