# Work package B handoff (power, display, session)

State at the end of the session. Branch: the worktree `agent-aab757692f8a093b4`; master (work
package A) is merged in.

## Done and verified on the GU605MI

- **Eco+** as the fourth mode (`tspmo profile eco-plus`, `Daemon1.SetProfile/CycleProfile`).
  KDE sees `power-saver`; KDE choosing `power-saver` keeps Eco+, anything else leaves it.
  `ProfileApplied { profile: power-saver, eco_plus: true }`. Package RAPL caps (both
  `intel-rapl` and `-mmio`), no turbo, uncore 1.6 GHz, 60 Hz; originals saved in
  `/run/tspmo/rapl-orig`, restored on leaving, on `tspmod` stop (`ExecStopPost`) and after a
  crash. Defaults are unchanged: power-saver on battery, balanced on AC. Tests in
  `crates/tspmod/src/state.rs`, `crates/tspmo-core/src/power.rs`.
- **Session helper and OSD** (`crates/tspmod/src/session.rs`): logind session, commands run as the
  user, `session::osd(icon, text, value)` through Plasma's `org.kde.osdService.showText`.
- **Refresh rate** (`max | 60 | auto`; Eco+ is always 60), **flicker-free dimming** (vcgt ramp in a
  generated ICC profile, follows KDE's own brightness keys and slider, level persisted),
  **colour modes** (`native | srgb | p3`), **eye care** (Night Light, constant). `docs/display.md`.
- **`tspmo bench`** (interleaved conditions, restore guard process, `--hold-charge`),
  **`tspmo ping`**, **doctor** checks (RAPL, kscreen-doctor, flicker-free off).
- **Incident fixes** (see "Known hazards" in `docs/power.md`):
  1. Nothing reads NVIDIA backlight/hwmon/DRM connectors/`/proc/driver/nvidia`
     (`tspmo_sys::pci::touches_dgpu`, `backlight::list` `sampled`). Poisoned-file tests.
  2. The GPU policy waits for the boot to settle (`crates/tspmod/src/boot.rs`: systemd
     `JobRemoved` signals; `drm-module-load.target`, else display manager, else graphical; 5 min
     bound; never while `nvidia_drm` is expected and absent). Deployed and seen working:
     "boot settled; applying the GPU policy".
- CLI exits quietly on a closed pipe.

## Written but NOT exercised on hardware (do these first)

- **Eco+ = dGPU off** (`gpu::wanted_mode`, the `eco-plus-gpu` thread in `main.rs`): unit-tested
  only. The merge of master and this change are committed but **not installed**; the installed
  daemon is the boot-deferral build. Before enabling on the machine: install, then do a live
  Eco+ on / off cycle with `power/runtime_suspended_time` of the dGPU increasing and
  `runtime_active_time` not (read only `power/*` of the dGPU, never nvidia-smi). Apps holding the
  dGPU produce an OSD naming them and the 15 s retry.

## Not done

1. **Fans off in Eco+.** Use work package A's core API (`crates/tspmo-core/src/asus/fans.rs`,
   `crates/tspmod/src/asus/thermal.rs`: `set_fan(mode, fan, Setting::Custom(curve))`,
   `reset_fan`). Plan: on `ProfileApplied { eco_plus: true }` set a custom curve per fan,
   0% PWM up to a safe temperature taken from the firmware quiet curve (`Fans1.Factory`) and
   well under the CPU's thermal limits, then a steep ramp; put the user's normal curves back on
   leaving. Document the thresholds in `docs/power.md`.
2. **Eco+ tuning against the new spec** (5 W typical, 7 W bursts, snappy): PL1 low with a longer
   `constraint_0_time_window_us`, PL2 high enough for interactive bursts. Measure p95 burst
   latency (target at most 1.3x power-saver), wake-up latency (a Rust cyclictest-style timer
   jitter tool) and KWin frame pacing at 60 Hz. Current default is pkg 5/7 W with the firmware's
   time windows; light-load p95 showed a tail (95 ms) under Eco+ in two bench runs that were
   contended by other builds: **re-measure on a quiet machine before trusting 5/7 W**.
   Eco+ must not be tried while builds run (it made the desktop unusable).
3. Keyboard backlight and lid-display idle-off in Eco+ (A's lighting/slash APIs).
4. Battery measurements: all numbers so far are psys/package on AC (battery was not available
   to unplug). The machine is on battery now, so `sudo tspmo bench` reports a real battery column.

## Measurements so far

`docs/power.md` has the table (AC, charging held at 60% with `charge_control_end_threshold`; the
firmware only honours 60/65/70/75/80/100). Key results: 60 Hz is the main lever; package caps
cost nothing on light work; a psys cap at 12 W or below slows light work 4.7x (400 MHz); E-core
lassoing is worse. Runs with the threshold at 40 (not honoured: charging continued) were
discarded.

## Resume

```
cd <worktree>
CARGO_BUILD_JOBS=4 nice -n 19 cargo fmt --all && CARGO_BUILD_JOBS=4 nice -n 19 cargo clippy --workspace --all-targets -- -D warnings && CARGO_BUILD_JOBS=4 nice -n 19 cargo test --workspace
cd packaging/arch && CARGO_BUILD_JOBS=4 nice -n 19 makepkg -f
sudo pacman -U --noconfirm ./tspmo-git-0.1.0-1-x86_64.pkg.tar.zst && sudo systemctl daemon-reload && sudo systemctl restart tspmod
```

Before any test note `tspmo status` (mode per source) and put it back afterwards, also on
failure. `sudo tspmo bench ...` restores what it changes through a guard process (if it was
killed with `kill`, the file `/run/tspmo-bench-<pid>` lists what is still to restore).

## Known issues

- The first colour or dimming change after login loads the profile through `kscreen-doctor`
  (about 100 ms); KScreen remembers the last profile path, so after a reboot the screen can be
  dimmed until the daemon re-applies it at login.
- The 30% flicker floor is an estimate (no vendor data).
- `SessionNew` handling (apply at login) was not tested with a real logout.
- Only KDE is supported for refresh, dimming and colour; the OSD falls back to `tspmo-gui --osd`.
- `tools/bench/*.sh` are the old prototypes, superseded by `tspmo bench`.
