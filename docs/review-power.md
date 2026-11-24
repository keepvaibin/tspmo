# Review: power and system features

Method: `tools/test/e2e.sh` (73+ checks, writes and restores on the real machine), `hold.py`
(profile holds), unit tests against the GU605MI fixture, and the measurements in docs/power.md.
Machine: GU605MI, AC all session, KDE Wayland. "Not exercised" means exactly that.

Fixed during this review:

| Found | Fix |
|---|---|
| tspmod woke 8 times a second at idle: upstream `blocking` pool threads never exit (condition needs more than 500 threads) | patched copy in `vendor/blocking`; 0.1/s at idle, 0.35/s in Eco+ |
| An OSD popped up on every automatic mode change (plug, unplug) and said "Power saver" for Eco+ | automatic changes are silent; Fn+F5 and `SetProfile` show the right name |
| `SetBrightness(0)` was accepted and clamped: the e2e test left the screen at 2 % | out-of-range is `InvalidArgs` |
| Fn+F5 with Fn-lock on did nothing (the key reaches the keyboard driver as a plain key) | the daemon cycles the mode when the kernel did not within 400 ms (not exercised: no key press) |
| `tspmo dump` read deprecated `asus-nb-wmi` attributes: kernel log warning | explicit list |
| Flicker-free dimming "pulse": the screen went darker, then settled (the backlight sat below the floor for 260 ms while the daemon waited for KDE and loaded the profile; a flash on every step from re-sending the ICC source) | docs/display.md "The pulse": floor pinned within 1 ms, KDE's own target as the level, source sent once |
| Eco+ ended with a stale "Waiting to switch to eco" message | cleared |
| Eco+ did not say the GPU is meant to be off | `GpuWanted`; `SetGpuMode` in Eco+ saves the policy |
| Eco+ old design starved the desktop | redesigned, docs/power.md |
| Messages with sentences ("The desktop was started before tspmo was set up, so ...") | one short line each |
| Doctor said nothing about why the dGPU is awake | `nvidia-nvpcf`, `nvidia-held`, `compositor-nvidia` |
| Eco+ saved state from the old design (5/7 W) would have been read back | `eco_version=2` |

## Features

| Feature | How verified | Result |
|---|---|---|
| Four modes, per power source | e2e: SetProfile x4, sysfs `platform_profile`, PL1, E-core clock, fans, 60 Hz; unit tests | pass |
| KDE applet sync (power-profiles API) | e2e: `ActiveProfile` after each mode; KDE `power-saver` keeps Eco+, `balanced` leaves it | pass |
| Cycle | e2e: four steps return to the start | pass |
| Holds | `hold.py`: performance hold wins, listed, released, dropped when the client leaves; power-saver hold keeps Eco+; performance wins over Eco+ | pass |
| Fn+F5 sync | wrote `platform_profile` from sysfs (what the firmware does): daemon and KDE follow within 1.5 s, OSD. Key press itself not exercised | pass (no key) |
| GPU modes | `tspmo gpu`, policy reads; **live switching not possible in this session** (KWin started before the compositor setting and holds the NVIDIA driver; tspmod answers "Log out and in once"). `tools/test/gpu-live.sh` is ready for the next login | blocked |
| Boot deferral of the GPU policy | unit tests, seen working at start ("boot settled; applying the GPU policy"); not re-booted (never reboot) | pass |
| dGPU wake in Hybrid | kernel runtime-PM trace, 10 min: firmware NVPCF notification every ~22 s; none from tspmo | cause found |
| dGPU not woken by tspmod | e2e traces `rpm_resume` over the idle window: 0 from tspmod | pass |
| Fans: custom curves, auto, validation | e2e on a mode that is not active; refuses a stopped fan at 90 C; Rpm 6 ms | pass |
| Fans: Eco+ | live: `pwm1_enable=1`, first points 0 %; 0 rpm on all three fans measured with 0 % curves; ramps with temperature (66 C core: running); unit tests | pass |
| Temps | `Fans1.Temps` (CPU package only; the dGPU's sensors wake it) | pass |
| Power limits | e2e: set on a mode that is not active, reset | pass |
| Battery limit, charge once | e2e: 80 and back to 100 read from sysfs, 150 refused, once on and off | pass |
| Toggles (overdrive, boot sound) | e2e: flip, read sysfs, restore; bad value and `gpu_mux_mode` refused | pass |
| Fn-lock | e2e `--fnlock`: on, hwdb has the media layout, off; key presses not exercised | pass (no key) |
| Hotkeys and OSD | unit tests (decode, drain, idle wake); no key presses possible | unit only |
| Lighting | e2e: brightness, idle timeout; Eco+ idle-off: unit test, live (backlight 0 after 10 s in Eco+) | pass |
| Slash | e2e: enable off/on; Eco+ dark: unit test on the packets (hardware not observed) | pass (unit) |
| Display refresh | e2e: policy 60 gives 60 Hz, restore; Eco+ gives 60 Hz | pass |
| Flicker-free dimming, brightness | e2e: own value is a no-op, 0 refused | pass |
| Colour modes, eye care | e2e: srgb and back, 4500 K and back, `neon` refused | pass |
| Doctor | CLI output; `Daemon1.Doctor` 25 ms; unit tests for the new checks | pass |
| Dump | run: 603 files, nothing from the dGPU, no kernel warning | pass |
| Bench | used for every table in docs/power.md; restores profile, limits, refresh, charge limit | pass |
| Idle cost | wake-ups from per-thread context switches over 60 s: 0.1/s balanced, 0.35/s Eco+ (the guard sample), measured over 60 s with nobody at the keyboard; key events and a running window add theirs; CPU 0-30 ms per 60 s; RSS 8 MB. A running `tspmo-gui` adds its own polling (the e2e reports SKIP then) | pass |
| D-Bus latency | `tspmo ping 200`: p95 0.7-0.8 ms; GetAll 3-8 ms | pass |
| Transitions | kernel log over the whole session: no Xid, no errors besides the NVIDIA driver's `PlatformRequestHandler` assertion when the thermal mode changes while the driver is loaded (firmware notification; harmless; none in Eco). KDE popups: none seen; the OSD appears only for key and API changes. A refresh change (Eco+ to or from 60 Hz) is one modeset: the panel blinks once | pass |

## Known limits

- Eco+ cannot turn the NVIDIA GPU off in the session that predates tspmo's compositor setting;
  it says so once ("Log out and in once") and retries after logout.
- The guard reacts after about 6-10 s of sustained load; shorter jobs are not moved.
- Battery numbers: the machine stayed on AC (see docs/power.md for the conversion).
