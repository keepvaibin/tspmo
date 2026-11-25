//! `tspmo doctor`: read-only checks that explain what costs battery or breaks a feature,
//! each with the exact fix. Nothing here writes to the system.

use crate::machine::Machine;
use tspmo_sys::{SysRoot, backlight, dmi::Vendor, pci, usb};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// A feature is broken.
    Error,
    /// Costs battery or performance.
    Warn,
    /// Worth knowing; no action needed unless you want it.
    Info,
}

#[derive(Debug, Clone)]
pub struct Fix {
    pub summary: String,
    /// Shell commands, run in order, that apply the fix.
    pub commands: Vec<String>,
    pub needs_reboot: bool,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub id: &'static str,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub fix: Option<Fix>,
}

/// Run every check.
pub fn run(root: &SysRoot, m: &Machine) -> Vec<Finding> {
    let mut out = Vec::new();
    let checks: &[fn(&SysRoot, &Machine, &mut Vec<Finding>)] = &[
        oled_dpcd_backlight,
        duplicate_backlight,
        usb_hid_autosuspend,
        numlock_led,
        nvidia_runtime_pm,
        nvidia_unload_hung,
        nvidia_wakes,
        gtk_renderer,
        conflicting_daemons,
        tspmod_running,
        plymouth_stuck,
        charge_limit,
        eco_plus_limits,
        screen_tools,
        oled_flicker_free,
    ];
    for check in checks {
        check(root, m, &mut out);
    }
    out.sort_by_key(|f| f.severity);
    out
}

fn cmdline(root: &SysRoot) -> String {
    root.read_opt("/proc/cmdline").unwrap_or_default()
}

fn has_param(root: &SysRoot, key: &str) -> bool {
    cmdline(root)
        .split_whitespace()
        .any(|p| p.split('=').next() == Some(key))
}

/// Whether the bootloader configuration already carries a parameter (applied at next boot).
fn bootloader_has(root: &SysRoot, key: &str) -> bool {
    ["/etc/default/limine", "/etc/kernel/cmdline", "/etc/default/grub"]
        .iter()
        .filter_map(|p| root.read_opt(p))
        .any(|s| {
            s.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .any(|l| l.contains(key))
        })
}

/// Commands that add a kernel parameter persistently, for the bootloader in use.
fn add_kernel_param(root: &SysRoot, param: &str) -> Vec<String> {
    if root.exists("/etc/default/limine") {
        vec![
            format!("echo 'KERNEL_CMDLINE[default]+=\" {param}\"' | sudo tee -a /etc/default/limine"),
            "sudo limine-update".into(),
        ]
    } else if root.exists("/etc/kernel/cmdline") {
        vec![
            format!("sudo sed -i 's/$/ {param}/' /etc/kernel/cmdline"),
            "sudo mkinitcpio -P   # or your distro's UKI rebuild command".into(),
        ]
    } else if root.exists("/etc/default/grub") {
        vec![
            format!("sudo sed -i 's/^GRUB_CMDLINE_LINUX_DEFAULT=\"/&{param} /' /etc/default/grub"),
            "sudo grub-mkconfig -o /boot/grub/grub.cfg".into(),
        ]
    } else {
        vec![format!(
            "# add `{param}` to your bootloader's kernel command line"
        )]
    }
}

