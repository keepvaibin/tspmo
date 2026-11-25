# tspmo — build plan (ASUS first)

The source of truth for everyone working on tspmo. Read it fully before starting a work package.

## Product rules

- **Target: ASUS laptops.** Lenovo comes later; keep the vendor split clean (`asus` modules)
  so Lenovo can slot in.
- **Capabilities are discovered, never assumed.** Read what the kernel exposes: `platform_profile`,
  `/sys/class/firmware-attributes/asus-armoury/attributes/*` (self-describing:
  type/min/max/possible_values), hwmon by *name* (`asus`, `asus_custom_fan_curve`), LED class,
  hidraw by USB id. A feature the machine lacks is hidden, not shown disabled.
- **Zero idle cost.**
  - The daemon (`tspmod`, root) is event-driven: no timers or polling loops while idle, and
    0 wakeups/s. Threads block in the kernel (poll/read/D-Bus).
  - hidraw is opened per write, never held: holding it keeps USB devices awake (asusctl PR #404).
  - Never touch `/dev/nvidia*` or NVML: that wakes the dGPU.
- **Fast:** D-Bus queries must answer in under 10 ms even while firmware is busy. Never do a
  slow firmware call while holding the daemon lock: cache, or work on a thread.
- **Safe:** validate every write against the kernel's bounds, restore firmware defaults on
  request, and never leave hardware in a half-applied state.
- **Code style:**
  - Idiomatic Rust: no `unsafe`, no async runtime beyond what zbus brings, minimal deps.
    Prefer `std` and the existing `rustix`/`zbus`.
  - Ideas from other projects (asusctl, G-Helper, g-helper-linux) are studied, then rewritten
    in Rust. Never copy code verbatim. Credit sources in `docs/credits.md`.
- **UI wording:**
  - Use the fewest words possible: labels, not sentences. No reassurance ("don't worry",
    "no data is lost"), no filler, no exclamation marks.
  - Errors say what happened and what to do, in one short line.
- **Tests:**
  - Every backend function is unit-tested against a fake sysfs tree (`SysRoot`, `TSPMO_SYSROOT`,
    see `crates/tspmo-sys/src/lib.rs` `testutil::FakeRoot`) and the GU605MI fixture
    (`fixtures/asus-gu605mi`).
  - `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo
    test --workspace` must pass before every commit.

## Architecture (already built — reuse it)

| Path | What |
|---|---|
| `crates/tspmo-sys` | `SysRoot` (relocatable sysfs), `fwattr` (firmware-attributes with validation), `hwmon`, `backlight`, `power_supply`, `pci`, `usb`, `dmi` |
| `crates/tspmo-core` | `power` (profile tuning plan/apply), `gpu` (modes/status/users), `usbpm`, `doctor`, `machine` |
| `crates/tspmod` | the daemon. `state.rs` (`Daemon`, one mutex), `dbus.rs` (PPD API and `org.tspmo.Daemon1`), `gpu.rs` (live switching), `watch.rs` (event threads), `polkit.rs`, `events.rs` (in-process event bus), `session.rs` (desktop-session helpers, stub) |
| `crates/tspmo` | CLI (`info`, `status`, `profile`, `gpu`, `doctor`, `dump`, `session-env`, `sleep-hook`) |
| `data/` | systemd units/drop-ins, D-Bus policy/activation, polkit, user-environment generator, sleep hook |
| `packaging/arch/PKGBUILD` | split package `tspmo-git` + `tspmo-hid-asus-dkms` |

To install and test on the dev machine (passwordless sudo is enabled):

```
cd packaging/arch && makepkg -f && sudo pacman -U --noconfirm tspmo-git-0.1.0-1-x86_64.pkg.tar.zst && sudo systemctl daemon-reload && sudo systemctl restart tspmod
```

Never `git checkout` the PKGBUILD (its pkgver is static now).

### Event bus (`crates/tspmod/src/events.rs`)

`events::subscribe(|e| ...)` / `events::publish(&e)`. Events:
- `ProfileApplied { profile, eco_plus, on_battery }`
- `PowerSource`
- `Resumed`
- `Key(code)`

Use it instead of calling into another package's code.

### On-screen display (`crates/tspmod/src/session.rs`)

`session::osd(icon, text, value)`: any package calls it when a hotkey changes something.

### D-Bus

- Every interface lives on the system bus at object `/org/tspmo` (service `org.tspmo.Daemon1`).
- Each work package owns its interfaces, in its own files, registered from `main.rs` with one
  `serve_at` line each.
- Every interface:
  - exposes its full state as properties with `PropertiesChanged`, so a GUI can `GetAll` once
    and then follow signals
  - performs changes through methods checked with polkit (`polkit::check`; add actions to
    `data/polkit/org.tspmo.policy`)
- Document every interface in `docs/dbus.md`.

## Work package A — ASUS hardware

Files: `crates/tspmo-core/src/asus/*`, `crates/tspmod/src/asus/*`, the `register` lines in `main.rs`, CLI subcommands in `crates/tspmo/src/asus_cli.rs`, `docs/dbus.md` (your sections), tests and fixtures.

1. **Fans (`org.tspmo.Fans1`).**
   - Every fan in hwmon `asus_custom_fan_curve` (GU605 has `pwm1..3` = CPU, GPU, mid), with labels from hwmon `asus` `fanN_label`, and live RPM from `fanN_input`.
   - Per *mode* (silent / balanced / performance), per fan: an 8-point temperature→PWM curve, or *Auto* (firmware curve). Also *Full speed* and *Reset* (firmware defaults: `pwmN_enable=3` restores, then read back the factory points).
   - The EC resets custom curves when the thermal mode changes, so re-apply after every `ProfileApplied` and `Resumed`.
   - Validate: temperatures ascending, PWM non-decreasing, within the kernel's limits.
   - Persist to `/var/lib/tspmo/fans`.
2. **Power limits (`org.tspmo.Power1`).**
   - asus-armoury `ppt_pl1_spl`, `ppt_pl2_sppt`, `ppt_pl3_fppt`, `ppt_apu_sppt`, `ppt_platform_sppt`, `nv_dynamic_boost`, `nv_temp_target`, `nv_tgp` (whichever exist; bounds come from the attribute, and they differ on AC and battery).
   - Per mode: *Default* (firmware) or custom values, re-applied on `ProfileApplied`/`Resumed`.
   - G-Helper's `IsFanRequired`/`IsPowerRequired` coupling applies to GU605: when custom power limits are on, apply fan curves too (the firmware curve if the user has none).
3. **Battery (`org.tspmo.Battery1`).**
   - Charge limit via `charge_control_end_threshold`. The GU605 firmware only honours 60/65/70/75/80/100: put a quirk table in `crates/tspmo-core/src/asus/quirks.rs`, keyed by DMI board name prefix.
   - *Charge to 100% once*: back to the limit after unplug, or once full. Event-driven via UPower or power_supply uevents, no polling.
   - Health (full/design), cycles, live W, time left.
   - Default 100 (do not change the user's limit unless asked).
4. **Hardware toggles (`org.tspmo.Hardware1`):** `panel_overdrive`, `boot_sound`, `mini_led_mode`, `charge_mode` (find out what it means), plus anything else in asus-armoury, generically driven by the attribute's self-description.
5. **Keyboard lighting (`org.tspmo.Lighting1`).**
   - Brightness levels (`asus::kbd_backlight`).
   - Effects/colours/speed for RGB models (Aura). Port the protocol from asusctl `rog-aura` into zero-alloc packet builders. This model (GU605MI, board 0b05:19b6) is reportedly white-only: detect capability (asusctl's per-model data, and the HID report descriptor) and expose only what works.
   - Power-state lighting (boot / awake / sleep / shutdown), lighting profiles per power source, and an idle timeout.
   - The hidraw protocol (Aura init and power-state packets) must be written per write, never held.
6. **Lid displays (`org.tspmo.Lid1`).**
   - **Slash** (0b05:193b on this machine; port `rog-slash`): on/off, brightness, mode/animation, interval, plus show on boot / sleep / battery / lid-closed. Default off on battery.
   - **AniMe Matrix** (port `rog-anime`): modes, brightness, image/GIF/text. Fixture-tested only, since there's no hardware here.
7. **Hotkeys.**
   - A thread blocking on the ASUS keyboard / "Asus WMI hotkeys" evdev devices (read only; never grab them).
   - Map ROG/Armoury key, Aura (Fn+F4), fan (Fn+F5) and M-keys from the hid-asus/asus-wmi keymaps, read from the kernel source for this kernel.
   - Publish `events::Key`. Aura cycles the lighting mode, with an OSD.
   - The fan key belongs to tspmod (`fankey.rs`): it cycles eco-plus, silent, balanced, performance once and shows one OSD. The kernel's own step of `platform_profile` on that key is not followed (see `docs/power.md`, "Fn+F5 and `platform_profile`").
   - The ROG key itself is bound by the GUI package (compositor shortcut); just publish it.
8. **Fn-lock:** G-Helper-style software Fn-lock (media keys by default) via a udev hwdb remap the daemon writes and reloads (`systemd-hwdb --usr update` is wrong for a generated file: use `/etc/udev/hwdb.d` + `systemd-hwdb update` + `udevadm trigger` for the keyboard only).

Hardware safety:
- Before testing a fan curve, save the current state, and restore it after.
- Never set PWM below the firmware's own minimum at high temperatures.
- Do not switch the GPU MUX (it needs a reboot).

## Work package B — power, display and session

Files: `crates/tspmo-core/src/power.rs`, `crates/tspmod/src/{state.rs,dbus.rs,session.rs}`, new `crates/tspmod/src/display/*`, `crates/tspmo-core/src/display/*`, the CLI (`crates/tspmo/src/main.rs`), `data/`, `docs/dbus.md` (your sections).

1. **Eco+, the fourth mode.**
   - Modes: `eco-plus`, `silent`, `balanced`, `performance`, stored per power source.
   - The PPD API (`ActiveProfile`) maps `power-saver` to eco-plus both ways; silent reads `balanced`. KDE `balanced` keeps silent; any other KDE choice leaves it.
   - `org.tspmo.Daemon1.Profile` / `SetProfile` / `CycleProfile` use all four.
   - Publish `ProfileApplied { eco_plus: true }`.
   - Eco+ = the firmware quiet mode plus:
     - an Intel RAPL **platform (psys) limit** (`/sys/class/powercap/intel-rapl:1`, name `psys`: `constraint_0` long-term, `constraint_1` short-term) and package limits on both `intel-rapl` and `intel-rapl-mmio`
     - **60 Hz**
   - Save the original limits at start-up and restore them when leaving Eco+.
   - User target: about 5 W sustained with a 7 W burst. Measured so far: a 5 W psys cap made a light workload 4.7× slower (on AC, where psys reads high).
   - Tune it **on battery** with the interleaved harness. Choose limits that reach the lowest battery draw while light work (`load.py`-style 10 Hz bursts) stays at most 1.5× slower and the desktop stays smooth. If 5/7 W can't meet that, pick the closest that does and document the numbers in `docs/power.md`.
   - Also evaluate:
     - E-core lassoing (systemd `AllowedCPUs`), only if it wins measurably. So far it didn't.
     - uncore max frequency (`/sys/devices/system/cpu/intel_uncore_frequency/package_00_die_00/max_freq_khz`).
     - turbo off.
   - Make the core topology detection generic (Intel P/E/LP-E via `cpu_core`/`cpu_atom` and L3 presence; AMD via `cpu_capacity`/max freq), so AMD can be added.
2. **`tspmo bench`.** Port the interleaved A/B harness to Rust (CLI):
   - conditions × rounds, interleaved
   - battery draw from `current_now×voltage_now` and RAPL psys/package energy counters
   - a built-in light load with chunk latency, and medians
   - it must restore everything it changes, even on Ctrl-C
3. **Session helper (`session.rs`).**
   - Find the active graphical session (logind: active session on seat0, its user, `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`) and run short commands as that user (no resident helper).
   - Implement `osd()`:
     - on KDE, call plasmashell's `org.kde.osdService` (`showText(icon, text)` / `showProgress`-style API; check what this Plasma 6.8 exposes)
     - otherwise, a `tspmo-gui --osd` one-shot (stub the flag; the GUI package implements it)
4. **Display (`org.tspmo.Display1`)**, for the internal panel and any external ones:
   - **Refresh rate:** list modes, set max / 60 / *Auto* (60 on battery, max on AC; Eco+ always 60). Via KWin (`kscreen-doctor` or, better, the kde-output-management Wayland protocol); keep the current resolution and scale.
   - **Panel overdrive and Mini-LED:** coordinate with package A, which owns the firmware toggles; Display1 just reflects them.
   - **Flicker-free dimming, the default on OLED:**
     - Detect OLED from EDID (panel type, or the ASUS OLED panel ids; the GU605MI panel is Samsung ATNA60DL01).
     - Research how G-Helper does it (seerge/g-helper `app/Display/VisualControl.cs` and related files) and the panel's PWM threshold.
     - Keep the backlight at or above the threshold and dim the rest in software through KWin's own brightness/dimming (`kscreen-doctor` shows "Brightness control: supported, set to X% and dimming to Y%"; find the API).
     - The brightness keys and KDE's slider must then behave this way automatically, with smooth steps and the normal KDE OSD.
     - On LCD panels, normal backlight dimming.
     - Measure that the backlight stays at or above the threshold while the image darkens, and that Night Light still works.
   - **Colour modes**, like G-Helper's visual modes on OLED:
     - Native / sRGB / DCI-P3 gamut (KWin colour profile per output; generate ICC from EDID primaries if needed, with a small hand-written ICC writer).
     - *Eye care* (warm white point via KWin Night Light, manual temperature).
     - Vivid / Cinema if they map to something real.
     - Each shows an OSD.
   - **OLED care:** pixel-shift is not available on Wayland. Expose a short dim/screen-off timeout for static content (KDE powerdevil settings) and the panel's own features if any. Don't invent features.
5. **`doctor` updates** for the new areas, and keep `docs/power.md` with the measured numbers.

## Work package C — GUI, widget, hotkey (after A and B are merged)

- **Slint popup (`crates/tspmo-gui`)**, using the femtovg or software renderer. It must never use the NVIDIA GPU.
  - **Toggle:** the ROG key runs `tspmo-gui --toggle`. If an instance is open (abstract socket `@tspmo-gui-$UID`), it closes; otherwise it opens. Nothing stays resident once closed.
  - **Targets:** key to first frame under 120 ms, under 40 MB RSS, and the dGPU stays asleep.
  - **Sections:** Mode (4) · GPU · Screen · Battery · Fans · Power limits · Lighting · Lid · System.
  - **Style:** animated, responsive, keyboard-first, minimal text, light and dark themes.
- **OSD** (`tspmo-gui --osd icon text value`) for non-KDE desktops.
- **The ROG key and other bindings,** installed per desktop (KDE kglobalaccel `.desktop` with `X-KDE-Shortcuts`).
- **A Plasma widget** (`data/plasma/`) replacing the power-profile section with all four modes, using Plasma's D-Bus QML bindings, with no polling.

## Work package D — audit and test (after C)

Independent review against this plan:
- correctness
- idle wakeups (`powertop`, `perf trace`)
- RSS
- latency
- wording
- missing features vs G-Helper's README list
- packaging
