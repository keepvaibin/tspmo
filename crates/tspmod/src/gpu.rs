//! Performing GPU mode switches.
//!
//! **Eco ↔ Hybrid is live**, measured on a GU605MI under KWin 6.8:
//!
//! * *Hybrid → Eco* (~1 s): announce the removal of the dGPU's DRM devices so the compositor
//!   drops them (KWin handles a secondary GPU going away without disturbing the session), wait
//!   until nothing holds them, stop NVIDIA's helper daemons, unload the driver with `rmmod`
//!   (`modprobe -r` also tries to remove shared dependencies such as `video` and reports a
//!   failure), then `dgpu_disable=1`: the firmware cuts power and removes the device from PCI.
//! * *Eco → Hybrid* (~8 s, almost all inside the firmware call): `dgpu_disable=0`, PCI rescan,
//!   load the driver, restart the helpers. The compositor hot-adds the GPU from the kernel event.
//!
//! Switches run on their own thread: the firmware call blocks for seconds and the D-Bus API
//! (KDE's battery applet, the GUI) must stay responsive. Errors a user can act on ("an app is
//! using the GPU") are found by a fast [`precheck`] and returned immediately.
//!
//! **Ultimate (MUX) needs a reboot:** the panel is physically rewired. We write
//! `gpu_mux_mode`; the firmware applies it at the next boot.

use crate::dbus::{Shared, emit_changed};
use std::process::Command;
use std::time::{Duration, Instant};
use tspmo_core::gpu::{self, Mode, Policy, Status, Switcher};
use tspmo_sys::{SysRoot, fwattr};

/// Kernel modules of the NVIDIA driver stack, in unload order.
const NVIDIA_MODULES: &[&str] = &["nvidia_drm", "nvidia_modeset", "nvidia_uvm", "nvidia"];

/// Shown when the running desktop still has NVIDIA's EGL driver loaded (it was started before
/// tspmo's compositor settings existed). Happens once, right after installing tspmo.
pub const NEEDS_FRESH_SESSION: &str = "The desktop holds the NVIDIA driver. Log out and in once.";