/// i915 OLED panels are dimmed over the eDP AUX channel (DPCD). Without
/// `i915.enable_dpcd_backlight=3` the driver drives a PWM line the panel ignores, so
/// brightness keys and sliders appear to do nothing.
fn oled_dpcd_backlight(root: &SysRoot, m: &Machine, out: &mut Vec<Finding>) {
    let Some(intel) = m.backlights.iter().find(|b| b.name == "intel_backlight") else {
        return;
    };
    if has_param(root, "i915.enable_dpcd_backlight") || has_param(root, "xe.enable_dpcd_backlight") {
        return;
    }
    let stuck_low = intel.percent() < 2.0;
    let is_oled_hint = m.dmi.vendor() == Vendor::Asus || stuck_low;
    if !is_oled_hint {
        return;
    }
    if bootloader_has(root, "i915.enable_dpcd_backlight") {
        out.push(Finding {
            id: "oled-dpcd-backlight",
            severity: Severity::Info,
            title: "Brightness fix is installed; reboot to activate it".into(),
            detail: "i915.enable_dpcd_backlight=3 is in the bootloader configuration but the \
                     running kernel was started without it."
                .into(),
            fix: None,
        });
        return;
    }
    out.push(Finding {
        id: "oled-dpcd-backlight",
        severity: if stuck_low {
            Severity::Error
        } else {
            Severity::Warn
        },
        title: "Screen brightness can't be controlled (OLED needs DPCD backlight)".into(),
        detail: format!(
            "intel_backlight is at {}/{} ({:.1}%). OLED panels take brightness over the eDP AUX \
             channel; i915 is using a control the panel ignores. The kernel log suggests \
             i915.enable_dpcd_backlight=3 (force the Intel DPCD interface).",
            intel.brightness,
            intel.max,
            intel.percent()
        ),
        fix: Some(Fix {
            summary: "Add i915.enable_dpcd_backlight=3 to the kernel command line".into(),
            commands: add_kernel_param(root, "i915.enable_dpcd_backlight=3"),
            needs_reboot: true,
        }),
    });
}

fn duplicate_backlight(_root: &SysRoot, m: &Machine, out: &mut Vec<Finding>) {
    let nv: Vec<&backlight::Backlight> = m.backlights.iter().filter(|b| b.is_nvidia()).collect();
    if nv.is_empty() || m.backlights.len() < 2 {
        return;
    }
    out.push(Finding {
        id: "duplicate-backlight",
        severity: Severity::Info,
        title: "The NVIDIA driver registers an extra backlight that controls nothing".into(),
        detail: format!(
            "{} is registered by the dGPU, but the built-in panel is wired to the iGPU. KDE and \
             GNOME match the backlight to the panel's connector so they ignore it, but simpler \
             tools (brightnessctl, some bars) may pick it. tspmo always uses the connector's own \
             backlight.",
            nv.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", ")
        ),
        fix: None,
    });
}

/// Internal vendor HID devices left at `power/control=on` keep the USB controller awake,
/// which blocks the deepest package C-state. Measured on a GU605MI: ~0.8 W at screen-off idle.
fn usb_hid_autosuspend(root: &SysRoot, _m: &Machine, out: &mut Vec<Finding>) {
    let awake: Vec<usb::UsbDevice> = crate::usbpm::internal_devices(root)
        .into_iter()
        .filter(|d| !d.autosuspend_enabled())
        .collect();
    if awake.is_empty() {
        return;
    }
    let list = awake
        .iter()
        .map(|d| format!("{} ({})", d.id(), d.name.as_deref().unwrap_or("?")))
        .collect::<Vec<_>>()
        .join(", ");
    let safe = crate::usbpm::autosuspend_safe(root);
    out.push(Finding {
        id: "usb-hid-autosuspend",
        severity: Severity::Warn,
        title: "Internal keyboard/lighting USB chips never sleep".into(),
        detail: format!(
            "{list} stay powered, which keeps the CPU out of its deepest idle state (about \
             0.6 W at light use on a GU605MI). {}",
            if safe {
                "The patched keyboard driver is loaded; tspmod enables autosuspend at start-up."
            } else {
                "Letting them sleep needs tspmo's patched keyboard driver (tspmo-hid-asus-dkms): \
                 the stock one can't change the keyboard backlight while it sleeps."
            }
        ),
        fix: Some(Fix {
            summary: if safe {
                "Restart tspmod to apply it".into()
            } else {
                "Install the patched keyboard driver, then reload it".into()
            },
            commands: if safe {
                vec!["sudo systemctl restart tspmod".into()]
            } else {
                vec!["sudo pacman -S --needed dkms   # then install tspmo-hid-asus-dkms".into()]
            },
            needs_reboot: false,
        }),
    });
}

