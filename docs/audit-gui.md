# GUI audit

Independent audit of `tspmo-gui` and the Plasma widget, run against the **real** `tspmod` on the
system bus (ASUS GU605MI, Plasma 6 Wayland, 2560x1600 at 1.5x, dark theme, light checked with
`TSPMO_THEME=light`). Input was injected through uinput (clicks, Tab, Esc, the real `KEY_PROG1`),
state read back with `tspmo status|display|fans|limits|battery|hw|light|lid` and `busctl`.
Screenshots were cropped to the window; the rest of the desktop was never kept.

Severity: H = wrong or unusable, M = confusing or against the brief, L = polish.

## Found and fixed

| # | Sev | Issue | Fix | Verified |
|---|---|---|---|---|
| 1 | M | Tiny shortcut digits (1-4) and F/S/K/Y letters crowded tile corners | Hints removed from tiles and buttons; keys still work | screenshots; keys 1-4 sent `SetProfile` (eco-plus, power-saver, performance, balanced) |
| 2 | H | `Not charging 0.0 W` / `Discharging 0.0 W`: the daemon reports 0 W while charging at times, shown as fact | Rate only while Charging or Discharging and at least 0.5 W; one line `82%  Charging 41.9 W`; `%` without a space everywhere | live: `Not charging`, `Charging 42.3 W`, `Charging` with no rate when daemon says 0.0; unit test `battery_line` |
| 3 | H | System window 1355 px wide: the daemon's long GPU sentence set the window width | Only the text before the first colon is shown (`Waiting to switch to eco`) | window 691 px wide again |
| 4 | M | `Charger 2` raw code | `USB-C`, `barrel`, `none`; read-only rows use the checkbox row height | live (charge_mode=2 shows USB-C) |
| 5 | H | Keyboard effect stepper empty and a speed (Normal) preselected when the daemon has no effect yet; Next skipped the first effect | Shows `Firmware`, no speed/colour selected; stepping starts at the first (or last) effect | live (daemon Effect is empty here); unit test `cycles_with_wrap` |
| 6 | M | Colour swatches had no keyboard access | Swatches row: Tab, arrows, Enter/Space, ring | headless + code |
| 7 | M | Boost could be shown/set below Sustained (and fast boost below boost) | Setting one limit moves the neighbours: sustained <= boost <= fast boost | live: drag boost to 35 W, sustained follows to 35 W, `Firmware defaults` restores 90/135; unit test |
| 8 | M | Sentences: `firmware curve`, `needs fan curve`, `custom curve`, `Warm white point`; `Colour`; slash `255`; `no apps` | `auto/custom/full`, dropped, `Warm white`, `Color`, `100%`, `No apps` | screenshots |
| 9 | M | Focus ring stayed on a clicked control; after clicking a tile the ring showed a stale tile | Rings only for keyboard focus (`Nav.keys`); mouse click clears it; tile highlight resyncs on keyboard focus only | live click vs Tab screenshots, all 9 popup tab stops show a ring |
| 10 | M | OLED flicker-free toggle wrote `on`, leaving the user's `auto` as `on` | On an OLED panel turning it on sends `auto` | live: off then on gives `setting auto` |
| 11 | M | Offline error read `The name is not activatable` | `tspmod is not running`; `Not authorized`; errors cut to 60 characters | offline screenshot (session bus) |
| 12 | L | One redundant full state push per second (tick) | removed | idle CPU 0.5% to 0.2% (4 ticks in 20 s) |
| 13 | L | Fans & Power min width 2 x 28 rem; 1590 px at 20 pt | 24 rem columns (688 px at 10 pt) | snapshots, live |
| 14 | L | Speculative `RefreshModes` binding that the real daemon never provides | removed | unit test |
| 15 | M | Widget: selected tile washed in the accent (against the design rule), `ROG` and digit hints | neutral selection fill + accent bar like the app, hints removed | `plasmawindowed` against the daemon |

## Checked and fine (real daemon)

- Values match the CLI: profile, 240/60 Hz, brightness, battery %, limit, health 71%, cycles, time
  left, fan rpm (largest of CPU/GPU/Mid), power limits, boot sound, charger, Slash show list, light
  state, idle timeout.
- Writes take effect and come back through `PropertiesChanged`: mode tiles and keys 1-4, GPU
  policy (Optimized then Standard), refresh Auto/60/Max, brightness (drag), keyboard backlight,
  charge limit steps, colour mode, eye care, flicker-free, boot sound, Fn lock (about 1 s),
  backlight power states, idle timeout, Slash show-on, power limits and reset.
- Changes made elsewhere update the GUI live: CLI (brightness, limit, colour, eye care) and the
  KDE power-profile path (`net.hadess.PowerProfiles.ActiveProfile`), accent follows the mode.
- Popup placement bottom-right above the panel (`1352,571 347x458` logical at 1.5x); `--toggle`
  twice opens then closes; Esc closes; a click elsewhere closes; the real `KEY_PROG1` opens and
  closes the installed popup; a second `--window screen` does not duplicate; Esc closes a window
  and the process exits with the last one.
- No scrolling anywhere; sizes at 10/14/20 pt: popup 347x458 / 486x641 / 694x915, Screen 379x386
  /531x540/758x771, System 691x255 / 967x357 / 1382x510.
- Corners: 8x zoom of the popup corners (light and dark) shows anti-aliased rounding, no square
  corner behind it.
- Performance with the real daemon: first present 30 ms, daemon state read 24 ms, RSS 19.3 MB
  (PSS 12.7 MB), 6 threads, no DRI or NVIDIA file descriptors, idle CPU about 0.2% with the popup
  open (the 1 Hz readout), nothing after it closes.

## Not verified / left

- Keyboard effect, colour and speed writes (the keyboard has no effect set, and a write cannot
  be undone to "untouched"); Slash enable/brightness/mode and AniMe writes (visible hardware).
- Hover and pressed surfaces were checked in code only.
- Layouts with missing hardware were not run against a reduced daemon; `sections()` tests and the
  `if Bk.has-*` structure cover them (features are hidden, not greyed).
- The first click right after the popup was launched once did not register (6 later attempts
  did); not reproduced.
- Another agent was benchmarking on the same machine during the audit; two of my mode/refresh
  changes landed inside one of its runs.
