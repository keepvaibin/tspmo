# Power modes

Four modes, chosen per power source (AC, battery): **eco-plus**, **power-saver**, **balanced**,
**performance**. KDE, GNOME and `powerprofilesctl` see three: Eco+ reads as `power-saver`.
Choosing `power-saver` there while Eco+ is active keeps Eco+; any other choice leaves it.
`tspmo profile` (or `org.tspmo.Daemon1.SetProfile`) uses all four.

## What a mode changes

| | Eco+ | power-saver | balanced | performance |
|---|---|---|---|---|
| Firmware thermal mode | quiet | quiet | balanced | performance |
| CPU energy preference / bias | balance_performance / 8 | power / 15 | balance_power or balance_performance | performance / 0 |
| Turbo | on | off on battery | on | on |
| Package power limit (RAPL) | PL1 12 W over 10 s, PL2 28 W | firmware | firmware | firmware |
| Efficiency-core maximum clock | 1.2 GHz | firmware | firmware | firmware |
| Background jobs | on the efficiency cores, weight 10 | | | |
| NVIDIA GPU | off (Eco) | policy | policy | policy |
| Fans | stopped below 58-66 C, steep ramp | firmware | firmware | firmware |
| Keyboard backlight | dark after 10 s idle | as set | as set | as set |
| Slash | dark | as set | as set | as set |
| Screen | 60 Hz | refresh policy | refresh policy | refresh policy |

On battery every mode also gets PCIe link power saving, audio codec power-down and lazier disk
writeback. All writes skip values that are already set, because on ASUS each `platform_profile`
write makes the EC notify the NVIDIA driver and wakes the dGPU (the driver then logs
`NVRM: ... failed to get platform power mode from SBIOS` assertions: harmless, and absent once
the GPU is off).

Eco+ saves the original RAPL limits and windows and the efficiency cores' clock in
`/run/tspmo/rapl-orig` when it starts and writes them back when it ends, when `tspmod` stops
(`ExecStopPost`) and after a daemon crash (the file is read again at start-up).

## Eco+ design

Eco+ is the maximum-battery mode, selectable on AC and on battery. It must also stay snappy:
the first design (package 5/7 W flat, turbo off, uncore 1.6 GHz) made the laptop unusable
while a build ran. A power limit slows every core alike, so a build on all cores starved the
foreground of both budget and clock. Eco+ now separates the two:

1. **Latency for the foreground.** Turbo stays, the energy hint is `balance_performance`
   (a raw number, even 128, measured 1.65x slower: use the kernel's names). Bursts finish at
   full clock and the cores idle again.
2. **Background work is confined.** The *guard* (`crates/tspmod/src/guard.rs`) moves heavy jobs
   to `tspmobg.slice`: efficiency cores only, CPU weight 10, clock capped at 1.2 GHz. The
   compositor, shell and every graphical app that is not a terminal are never moved. Terminals
   (where builds run), services, login sessions and the indexer are. A job is moved from its
   root (the `make`, not the shell), with everything it starts afterwards.
3. **A sustained budget, not a cap.** PL1 12 W averaged over 10 s, PL2 28 W. Ten efficiency
   cores at 1.2 GHz draw about 3 W over the idle package, so the budget is never hit by background
   work and bursts are never throttled.

The guard samples `/proc/stat` every 4 s while Eco+ is active (nothing otherwise) and scans
processes only while more than half a core is busy. It reacts after about 6 s. Everything goes
back when Eco+ ends or the daemon stops (`/run/tspmo/guard` remembers where each process came
from).

Fans: a custom curve per fan while Eco+ is active (`fans::eco_curve`). Off up to four degrees
past the first step of the firmware's own quiet curve, clamped to 58-66 C (the idle laptop at 5 W
settles near 57 C with the fans stopped), then 12 % within 3 C and 100 % from Tjmax - 20 C
(90 C); from 75 C never below the firmware's curve. Stopping needs 0 % to be honoured by the EC:
it is (measured: 0 rpm on all three fans).

### Measured (AC, charging held, 4 rounds interleaved, `tspmo bench`)