fn numlock_led(root: &SysRoot, m: &Machine, out: &mut Vec<Finding>) {
    if m.dmi.vendor() == Vendor::Other {
        return;
    }
    let lit = root
        .glob("/sys/class/leds/input*::numlock/brightness")
        .iter()
        .any(|p| root.read_parse::<u32>(p).is_some_and(|v| v > 0));
    if lit {
        out.push(Finding {
            id: "numlock-led",
            severity: Severity::Info,
            title: "Num Lock is on, which keeps the keyboard from suspending".into(),
            detail: "USB HID won't autosuspend a keyboard while one of its LEDs is lit.".into(),
            fix: Some(Fix {
                summary: "Turn Num Lock off (and disable 'Num Lock on at login' in your desktop settings)"
                    .into(),
                commands: vec![],
                needs_reboot: false,
            }),
        });
    }
}

fn nvidia_runtime_pm(root: &SysRoot, m: &Machine, out: &mut Vec<Finding>) {
    for gpu in m.gpus.iter().filter(|g| g.vendor == pci::VENDOR_NVIDIA) {
        if gpu.driver.as_deref() != Some("nvidia") {
            continue;
        }
        let dpm: Option<u32> = root.read_parse("/sys/module/nvidia/parameters/NVreg_DynamicPowerManagement");
        // 0 = disabled, 1 = coarse, 2 = fine-grained, 3 = driver default (fine-grained on Turing+).
        let dpm_ok = matches!(dpm, Some(2 | 3));
        let control_ok = gpu.pm_control.as_deref() == Some("auto");
        if dpm_ok && control_ok {
            continue;
        }
        out.push(Finding {
            id: "nvidia-rtd3",
            severity: Severity::Warn,
            title: format!("The NVIDIA GPU ({}) can't power off when idle", gpu.address),
            detail: format!(
                "NVreg_DynamicPowerManagement={} and power/control={}. Both are needed for the \
                 dGPU to reach D3cold.",
                dpm.map_or("unset".into(), |v| v.to_string()),
                gpu.pm_control.as_deref().unwrap_or("?")
            ),
            fix: Some(Fix {
                summary: "Enable fine-grained runtime D3".into(),
                commands: vec![
                    "echo 'options nvidia NVreg_DynamicPowerManagement=0x02' | sudo tee /etc/modprobe.d/tspmo-nvidia-pm.conf".into(),
                    "echo 'ACTION==\"bind\", SUBSYSTEM==\"pci\", ATTR{vendor}==\"0x10de\", ATTR{class}==\"0x03[0-9]*\", TEST==\"power/control\", ATTR{power/control}=\"auto\"' | sudo tee /etc/udev/rules.d/80-tspmo-nvidia-pm.rules".into(),
                ],
                needs_reboot: true,
            }),
        });
    }
}

/// `rmmod nvidia` hung inside the kernel (NVIDIA's ACPI notify handler against the unload: an
/// ABBA on the RM lock). The task cannot be killed and the driver is half gone: only a restart
/// brings the GPU back, and tspmod refuses GPU switches until then.
fn nvidia_unload_hung(root: &SysRoot, _m: &Machine, out: &mut Vec<Finding>) {
    if !root.exists(crate::gpu::RESTART_MARKER) {
        return;
    }
    out.push(Finding {
        id: "nvidia-unload-hung",
        severity: Severity::Error,
        title: crate::gpu::RESTART_REQUIRED.into(),
        detail: "Unloading the NVIDIA driver for Eco hung in the kernel (an `rmmod` task in D \
                 state that cannot be killed). GPU switches are refused until the next boot."
            .into(),
        fix: Some(Fix {
            summary: "Restart the computer".into(),
            commands: vec!["systemctl reboot".into()],
            needs_reboot: true,
        }),
    });
}