/// Shown when a GPU change is asked for before the boot has settled (see `boot.rs`).
pub const STILL_STARTING: &str = "Still starting. Try again in a minute.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eco_plus_forces_eco_and_leaving_it_restores_the_policy() {
        for policy in [
            Policy::Optimized,
            Policy::Fixed(Mode::Hybrid),
            Policy::Fixed(Mode::Eco),
        ] {
            for battery in [true, false] {
                assert_eq!(wanted_mode(policy, battery, true), Mode::Eco);
                assert_eq!(wanted_mode(policy, battery, false), policy.target(battery));
            }
        }
        // Optimized on AC is Hybrid again once Eco+ ends.
        assert_eq!(wanted_mode(Policy::Optimized, false, false), Mode::Hybrid);
        // Ultimate is a reboot-only choice that Eco+ leaves alone.
        assert_eq!(
            wanted_mode(Policy::Fixed(Mode::Ultimate), true, true),
            Mode::Ultimate
        );
    }
}

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

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, crate::state::Daemon> {
    s.lock().unwrap_or_else(|p| p.into_inner())
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
    Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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

/// Processes holding the dGPU, minus NVIDIA's own helper daemons (we stop those ourselves).
pub fn blocking_users(root: &SysRoot) -> Vec<gpu::User> {
    let addr = gpu::dgpu(root).map(|g| g.address);
    gpu::users(root, addr.as_deref())
        .into_iter()
        .filter(|u| !gpu::NVIDIA_SERVICES.iter().any(|(comm, _)| u.name == *comm))
        .collect()
}

/// The dGPU's DRM nodes (`card0`, `renderD129`, ...).
fn drm_nodes(root: &SysRoot, addr: &str) -> Vec<String> {
    root.list(format!("{}/{addr}/drm", tspmo_sys::pci::DEVICES))
        .into_iter()
        .filter(|n| n.starts_with("card") || n.starts_with("renderD"))
        .collect()
}

/// Tell compositors the dGPU's display devices are going away (or back).
fn announce(root: &SysRoot, addr: &str, action: &str) {
    for n in drm_nodes(root, addr) {
        let _ = root.write(
            format!("{}/{addr}/drm/{n}/uevent", tspmo_sys::pci::DEVICES),
            action,
        );
    }
}

/// While the dGPU is added or removed, the firmware sends ACPI video "output changed"
/// notifications, which the kernel turns into a display-switch key press (KEY_SWITCHVIDEOMODE):
/// KDE then pops up its "which screens?" chooser. Mute only those events for the duration of a
/// switch (plus a settle time for late notifications); brightness events are untouched.
struct MuteDisplaySwitchKey {
    root: SysRoot,
}

const VIDEO_KEY_EVENTS: &str = "/sys/module/video/parameters/report_key_events";

/// Overlapping switches share one mute: the original value is saved by the first and restored
/// by the last.
static MUTE: std::sync::Mutex<(u32, Option<String>)> = std::sync::Mutex::new((0, None));

impl MuteDisplaySwitchKey {
    fn new(root: &SysRoot) -> Self {
        let mut m = MUTE.lock().unwrap_or_else(|p| p.into_inner());
        if m.0 == 0 {
            m.1 = root.read_opt(VIDEO_KEY_EVENTS);
            if let Some(prev) = &m.1 {
                // Bit 0 = output-switch events, bit 1 = brightness events; -1 means "all".
                let v: i32 = prev.trim().parse().unwrap_or(-1);
                let all = if v < 0 { 3 } else { v };
                let _ = root.write(VIDEO_KEY_EVENTS, &(all & !1).to_string());
            }
        }
        m.0 += 1;
        Self { root: root.clone() }
    }
}

impl Drop for MuteDisplaySwitchKey {
    fn drop(&mut self) {
        std::thread::sleep(Duration::from_secs(3));
        let mut m = MUTE.lock().unwrap_or_else(|p| p.into_inner());
        m.0 = m.0.saturating_sub(1);
        if m.0 == 0 {
            if let Some(prev) = m.1.take() {
                let _ = self.root.write(VIDEO_KEY_EVENTS, &prev);
            }
        }
    }
}

/// Fast checks for problems the user can act on, before any slow work starts.
pub fn precheck(root: &SysRoot, st: &Status, target: Mode) -> Result<(), String> {
    if !crate::boot::allowed() {
        return Err(STILL_STARTING.into());
    }
    match st.switcher {
        Switcher::None if target != Mode::Hybrid => {
            return Err("this laptop has no firmware GPU switch".into());
        }
        Switcher::Asus { has_mux: false, .. } if target == Mode::Ultimate => {
            return Err("no MUX switch: no Ultimate mode".into());
        }
        _ => {}
    }
    if target == Mode::Eco && st.mode == Mode::Hybrid && st.dgpu.is_some() {
        let users = blocking_users(root);
        let apps: Vec<String> = users
            .iter()
            .filter(|u| u.kind == gpu::UseKind::Render)
            .map(|u| format!("{} ({})", u.name, u.pid))
            .collect();
        if !apps.is_empty() {
            return Err(format!(
                "the NVIDIA GPU is in use by {}; close them and try again",
                apps.join(", ")
            ));
        }
        if users.iter().any(|u| u.driver_handles) {
            return Err(NEEDS_FRESH_SESSION.into());
        }
    }
    Ok(())
}

/// Power the dGPU off, live. Any failure rolls back to Hybrid exactly as it was.
fn power_off(root: &SysRoot, fw_attrs: bool) -> Result<(), String> {
    let Some(addr) = gpu::dgpu(root).map(|g| g.address) else {
        return Ok(()); // already off the bus
    };

    // 1. The desktop lets go of the GPU's display devices.
    announce(root, &addr, "remove");
    if !wait_for(Duration::from_secs(4), || blocking_users(root).is_empty()) {
        announce(root, &addr, "add");
        let left: Vec<String> = blocking_users(root).iter().map(|u| u.name.clone()).collect();
        return Err(format!(
            "{} didn't release the NVIDIA GPU; staying in Hybrid",
            left.join(", ")
        ));
    }

    // 2. NVIDIA's helpers and driver.
    let mut stopped = Vec::new();
    for (_, unit) in gpu::NVIDIA_SERVICES {
        if run("systemctl", &["is-active", "--quiet", unit]) && run("systemctl", &["stop", unit]) {
            stopped.push(*unit);
        }
    }
    let rollback = |msg: String| {
        run("modprobe", &["nvidia_drm"]);
        for unit in &stopped {
            run("systemctl", &["start", unit]);
        }
        announce(root, &addr, "add");
        Err(msg)
    };
    for m in NVIDIA_MODULES {
        if root.exists(format!("/sys/module/{m}")) && !run("rmmod", &[m]) {
            return rollback(format!(
                "couldn't unload {m}: something still uses the NVIDIA driver"
            ));
        }
    }

    // 3. Cut the power.
    if let Err(e) = set_asus(root, fw_attrs, "dgpu_disable", "1") {
        return rollback(e);
    }
    if !wait_for(Duration::from_secs(5), || gpu::dgpu(root).is_none()) {
        eprintln!("tspmod: gpu: dgpu_disable=1 written but the device is still on the bus");
    }
    Ok(())
}

fn power_on(root: &SysRoot, fw_attrs: bool) -> Result<(), String> {
    // The firmware powers the GPU up and re-enumerates it; this call takes several seconds.
    set_asus(root, fw_attrs, "dgpu_disable", "0")?;
    if gpu::dgpu(root).is_none() {
        let _ = root.write("/sys/bus/pci/rescan", "1");
    }
    if !wait_for(Duration::from_secs(10), || gpu::dgpu(root).is_some()) {
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

/// Perform a switch. Blocking: call from a worker thread (or at start-up, before D-Bus).
pub fn switch(root: &SysRoot, st: &Status, target: Mode) -> Result<Outcome, String> {
    precheck(root, st, target)?;
    let Switcher::Asus { fw_attrs, .. } = st.switcher else {
        return Ok(Outcome::Done("Hybrid is the only mode".into()));
    };

    // Going back to what is running now cancels a pending MUX change.
    if st.pending.is_some() && target == st.mode {
        let mux = if st.mode == Mode::Ultimate { "0" } else { "1" };
        set_asus(root, fw_attrs, "gpu_mux_mode", mux)?;
        return Ok(Outcome::Done(format!(
            "Cancelled the pending switch; staying in {target}"
        )));
    }

    match (st.mode, target) {
        (a, b) if a == b && st.pending.is_none() => Ok(Outcome::Done(format!("Already in {b} mode"))),
        (Mode::Hybrid, Mode::Eco) => {
            power_off(root, fw_attrs)?;
            Ok(Outcome::Done("Eco: the NVIDIA GPU is off".into()))
        }
        (Mode::Eco, Mode::Hybrid) => {
            power_on(root, fw_attrs)?;
            Ok(Outcome::Done("Hybrid: the NVIDIA GPU is ready".into()))
        }
        (_, Mode::Ultimate) => {
            if st.mode == Mode::Eco {
                power_on(root, fw_attrs)?;
            }
            set_asus(root, fw_attrs, "gpu_mux_mode", "0")?;
            Ok(Outcome::NeedsReboot("Ultimate set. Restart to apply.".into()))
        }
        (Mode::Ultimate, b) => {
            set_asus(root, fw_attrs, "gpu_mux_mode", "1")?;
            let then = if b == Mode::Eco {
                " (Eco is applied right after)"
            } else {
                ""
            };
            Ok(Outcome::NeedsReboot(format!(
                "{b} is ready: restart to finish{then}"
            )))
        }
        (_, b) => Ok(Outcome::Done(format!("Already in {b} mode"))),
    }
}

/// The mode wanted now. Eco+ means the dGPU is off, whatever the policy says (a policy that
/// fixes Ultimate stays: that one needs a reboot and is not Eco+'s to undo). Leaving Eco+ just
/// goes back to the policy, which Eco+ never changed.
pub fn wanted_mode(policy: Policy, on_battery: bool, eco_plus: bool) -> Mode {
    if eco_plus && policy != Policy::Fixed(Mode::Ultimate) {
        Mode::Eco
    } else {
        policy.target(on_battery)
    }
}

/// What the policy wants right now, if it differs from the hardware and can be done live.
pub fn policy_target(d: &crate::state::Daemon) -> Option<Mode> {
    // Not while the boot is still bringing the display stack (and the NVIDIA driver) up.
    if !crate::boot::allowed() {
        return None;
    }
    let target = wanted_mode(d.gpu_policy, d.on_battery, d.effective().is_eco_plus());
    let st = d.gpu_status();
    let live = st.mode != Mode::Ultimate && target != Mode::Ultimate && st.pending.is_none();
    (live && st.mode != target).then_some(target)
}

/// Start a switch on a worker thread. `policy`: saved once the switch succeeds (user requests).
/// `automatic`: a policy-driven switch, retried while an app holds the GPU.
/// Returns immediately; progress is published through `GpuSwitching` / `GpuMessage`.
pub fn begin(
    conn: zbus::Connection,
    shared: Shared,
    target: Mode,
    policy: Option<Policy>,
    automatic: bool,
) -> Result<(), String> {
    let (root, st) = {
        let mut d = lock(&shared);
        if let Some(t) = d.gpu_switching {
            return Err(format!("already switching to {t}; one moment"));
        }
        let st = d.gpu_status();
        if let Err(e) = precheck(&d.root, &st, target) {
            if automatic {
                d.gpu_waiting = e != NEEDS_FRESH_SESSION;
                let msg = format!("Waiting to switch to {target}: {e}");
                // Eco+ promises the dGPU off: say what is holding it (once, not on every retry).
                if d.effective().is_eco_plus() && d.gpu_message != msg {
                    crate::session::osd("video-card", &e, None);
                }
                d.gpu_message = msg;
            }
            return Err(e);
        }
        d.gpu_switching = Some(target);
        d.gpu_message = format!("Switching to {target}…");
        (d.root.clone(), st)
    };
    let worker_conn = conn.clone();
    let worker_shared = shared.clone();
    std::thread::Builder::new()
        .name("gpu-switch".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            zbus::block_on(emit_changed(&worker_conn, &worker_shared));
            let mute = MuteDisplaySwitchKey::new(&root);
            let result = switch(&root, &st, target);
            let boot_mux = lock(&worker_shared).boot_mux;
            let fresh = crate::state::Daemon::read_gpu_status(&root, boot_mux);
            {
                let mut d = lock(&worker_shared);
                d.gpu_cache = fresh;
                d.gpu_switching = None;
                match &result {
                    Ok(o) => {
                        if let Some(p) = policy {
                            d.gpu_policy = p;
                            d.save();
                        }
                        d.gpu_waiting = false;
                        d.gpu_message = o.message().to_owned();
                    }
                    Err(e) => {
                        d.gpu_waiting = automatic && e != NEEDS_FRESH_SESSION;
                        d.gpu_message = format!("Couldn't switch to {target}: {e}");
                    }
                }
                eprintln!("tspmod: gpu: {}", d.gpu_message);
            }
            zbus::block_on(emit_changed(&worker_conn, &worker_shared));
            drop(mute); // restores after a settle time, off the critical path
            if automatic {
                crate::watch::gpu_retry(&worker_conn, &worker_shared);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Bring the hardware in line with the policy (power-source change, logout, retry).
pub fn enforce(conn: &zbus::Connection, shared: &Shared) {
    let target = policy_target(&lock(shared));
    match target {
        Some(t) => {
            let _ = begin(conn.clone(), shared.clone(), t, None, true);
        }
        None => lock(shared).gpu_waiting = false,
    }
}
