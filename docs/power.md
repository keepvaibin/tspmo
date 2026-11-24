# Power modes

Four modes, chosen per power source (AC, battery): **eco-plus**, **power-saver**, **balanced**,
**performance**. KDE, GNOME and `powerprofilesctl` see three: Eco+ reads as `power-saver`.
Choosing `power-saver` there while Eco+ is active keeps Eco+; any other choice leaves it.
`tspmo profile` (or `org.tspmo.Daemon1.SetProfile`) uses all four.

## What a mode changes

| | Eco+ | power-saver | balanced | performance |
|---|---|---|---|---|
| Firmware thermal mode | quiet | quiet | balanced | performance |
| CPU energy preference / bias | power / 15 | power / 15 | balance_power or balance_performance | performance / 0 |
| Turbo | off | off on battery | on | on |
| Package power limit (RAPL) | 5 W / 7 W | firmware | firmware | firmware |
| Uncore maximum clock | 1.6 GHz | firmware | firmware | firmware |
| Screen | 60 Hz | refresh policy | refresh policy | refresh policy |

On battery every mode also gets PCIe link power saving, audio codec power-down and lazier disk
writeback. All writes skip values that are already set, because on ASUS each `platform_profile`
write makes the EC notify the NVIDIA driver and wakes the dGPU.

Eco+ saves the original RAPL and uncore values in `/run/tspmo/rapl-orig` when it starts and
writes them back when it ends, when `tspmod` stops (`ExecStopPost`) and after a daemon crash (the
file is read again at start-up).

## How Eco+ was tuned

Hardware: ROG Zephyrus G16 GU605MI, Core Ultra 9 185H, 2560x1600 OLED, 72 Wh battery.

Method: an interleaved A/B harness (`tools/bench/`, now `tspmo bench`). A light 10 Hz burst load,
two workers doing a fixed 5.4 ms chunk, logs the time each chunk took. Every condition is power-saver
with turbo off and energy preference `power`. Per condition: 8 s to settle, 20 s measured with
`turbostat`. The criterion: light work at most 1.5x slower, desktop smooth.

**These runs were on AC with charging stopped** (the charge limit set to 60% while the battery was
above it), because the laptop could not be unplugged during the work. "psys" is the RAPL platform
counter (`SysWatt`), "pkg" the CPU package counter. Neither is battery draw; psys reads the whole
platform on AC and is noisy (a background browser moves it by a watt or more), so treat differences
below about 0.5 W as noise. Your own battery numbers: `sudo tspmo bench --hold-charge`, or unplugged
(`tspmo bench` then reads `power_now` and reports a battery column).

| Condition (all 60 Hz unless noted) | psys W | pkg W | chunk ms (median / p95) |
|---|---|---|---|
| 240 Hz | 18.1 | 3.27 | 5.35 / 6.00 |
| 60 Hz (baseline) | 5.5 | 2.39 | 5.37 / 6.11 |
| pkg 5/7 W | 5.4 | 2.36 | 5.41 / 5.91 |
| pkg 8/12 W | 5.3 | 2.34 | 5.37 / 5.93 |
| pkg 5/7 W + uncore 1.2 GHz | 5.0 | 2.22 | 5.37 / 5.95 |
| uncore 1.6 GHz | 5.3 | 2.32 | 5.40 / 5.98 |
| E-cores only (`AllowedCPUs=12-21`) | 8.7 | 3.12 | 6.61 / 6.73 |
| psys 7/9 W | n/a | n/a | 25.2 / 27.8 (4.7x) |
| psys 8/11 W | n/a | n/a | 25.2 / 26.8 (4.7x) |
| psys 9/12 W | n/a | n/a | 25.2 / 26.4 (4.7x) |
| pkg 6/8 W | n/a | n/a | 5.44 / 5.98 |

Rows marked n/a were measured while the charger was still charging (the limit was set to 40%, which
the firmware ignores): only their latency columns are valid.

Caveat: two later `tspmo bench` runs of Eco+ (pkg 5/7) showed a p95 chunk time of 95 ms against
12.5 ms median, while other agents were compiling; not reproduced on a quiet machine, so the
5/7 W default needs re-measuring (see handoff-power.md).

What this says:

- **60 Hz is the big lever.** The platform power at 240 Hz is about 12 W higher on AC. The user's
  earlier battery measurement (10 Hz burst load) was 7.65 W at 240 Hz and 7.37 W at 60 Hz, so the
  battery gain is smaller than the AC number; AC psys includes the effect of a higher GPU clock.