/// Why an NVIDIA GPU that should be asleep is awake: the apps holding it, a desktop that
/// loaded its driver, or the firmware's own periodic notification. Names the culprit.
fn nvidia_wakes(root: &SysRoot, m: &Machine, out: &mut Vec<Finding>) {
    for gpu in m.gpus.iter().filter(|g| g.vendor == pci::VENDOR_NVIDIA) {
        if gpu.driver.as_deref() != Some("nvidia") || gpu.pm_control.as_deref() != Some("auto") {
            continue;
        }
        let users = crate::gpu::users(root, Some(&gpu.address));
        let apps: Vec<String> = users
            .iter()
            .filter(|u| u.kind == crate::gpu::UseKind::Render)
            .filter(|u| {
                !crate::gpu::NVIDIA_SERVICES
                    .iter()
                    .any(|(comm, _)| u.name == *comm)
            })
            .map(|u| format!("{} ({})", u.name, u.pid))
            .collect();
        let desktop_driver = users
            .iter()
            .find(|u| u.kind == crate::gpu::UseKind::Display && u.driver_handles);
        if !apps.is_empty() && !gpu.is_asleep() {
            out.push(Finding {
                id: "nvidia-held",
                severity: Severity::Warn,
                title: format!("NVIDIA GPU held awake by {}", apps.join(", ")),
                detail: "Apps started with the NVIDIA GPU keep it awake while they run. Close them, \
                         or start them on the iGPU."
                    .into(),
                fix: None,
            });
        }
        if let Some(d) = desktop_driver {
            out.push(Finding {
                id: "compositor-nvidia",
                severity: Severity::Info,
                title: format!("{} loaded the NVIDIA driver", d.name),
                detail: "The desktop started before tspmo's compositor setting existed, so Eco can't \
                         unload the driver. Log out and in once."
                    .into(),
                fix: None,
            });
        }
        // Nothing holds it, yet it is awake much of the time: the platform wakes it.
        if apps.is_empty() && gpu.awake_share().is_some_and(|s| s > 0.15) {
            let acpi = root.exists("/sys/bus/acpi/devices/NVDA0820:00");
            let pct = (gpu.awake_share().unwrap_or(0.0) * 100.0).round();
            out.push(Finding {
                id: "nvidia-nvpcf",
                severity: Severity::Warn,
                title: format!("NVIDIA GPU awake {pct}% of the time with nothing using it"),
                detail: if acpi {
                    "The firmware's NVPCF device (ACPI NVDA0820) notifies the driver about every 22 s; \
                     each time the driver resumes the GPU for about 25 s. No app or tspmo causes it. \
                     Eco turns the GPU off."
                        .into()
                } else {
                    "Something wakes it about every 22 s and no process holds it. Eco turns the GPU off."
                        .into()
                },
                fix: Some(Fix {
                    summary: "Turn the NVIDIA GPU off (Eco)".into(),
                    commands: vec!["tspmo gpu eco".into()],
                    needs_reboot: false,
                }),
            });
        }
    }
}

/// GTK 4.14+ defaults to a Vulkan renderer that can enumerate (and wake) the NVIDIA GPU.
fn gtk_renderer(root: &SysRoot, m: &Machine, out: &mut Vec<Finding>) {
    if !m.has_nvidia() {
        return;
    }
    let set = root
        .glob("/etc/environment.d/*.conf")
        .into_iter()
        .chain(root.glob("/usr/lib/environment.d/*.conf"))
        .chain(["/etc/environment".to_owned()])
        .filter_map(|p| root.read_opt(p))
        .any(|s| s.lines().any(|l| l.trim_start().starts_with("GSK_RENDERER=")));
    if set || root.exists("/usr/lib/systemd/user-environment-generators/60-tspmo") {
        return;
    }
    out.push(Finding {
        id: "gtk-renderer",
        severity: Severity::Info,
        title: "GTK 4 apps may wake the NVIDIA GPU at launch".into(),
        detail: "GTK's Vulkan renderer enumerates every GPU, which resumes a sleeping dGPU \
                 (about +2.6 s launch time and a power spike on a GU605MI). The OpenGL renderer \
                 stays on the iGPU."
            .into(),
        fix: Some(Fix {
            summary: "Use GTK's OpenGL renderer system-wide".into(),
            commands: vec!["echo 'GSK_RENDERER=ngl' | sudo tee /etc/environment.d/50-tspmo-gtk.conf".into()],
            needs_reboot: false,
        }),
    });
}