The load is 2 workers doing a 5.4 ms chunk at 10 Hz ("burst"); `--bg 20` adds 20 busy threads
as a service outside the desktop. p95 ratio is against balanced at 60 Hz in the same run.
The machine was in use meanwhile (other builds), so absolute watts carry noise; the ratios are
what the interleaving protects.

| Condition | burst median / p95 ms | p95 vs balanced | wake-up p95 us | package W | psys W |
|---|---|---|---|---|---|
| balanced | 7.03 / 7.72 | 1.00x | 58 | 6.2 | 31.0 |
| power-saver | 12.32 / 12.95 | 1.68x | 75 | 4.1 | 29.0 |
| **Eco+** | 6.94 / 7.32 | **0.95x** | 67 | 6.1 | 27.3 |
| balanced, 20 background threads | 8.46 / 10.62 | 1.00x | 55 | 24.2 | 49.6 |
| power-saver, 20 background threads | 9.87 / 10.93 | 1.03x | 57 | 19.6 | 44.1 |
| **Eco+, 20 background threads** | 6.53 / 7.30 | **0.69x** | 67 | 8.8 | 26.9 |
| first Eco+ (5/7 W), 20 background threads | 97 / 197 | 6.8x | 181 | 7.8 | 36.7 |

Acceptance (interactive p95 at most 1.3x balanced, with and without background load): met,
0.95x and 0.69x. With the background load Eco+ draws a third of balanced's package power.

Tuning runs that led here (same method, noisier):

- energy hint: `balance_performance` 6.9 ms; `balance_power` 12.3 ms (1.56x); raw 112 to 176 12.2 ms.
- background confined to efficiency cores at weight 20, clock cap 1.2 GHz, PL1 15 W: p95 1.14x
  of an unprotected balanced run; at PL1 8 W or 12 W the budget binds and the foreground slows
  2x to 2.5x. Hence the clock cap on the efficiency cores instead of a low PL1.
- power of ten efficiency cores kept busy (package, whole machine in light use): 7.0 W at
  1.2 GHz, 9.8 W at 2.0 GHz, 16.5 W uncapped.
- background on the two LP-E cores only: p95 1.35x, no better than the ten cores at 1.2 GHz.

## First Eco+ design (superseded)

Kept for the numbers: these runs led to the first, flat 5/7 W design.

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

`/var/lib/tspmo/state` (edit while `tspmod` is stopped; Eco+ values are read only with
`eco_version=2`, which `tspmod` writes):

```
eco_version=2
eco_pkg_w=12,28          # PL1, PL2 watts, or "off"
eco_pkg_window_ms=10000  # PL1 averaging window
eco_psys_w=off
eco_uncore_mhz=off       # or a clock
eco_ecap_mhz=1200        # efficiency cores' maximum clock, or "off"
eco_epp=balance_performance
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

**3. The NVIDIA GPU is awake about 20 s in every 30 s in Hybrid, with `tspmod` stopped too.**
Not tspmo, not an app. Traced with the kernel's runtime-PM events (`rpm_resume` with a stack
trigger on `0000:01:00.0`, 10 minutes, `tspmod` running):

- Every resume that no process asked for comes from `rm_acpi_nvpcf_notify` called by
  `acpi_ev_notify_dispatch`: the platform firmware notifies the driver through the ACPI device
  `NVDA0820` (NVPCF, Dynamic Boost) about every 22 s (sometimes two periods). The driver resumes
  the GPU, holds it about 7 s, and suspends it about 25 s after the resume.
- It continues with `nvidia-powerd` stopped, and with the charge held (not charging).
- The only other resumes were process exits that closed `/dev/nvidia*` (a browser's media
  decoder) and `nvidia-powerd` starting or stopping.
- Nothing in tspmo resumed the GPU during the 10 minutes. The driver exposes no autosuspend
  delay (`power/autosuspend_delay_ms` is unreadable), and the notification comes from the EC.

What helps: Eco (the GPU is off, no driver, no notifications). The default policy Optimized
already does that on battery. `tspmo doctor` names the cause (`nvidia-nvpcf`), or the apps
that hold the GPU (`nvidia-held`), or a desktop that loaded the driver (`compositor-nvidia`:
KWin started before tspmo's compositor setting existed holds `/dev/nvidia*`, so Eco cannot
unload the driver until the next login).