- **Package caps cost nothing on light work** and do not change idle power either: the package
  averages 2.2 to 2.4 W here, below any cap tried. They bind on heavy work, which is what Eco+ is
  for. The user's target of about 5 W sustained and 7 W burst is met at the package, so Eco+ uses
  5/7 W. A single-thread 4M-iteration Python burst took 0.33 s with and without the cap.
- **A platform (psys) cap does not work for this goal.** The idle platform already draws 5.5 to
  7.6 W, so any psys limit at or below 12 W pins the CPU at 400 MHz and light work becomes 4.7x
  slower. Eco+ leaves psys alone (`psys_w` can be set in the state file).
- **Uncore 1.2 to 1.6 GHz** shows 0.2 to 0.5 W lower psys with no latency change, but that is within
  the noise. Eco+ uses the conservative 1.6 GHz.
- **E-core lassoing loses.** Running everything on the 8 E-cores plus 2 LP-E cores made light work
  1.23x slower and raised power (8.7 W psys against 5.5 W): the E-cores run at a higher clock for
  longer. Not used. (The user's earlier LP-E-only result, 2.2x slower, is the other end.)
- Turbo off is part of power-saver on battery and of Eco+ always.

## Tunables

`/var/lib/tspmo/state` (edit while `tspmod` is stopped):

```
eco_pkg_w=5,7        # long-term, short-term watts, or "off"
eco_psys_w=off
eco_uncore_mhz=1600  # or "off"
```

## Topology

`tspmo_core::topology` classifies cores as performance, efficiency and low-power efficiency (no L3)
from `/sys/devices/cpu_core` and `cpu_atom` on Intel hybrid CPUs, and by maximum clock on others.
The bench uses it for `cpus=e|lpe|p`.

## Known hazards

Two incidents on the GU605MI, both caused by tspmo touching the NVIDIA GPU at the wrong time.
Both are fixed; the rules stay.

**1. Reading the dGPU wakes it, and a wake at the wrong moment kills it.** Reading
`/sys/class/backlight/nvidia_0/brightness` (or `actual_brightness`) goes through
`nvkms_get_backlight_brightness` and runtime-resumes the GPU. At 03:18 such a read coincided with
a power-source event; the driver's GSP timed out (Xid 119/154) and the RTX 4070 was gone until
reboot. Rules now:

- `tspmo_sys::backlight::list` lists NVIDIA backlights by name and type only (`sampled = false`,
  values never read); `hwmon::list` skips hwmon devices that sit on an NVIDIA GPU;
  `display::internal_panel` reads only the connectors of the GPU that drives the panel;
  `tspmo dump` copies nothing from NVIDIA backlights, hwmon, DRM connectors or `/proc/driver/nvidia`
  (`pci::touches_dgpu`).
- Of an NVIDIA PCI device only `vendor`, `device`, `class`, `boot_vga`, `power_state`,
  `power/control` and `power/runtime_status` are read, and `/proc/<pid>/fd` links. Nothing
  reads `/proc/driver/nvidia/*`, the dGPU's `hwmon`, or its connectors' `status`, `edid`, `modes`.
- Tests: a poisoned (mode 000) `nvidia_0/brightness`, hwmon `name` and dump source must not be read.

**2. Emergency mode at boot.** CachyOS's `drm-module-awaiter-generator` creates
`drm-module-load.target` (`JobTimeoutSec=60`, `OnFailure=emergency.target`,
`Before=display-manager.service`) whenever an NVIDIA GPU exists at generator time; it waits for
`sys-module-nvidia_drm.device`. On battery with policy Optimized, `tspmod` switched to Eco at
start-up, before `nvidia_drm` had loaded: "NVRM: No NVIDIA GPU found", the target timed out, the
system went to emergency mode. Rules now:

- At start-up `tspmod` applies the power profile and the compositor environment at once, but not
  the GPU policy (`boot.rs`). It waits on systemd's `JobRemoved` signals (no polling) for
  `drm-module-load.target` to be `active` (or `failed`), else `display-manager.service`, else
  `graphical.target`, then runs the normal live switch. The upper bound is 5 minutes.
- Never while the driver is still loading: with an NVIDIA GPU present and `nvidia_drm` not loaded,
  and the awaiter target or the `nvidia` module present, the policy is held back even after the
  bound.
- GPU changes asked for before then (`SetGpuMode`) get "The system is still starting; try again
  in a minute."
- The unit keeps `Before=display-manager.service` for the compositor environment only.