fn running(root: &SysRoot, names: &[&str]) -> Vec<String> {
    let mut found: Vec<String> = root
        .glob("/proc/*/comm")
        .into_iter()
        .filter_map(|p| root.read_opt(p))
        .filter(|c| names.contains(&c.as_str()))
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

fn conflicting_daemons(root: &SysRoot, _m: &Machine, out: &mut Vec<Finding>) {
    let found = running(
        root,
        &[
            "asusd",
            "legiond",
            "auto-cpufreq",
            "tlp",
            "power-profiles-",
            "tuned",
            "system76-power",
            "tccd",
        ],
    );
    let tlp_installed = root.exists("/etc/tlp.conf") && root.exists("/usr/bin/tlp");
    if found.is_empty() && !tlp_installed {
        return;
    }
    let mut names = found;
    if tlp_installed {
        names.push("tlp (installed)".into());
    }
    out.push(Finding {
        id: "other-power-tools",
        severity: Severity::Info,
        title: "Other power/vendor tools are present".into(),
        detail: format!(
            "{}. tspmod will defer to these for the controls they own instead of fighting them.",
            names.join(", ")
        ),
        fix: None,
    });
}

fn tspmod_running(root: &SysRoot, _m: &Machine, out: &mut Vec<Finding>) {
    if !running(root, &["tspmod"]).is_empty() || !root.is_real() {
        return;
    }
    let installed = root.exists("/usr/lib/systemd/system/tspmod.service");
    out.push(Finding {
        id: "tspmod",
        severity: Severity::Warn,
        title: "The tspmo daemon isn't running".into(),
        detail: "Without tspmod nothing tunes power profiles, follows AC/battery changes or \
                 switches GPU modes, and the desktop's power-profile switch has no backend."
            .into(),
        fix: Some(Fix {
            summary: if installed {
                "Start it".into()
            } else {
                "Install the package, then start it".into()
            },
            commands: if installed {
                vec!["sudo systemctl enable --now tspmod.service".into()]
            } else {
                vec![
                    "cd packaging/arch && makepkg -si".into(),
                    "sudo systemctl enable --now tspmod.service".into(),
                ]
            },
            needs_reboot: false,
        }),
    });
}

/// `plymouth quit` can hang on hybrid-GPU laptops, leaving plymouthd running all session
/// with every GPU's DRM device open (it then keeps the dGPU awake and blocks Eco mode).
fn plymouth_stuck(root: &SysRoot, _m: &Machine, out: &mut Vec<Finding>) {
    if running(root, &["plymouthd"]).is_empty() {
        return;
    }
    // Still booting? A graphical session existing means boot is long over.
    let session_up = !running(root, &["kwin_wayland", "gnome-shell", "Hyprland", "sway", "niri"]).is_empty();
    if !session_up {
        return;
    }
    let dropin = root.exists("/usr/lib/systemd/system/plymouth-quit.service.d/50-tspmo.conf");
    let mut commands = vec!["sudo pkill -x plymouthd".to_owned()];
    if !dropin {
        commands.insert(
            0,
            "sudo install -Dm644 data/systemd/plymouth-quit-timeout.conf /etc/systemd/system/plymouth-quit.service.d/50-tspmo.conf".into(),
        );
    }
    out.push(Finding {
        id: "plymouth-stuck",
        severity: Severity::Warn,
        title: "The boot splash (plymouthd) never exited".into(),
        detail: "plymouth-quit timed out at boot, so plymouthd has been running all session \
                 with the GPUs' display devices open. It wastes CPU, can keep the NVIDIA GPU \
                 awake and blocks switching to Eco."
            .into(),
        fix: Some(Fix {
            summary: if dropin {
                "Stop it now (tspmo's plymouth-quit timeout fixes it from the next boot)".into()
            } else {
                "Make plymouth-quit time out quickly, and stop it now".into()
            },
            commands,
            needs_reboot: false,
        }),
    });
}

fn charge_limit(_root: &SysRoot, m: &Machine, out: &mut Vec<Finding>) {
    for b in &m.batteries {
        if b.charge_end == Some(100) {
            out.push(Finding {
                id: "charge-limit",
                severity: Severity::Info,
                title: format!("{} charges to 100%", b.name),
                detail: "Fine when you need maximum runtime. Keeping it at 80% when you're mostly \
                         plugged in slows battery wear considerably."
                    .into(),
                fix: None,
            });
        }
    }
}

/// Eco+ caps the CPU through RAPL. Intel kernels expose it as `intel-rapl`; without it Eco+
/// still lowers clocks and refresh rate but cannot cap power.
fn eco_plus_limits(root: &SysRoot, _m: &Machine, out: &mut Vec<Finding>) {
    let intel = root.exists("/sys/devices/system/cpu/intel_pstate");
    if !intel || !crate::power::rapl_zones(root).is_empty() {
        return;
    }
    out.push(Finding {
        id: "eco-plus-rapl",
        severity: Severity::Info,
        title: "Eco+ can't cap CPU power: no RAPL zones".into(),
        detail: "/sys/class/powercap has no package or platform power zone. Eco+ still lowers the refresh rate \
                 and uses the firmware quiet mode."
            .into(),
        fix: Some(Fix {
            summary: "Load the RAPL driver".into(),
            commands: vec!["sudo modprobe intel_rapl_msr".into()],
            needs_reboot: false,
        }),
    });
}

/// Refresh rate, flicker-free dimming and colour modes go through KDE's `kscreen-doctor`.
fn screen_tools(root: &SysRoot, _m: &Machine, out: &mut Vec<Finding>) {
    if !root.exists("/usr/bin/kwin_wayland") || root.exists("/usr/bin/kscreen-doctor") {
        return;
    }
    out.push(Finding {
        id: "kscreen-doctor",
        severity: Severity::Warn,
        title: "kscreen-doctor is missing".into(),
        detail: "tspmo sets the refresh rate, dimming and colour mode of a KDE desktop through \
                 kscreen-doctor (package libkscreen)."
            .into(),
        fix: Some(Fix {
            summary: "Install libkscreen".into(),
            commands: vec!["sudo pacman -S --needed libkscreen".into()],
            needs_reboot: false,
        }),
    });
}

/// OLED panels flicker at low backlight levels; tspmo keeps the backlight above a floor and
/// dims the picture instead. It is on by default for OLED: tell the user if it was turned off.
fn oled_flicker_free(root: &SysRoot, m: &Machine, out: &mut Vec<Finding>) {
    let panel = crate::display::internal_panel(root);
    let oled = crate::display::edid::is_oled(panel.as_ref().and_then(|p| p.edid.as_ref()), m.dmi.model());
    if !oled {
        return;
    }
    let setting = root
        .read_opt("/var/lib/tspmo/display")
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("flicker_free=").map(|v| v.trim().to_owned()))
        })
        .unwrap_or_else(|| "auto".into());
    if setting != "off" {
        return;
    }
    out.push(Finding {
        id: "flicker-free-off",
        severity: Severity::Info,
        title: "Flicker-free dimming is off on an OLED panel".into(),
        detail: "At low brightness OLED panels dim with pulse-width modulation, which flickers. \
                 With flicker-free dimming the backlight stays above the panel's floor and the \
                 picture is darkened instead."
            .into(),
        fix: Some(Fix {
            summary: "Turn it on".into(),
            commands: vec!["tspmo display dimming auto".into()],
            needs_reboot: false,
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Machine;

    fn fixture() -> SysRoot {
        SysRoot::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/asus-gu605mi"
        ))
    }

    fn ids(root: &SysRoot) -> Vec<&'static str> {
        run(root, &Machine::probe(root)).iter().map(|f| f.id).collect()
    }

    #[test]
    fn a_hung_driver_unload_is_an_error_with_a_restart_fix() {
        let d = std::env::temp_dir().join(format!("tspmo-doctor-hung-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let root = SysRoot::new(&d);
        let m = Machine::probe(&root);
        let mut out = Vec::new();
        nvidia_unload_hung(&root, &m, &mut out);
        assert!(out.is_empty());
        let marker = d.join(crate::gpu::RESTART_MARKER.trim_start_matches('/'));
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "rmmod hung\n").unwrap();
        nvidia_unload_hung(&root, &m, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "nvidia-unload-hung");
        assert_eq!(out[0].severity, Severity::Error);
        assert_eq!(out[0].title, "Restart to recover the NVIDIA GPU");
        assert!(out[0].fix.as_ref().unwrap().needs_reboot);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn gu605_has_rapl_and_an_oled_with_flicker_free_on() {
        let ids = ids(&fixture());
        assert!(!ids.contains(&"eco-plus-rapl"));
        assert!(!ids.contains(&"flicker-free-off"));
    }

    #[test]
    fn missing_rapl_is_reported_on_intel() {
        let d = std::env::temp_dir().join(format!("tspmo-doctor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sys/devices/system/cpu/intel_pstate")).unwrap();
        let root = SysRoot::new(&d);
        let m = Machine::probe(&root);
        let mut out = Vec::new();
        eco_plus_limits(&root, &m, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "eco-plus-rapl");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn flicker_free_off_is_reported_for_oled_only() {
        let d = std::env::temp_dir().join(format!("tspmo-doctor-ff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = fixture();
        // A copy of the panel's connector with the setting file added.
        let drm = d.join("sys/class/drm/card1-eDP-1");
        std::fs::create_dir_all(&drm).unwrap();
        std::fs::write(drm.join("status"), "connected\n").unwrap();
        std::fs::copy(f.path("/sys/class/drm/card1-eDP-1/edid"), drm.join("edid")).unwrap();
        std::fs::create_dir_all(d.join("var/lib/tspmo")).unwrap();
        std::fs::write(d.join("var/lib/tspmo/display"), "flicker_free=off\n").unwrap();
        let root = SysRoot::new(&d);
        let m = Machine::probe(&root);
        let mut out = Vec::new();
        oled_flicker_free(&root, &m, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].fix.as_ref().unwrap().commands,
            ["tspmo display dimming auto"]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    fn awake_nvidia(tag: &str, active: u64, suspended: u64, holder: bool) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tspmo-doctor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let dev = d.join("sys/bus/pci/devices/0000:01:00.0");
        std::fs::create_dir_all(dev.join("power")).unwrap();
        for (f, v) in [
            ("class", "0x030200\n"),
            ("vendor", "0x10de\n"),
            ("power/runtime_status", "active\n"),
            ("power/control", "auto\n"),
        ] {
            std::fs::write(dev.join(f), v).unwrap();
        }
        std::fs::write(dev.join("power/runtime_active_time"), format!("{active}\n")).unwrap();
        std::fs::write(dev.join("power/runtime_suspended_time"), format!("{suspended}\n")).unwrap();
        std::fs::create_dir_all(d.join("sys/bus/pci/drivers/nvidia")).unwrap();
        std::os::unix::fs::symlink("../../../bus/pci/drivers/nvidia", dev.join("driver")).unwrap();
        std::fs::create_dir_all(d.join("sys/bus/acpi/devices/NVDA0820:00")).unwrap();
        if holder {
            let fd = d.join("proc/4242/fd");
            std::fs::create_dir_all(&fd).unwrap();
            std::fs::write(d.join("proc/4242/comm"), "firefox\n").unwrap();
            std::os::unix::fs::symlink("/dev/nvidia0", fd.join("7")).unwrap();
        }
        d
    }

    #[test]
    fn an_awake_nvidia_gpu_with_no_holder_is_blamed_on_the_firmware() {
        let d = awake_nvidia("nvpcf", 600, 400, false);
        let root = SysRoot::new(&d);
        let m = Machine::probe(&root);
        let mut out = Vec::new();
        nvidia_wakes(&root, &m, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].id, "nvidia-nvpcf");
        assert!(out[0].detail.contains("NVDA0820"));
        assert_eq!(out[0].fix.as_ref().unwrap().commands, ["tspmo gpu eco"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_app_holding_the_nvidia_gpu_is_named() {
        let d = awake_nvidia("held", 600, 400, true);
        let root = SysRoot::new(&d);
        let m = Machine::probe(&root);
        let mut out = Vec::new();
        nvidia_wakes(&root, &m, &mut out);
        assert!(
            out.iter()
                .any(|f| f.id == "nvidia-held" && f.title.contains("firefox")),
            "{out:?}"
        );
        assert!(!out.iter().any(|f| f.id == "nvidia-nvpcf"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_gpu_that_sleeps_is_not_reported() {
        let d = awake_nvidia("asleep", 50, 950, false);
        let root = SysRoot::new(&d);
        let m = Machine::probe(&root);
        let mut out = Vec::new();
        nvidia_wakes(&root, &m, &mut out);
        assert!(out.is_empty(), "{out:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
