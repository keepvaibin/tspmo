# Power modes

Four modes, chosen per power source (AC, battery), in this order:

- **eco-plus** ("Eco+"): the power-saving mode.
- **silent** ("Silent"): the quiet mode. Fans stopped while cool and the firmware's quiet
  thermal mode; the CPU behaves as in Balanced. Its point is noise, not power.
- **balanced** ("Balanced")
- **performance** ("Turbo")

KDE, GNOME and `powerprofilesctl` see three names:

| tspmo | `ActiveProfile` reads | KDE sets it by |
|---|---|---|
| eco-plus | `power-saver` | `power-saver` (KDE's "Power Save" selects Eco+) |
| silent | `balanced` | nothing: KDE has no quiet profile |
| balanced | `balanced` | `balanced` |
| performance | `performance` | `performance` |

`balanced` set from KDE while Silent is active keeps Silent (KDE reads it as `balanced` already);
anything else leaves it. A `power-saver` hold is an Eco+ hold. `tspmo profile` (or
`org.tspmo.Daemon1.SetProfile`) uses all four; `power-saver`, `saver` and `eco` are aliases of
eco-plus there, `quiet` of silent. Default on both sources: balanced. A stored `power-saver`
(earlier versions' quiet mode) is read as `silent` and rewritten.

## What a mode changes

| | Eco+ | Silent | Balanced | Turbo |
|---|---|---|---|---|
| Firmware thermal mode | quiet | quiet | balanced | performance |
| CPU energy preference / bias | balance_performance / 8 | as Balanced | balance_power or balance_performance / 8 or 6 | performance / 0 |
| Turbo | on | on | on | on |
| Package power limit (RAPL) | PL1 12 W over 10 s, PL2 28 W | firmware | firmware | firmware |
| Efficiency-core maximum clock | 1.2 GHz | firmware | firmware | firmware |
| Background jobs | on the efficiency cores, weight 10 | | | |
| NVIDIA GPU | off (Eco) | policy | policy | policy |
| Fans | stopped below 58-66 C, steep ramp | same curve | firmware | firmware |
| Keyboard backlight | dark after 10 s idle | as set | as set | as set |
| Slash | dark | as set | as set | as set |
| Screen | 60 Hz | refresh policy | refresh policy | refresh policy |

Silent uses Eco+'s fan curve (`fans::eco_curve`, same thresholds) and nothing else of Eco+. Fan
curves and power limits set in the GUI are kept per firmware thermal mode (silent, balanced,
performance); Eco+ shares Silent's.

On battery every mode also gets PCIe link power saving, audio codec power-down and lazier disk
writeback. All writes skip values that are already set, because on ASUS each `platform_profile`
write makes the EC notify the NVIDIA driver and wakes the dGPU (the driver then logs
`NVRM: ... failed to get platform power mode from SBIOS` assertions: harmless, and absent once
the GPU is off).

Eco+ saves the original RAPL limits and windows and the efficiency cores' clock in
`/run/tspmo/rapl-orig` when it starts and writes them back when it ends, when `tspmod` stops
(`ExecStopPost`) and after a daemon crash (the file is read again at start-up).

## Fn+F5 and `platform_profile`

Fn+F5 has exactly one owner: tspmod (`crates/tspmod/src/fankey.rs`). The kernel (asus-wmi) also
steps `platform_profile` itself on that key (balanced, performance, quiet), and that step is not a
choice of the user's.

- On the key (the keyboard's key, or its Fn-lock variant on the WMI device) tspmod takes the
  next mode after the latest one it chose, eco-plus, silent, balanced, performance, eco-plus,
  applies it once (Eco+ with the GPU first and the thermal write held back, as always) and
  shows one OSD with it. Two reports of the same press less than 50 ms apart count once;
  quicker presses are separate and each advances one step.
- The watcher on `platform_profile` ignores a change that falls 500 ms before to 500 ms after a
  press (the notification can come first: it holds a change back 200 ms to see whether a press
  explains it). The handler waits up to 250 ms for the kernel's step so that the write it then
  makes is usually already in place; 550 ms after the last press it rewrites the thermal mode
  if the kernel's step left something else. The final hardware state is what tspmod chose.
- Every `platform_profile` change still counts for the 30 s rule below (no driver unload within
  30 s of a change).
- Journal lines (`journalctl -u tspmod`): `fan key from <device> at <ms>`,
  `platform_profile -> <value> at <ms>` (same clock) and what was done with it, for checking the
  timing on a given machine.

A genuine external change (another tool writing `platform_profile`) is followed to the nearest
mode: balanced and performance as they are, `quiet` as Silent, with one OSD. Eco+ is never
entered from outside: its GPU, power caps, guard and 60 Hz are applied only when the user chooses
Eco+ (tspmo, KDE's Power Save, Fn+F5). Quiet while Eco+ is active is Eco+'s own thermal mode and
changes nothing.

OSD: one note for each change the user makes (Fn+F5, `SetProfile`/`CycleProfile` from the GUI or
CLI). Changes KDE makes show KDE's own note; plugging and unplugging the charger applies the
per-source mode silently.

## Eco+ design

Eco+ is the power-saving mode, selectable on AC and on battery. It must also stay snappy:
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

### Light load, nothing running (`--no-load --no-wake`, 6 rounds, 25 s each)

| Condition | psys W | package W |
|---|---|---|
| balanced, 60 Hz | 17.3 | 4.9 |
| **Eco+** | **12.1** | **3.9** |

Eco+ saves 5.2 W at the platform (30 %) and 1.0 W at the package against balanced at 60 Hz, in a
session where the NVIDIA GPU stays in Hybrid (it sleeps most of the time but wakes every ~22 s
for ~25 s; Eco removes that too, once the session can switch). The screen was at 100 %
brightness on an OLED, which is most of the 12 W: psys includes the panel.

**What 5 W on the battery means in psys.** The earlier runs (below) put the battery about
1.9 W (1.34x) above psys for the same light load: 7.37 W on the battery at 60 Hz against 5.5 W
psys on AC. A 5 W battery draw is therefore about 3.1-3.7 W psys, and 7 W bursts about 5-5.5 W.
The panel is the floor: at 100 % brightness this OLED alone keeps psys near 12 W, so the figure
is reachable only with the screen dimmed. Not measured on the battery (the machine stayed on AC).

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

`/var/lib/tspmo/state` (`profile_ac` and `profile_battery` hold a mode name; edit while `tspmod` is stopped; Eco+ values are read only with
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

Incidents on the GU605MI caused by tspmo touching the NVIDIA GPU at the wrong time (1, 2, 4), and one finding about it (3).
All are fixed or worked around; the rules stay.

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

**4. Unloading the NVIDIA driver deadlocks against the firmware's notification.** GU605MI,
NVIDIA open 620.73: during an Eco+ entry `rmmod nvidia` hung in D state for good (unkillable;
only a reboot clears it):

```
rmmod:   __flush_workqueue <- acpi_remove_notify_handler <- nv_acpi_methods_uninit
         <- RmShutdownAdapter <- nv_shutdown_adapter <- nv_stop_device <- nv_pci_remove_helper
         <- pci_unregister_driver <- cleanup_module
kworker: rmapiLockAcquire <- rm_acpi_nvpcf_notify   (blocked on an rw-semaphore owned by rmmod)
```

An ABBA: `rmmod` holds the RM API lock and flushes the ACPI notify workqueue, while a pending
NVPCF notification (hazard 3: every ~22 s in Hybrid, and right after every `platform_profile`
write) waits in that queue for the RM API lock. Eco+ wrote the thermal mode and then switched to
Eco, so the unload met the notification. Any unbind or PCI removal of the NVIDIA device takes the
same path, so skipping `rmmod` and just writing `dgpu_disable` does not avoid it. Rules now
(`crates/tspmod/src/quiet.rs`, `gpu.rs`, `state.rs`):

- **Order.** Entering Eco+ from Hybrid with the dGPU on switches the GPU first: `Daemon::apply`
  holds back the `platform_profile` write (`eco_switch_ahead`) and the GPU worker writes it when
  the switch is over, whether it worked or not (`release_thermal`). Every `platform_profile`
  change (ours, or Fn+F5) is timestamped, and no unload starts within 30 s of one: the switch
  thread waits it out.
- **Quiet window.** Before the first `rmmod` and before `rmmod nvidia`, the dGPU's
  `power/runtime_status` must be `suspended` (a notification resumes the GPU, so a suspended GPU
  has none pending). If it is awake, only that attribute is read every 150 ms until it
  suspends, then the unload runs at once. After 45 s awake the switch is undone and
  GpuMessage is "NVIDIA GPU busy; try again"; the retry thread tries policy-driven switches again.
- **Never block.** `rmmod` runs as a child under an 8 s watchdog. If it does not return, the
  daemon gives up on it: it writes `/run/tspmo/gpu-restart` (gone after a reboot), clears
  `GpuSwitching`, sets GpuMessage "Restart to recover the NVIDIA GPU", refuses every further GPU
  switch (including policy retries), and `tspmo doctor` reports `nvidia-unload-hung` with the
  fix `systemctl reboot`. The kernel task stays; a thread reaps it if it ever returns. Note that
  `systemctl restart tspmod` can then wait out the stop timeout, as systemd cannot kill the task.
- **Cancellation.** A policy-driven switch (Eco+ entry, power source, policy) is withdrawn the
  moment the policy no longer wants its target: leaving Eco+, plugging in, choosing another
  policy. The switch thread asks at every wait step (the 30 s window, the suspend wait) and
  immediately before the first `rmmod`; after that it finishes. A cancelled switch ends quietly
  (no message, `GpuSwitching` cleared, `PropertiesChanged` emitted). The held-back Eco+ write is a
  flag, not a stored write: `release_thermal` writes the thermal mode of the mode in force
  at that moment, and only if that is still Eco+; leaving Eco+ writes its own mode at once. Eco+'s
  other settings (limits, E-core cap, guard, fans, lighting, 60 Hz) are applied on entry and
  restored on leaving whatever the switch is doing.
- `GpuSwitching` is cleared at daemon start and if the switch thread ends abnormally.
- Tests: fake sysroot and clock for the quiet window, the 30 s rule and the bound
  (`quiet.rs`), the ordering and held-back write (`state.rs`), the watchdog with a real child,
  the doctor finding. `tools/test/gpu-live.sh` enters Eco+ from Hybrid right after a thermal-mode
  change, twice, and checks for a stuck `rmmod`, the restart marker and hung-task reports.

### Apps that hold the NVIDIA GPU (Eco and Eco+)

Eco and Eco+ switch the dGPU off: powered down, off the PCI bus, 0 W. Nothing turns it back on
until the mode is left: the only code that powers it on is a switch to Hybrid (`gpu::power_on`),
which is only started by the policy (`wanted_mode`: Eco+ is always Eco) or by `SetGpuMode`
(which in Eco+ saves the policy only). The retry thread, a power-source change, a resume (which
now re-checks the policy and switches off a GPU the firmware brought back) and hotplug all go
through `wanted_mode`. A switch needs the GPU free of apps: any process with `/dev/nvidia*` or
the dGPU's render node open blocks it.

**Why an everyday app holds it.** GLVND's EGL enumerates every vendor and device. With NVIDIA's
EGL installed, an app that only looks for a GPU (Firefox's RDD and GPU helpers, Chromium and
Electron GPU processes, Qt) loads `libEGL_nvidia` and opens `/dev/nvidia0` and `/dev/nvidiactl`,
even with `LIBVA_DRIVER_NAME` and `MOZ_DRM_DEVICE` on the iGPU. Measured on the GU605MI:
Firefox's RDD process held `/dev/nvidia0` (x3), `/dev/nvidiactl` (x2) and `renderD129`.

**Session environment.** `tspmo session-env` (the systemd user-environment generator) therefore
also emits `__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json` outside
Ultimate mode, as KWin already gets in `compositor.env`. It applies to apps started after the
next login. Checked (`eglinfo -B`, `glxinfo -B`, `vulkaninfo --summary` with the generated
environment):

| Launch | EGL | GLX | Vulkan |
|---|---|---|---|
| plain | Mesa (Intel) | Mesa (Intel) | both GPUs listed, Intel first |
| `prime-run`, KDE "Run using dedicated graphics card", Steam/Proton | Mesa (Intel) | NVIDIA | NVIDIA |
| `tspmo-run` | NVIDIA | NVIDIA | NVIDIA |

Vulkan and GLX do not use GLVND's EGL, so DXVK/VKD3D (Proton) and native GL games under X11 keep
the 4070 through PRIME offload. Only a program that renders through **EGL** on the dGPU (a
Wayland-native GL app or game, SDL3 on Wayland) needs the NVIDIA EGL back. switcheroo-control
(KDE's launcher entry) builds its dGPU environment in code (`__GLX_VENDOR_LIBRARY_NAME`,
`__NV_PRIME_RENDER_OFFLOAD`, `__VK_LAYER_NV_optimus`, `VK_LOADER_DRIVERS_SELECT`): there is no
udev or hwdb property to add `__EGL_VENDOR_LIBRARY_FILENAMES`, and nvidia-prime's `prime-run`
belongs to its package. So: `tspmo-run COMMAND` (installed in `/usr/bin`) is `prime-run` plus
NVIDIA's EGL; in Eco it runs the command on the iGPU with a note. Use it as a Steam launch
option (`tspmo-run %command%`) or in a copy of a launcher whose `Exec=` it prefixes for EGL
apps. Other side effects: nothing in the tested set (Firefox, Chromium/Electron, Qt, GTK) loses
hardware rendering or decoding: they run on Mesa/Intel as before, minus the NVIDIA device that
the enumeration used to open. CUDA/NVENC do not use EGL. nvidia-vaapi-driver would need
`tspmo-run`, but `LIBVA_DRIVER_NAME=iHD` is set anyway.

**When a user-initiated switch to Eco or Eco+ finds apps on the GPU** (`gpu::begin`):

- Only ordinary users' apps are ever closed (`User::closable`: uid 1000 or above, never the
  compositor or session processes `kwin_wayland`, `Xwayland`, `plasmashell`, `systemd`,
  `systemd-logind`, which let go through the announce path, and never NVIDIA's helpers).
- Browser helpers that restart by themselves (Firefox `RDD Process`, `GPU Process`,
  `Utility Process`; Chromium and Electron `--type=gpu-process`) are closed silently: SIGTERM,
  SIGKILL after 2 s, then the switch goes ahead. The browser starts them again, now on the iGPU
  when it was started with the session environment. This only happens when helpers are the only
  thing in the way.
- Anything else: `GpuMessage` is "Firefox is using the NVIDIA GPU" (one app, "A and B are
  using...", at most three named) and one OSD names it. Each app is announced once per mode
  change (Eco+ entry, an explicit `SetGpuMode`), not on every retry; the retry thread (every
  15 s, Eco+ and automatic switches only) keeps trying and a later success shows no OSD. Unplugging
  with Optimized, outside Eco+, sets the message but shows no OSD.
- `CloseGpuApps()` (polkit `org.tspmo.set-gpu`) closes every closable app in the way (SIGTERM,
  3 s, SIGKILL) and completes the waiting switch: Eco+, an automatic switch, or the explicit
  `SetGpuMode("eco")` that was refused.
- Setting `CloseGpuAppsAuto` (`SetCloseGpuAppsAuto(b)`, saved as `close_gpu_apps=` in
  `/var/lib/tspmo/state`): the daemon does that itself for every switch to Eco, as G-Helper's
  "Stop GPU apps" does. Off by default.
- `tspmo doctor`: `nvidia-egl-probe` names apps that hold the GPU only because NVIDIA's EGL was
  loaded without the session environment (no PRIME offload variable in their environment).
  Fix: restart them; log out and in once if the whole session started before tspmo.
- Tests: classification from a fake `/proc`, the kill sequence with a fake process table
  (exits on TERM, ignores TERM, cannot be killed), the closable and plan rules.

### On the battery (first runs, light load, nothing running)

`tspmo bench --no-load --no-wake`, 30-95 % brightness (the user's), other builds running: the
absolute watts are not usable, but the pattern is. Every condition that starts with a
thermal-mode change (power-saver, Eco+, and the balanced that follows them) measured 10-14 W
higher at the platform than the one that did not (`hz=60` after balanced: 11.5 W against 20-23 W).
The cause is the dGPU: each `platform_profile` write makes the EC notify the NVIDIA driver, which
wakes the GPU for about 25 s, longer than the 8-12 s settle. Compare conditions only with a
settle of 35 s or more, or with the GPU off (Eco): with the GPU off there is no driver to wake.
