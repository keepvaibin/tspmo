# GUI review

Machine: ASUS GU605MI, Plasma 6 / KWin Wayland, one output 2560x1600 at scale 1.5 (1707x1067
logical), 240 Hz panel (60 Hz while on battery), dGPU asleep. Screens in `docs/img/gui/` are
headless renders (`--snapshot`) of the real UI code; live checks were made on the desktop with
uinput input against the mock daemon (`examples/mockd.rs`, session bus), so no hardware setting
was touched. Measurements use the real `tspmod` on the system bus, read only.

## Windows and controls

| Window / control | How verified | Result |
|---|---|---|
| Popup placement | live: KWin's own window geometry (script) | `x=1352 y=571 347x458`, bottom-right, 8 px above the work area edge, above the floating panel; opens on the active output |
| Popup opens at its final size, filled in | trace | first frame waits for the daemon's first answer (about 10 ms); one present, correct scale (`preferred scale 180/120` arrives with the first configure) |
| Popup closes: Esc | live, uinput | window gone, process exits (code 0) |
| Popup closes: second ROG press | `tspmo-gui --toggle` twice (what the key runs) | opens, then closes, process exits. The binding itself (`Exec=tspmo-gui --toggle`, `Launch (1)`) is unchanged; a real `KEY_PROG1` press was **not** run against this build (see handoff) |
| Popup closes: focus lost | live: opening another window from the popup | popup goes, the window stays, process stays |
| Mode tiles | live: keys `1`-`4`; click; Tab, Left/Right + Enter/Space; Shift+Tab; wheel on the row | each sends the right `SetProfile`; arrows alone send nothing; wheel down = next, up = previous |
| GPU tiles, refresh tiles, charge-limit tiles | live click | `SetGpuMode(hybrid)`, `SetRefreshPolicy(60)`, `Battery1.SetLimit(70)` |
| Brightness slider | live drag from 20 % to about 80 % | `SetBrightness(81)` once, on release |
| Keyboard backlight slider | live click + Right, Right | `SetBrightness` steps |
| Window buttons | live click on each of the four | each opens its window at its layout size and centred by KWin; popup closes; Esc closes each; process ends with the last |
| Fans & Power: curve drag | live: drag point 4 up | `SetCurve` with the point clamped at its neighbour (57 C, 50 %) |
| Fans & Power: curve keys | live: click point, Up Up, Enter | `SetCurve` |
| Fans & Power: Auto | live click | `SetAuto(balanced, 1)` |
| Screen, Keyboard & Lid, System | headless render at five text sizes (10-20 pt) and scales 1 to 2; live open; offline state | no clipping, no scroll; sections follow the interfaces present |
| OSD | live `--osd performance Turbo 60` | bottom centre, overlay, Turbo accent, bar, gone after 1.5 s; first frame 37 ms |
| Offline daemon | headless | only the error line, wrapped, no dead controls |
| Plasma widget | `plasmawindowed org.tspmo.power` against the real daemon | four mode tiles with accents, GPU tiles, "dGPU asleep"/"active" follows the daemon with no polling, "All settings" |
| No layer-shell | `TSPMO_NO_LAYER=1` | popup is a plain titled window; Esc closes and the process ends |

Light and dark: both rendered for popup, Fans & Power and Screen (`docs/img/gui/*light*`). The
theme comes from `kdeglobals`; the compositor draws the window frame.

## Text and scale

Every length is in `rem`; windows are exactly as large as their layout. No window scrolls at any
size below. "Scale" is the compositor scale (fractional), "text" the system font size.

| Window | 10 pt, x1 | 12 pt, x1.25 | 14 pt, x2 | 20 pt (200 %), x1 |
|---|---|---|---|---|
| Popup | 347 x 458 | 416 x 549 | 486 x 641 | 694 x 915 |
| Fans & Power | 795 x 367 | | | 1590 x 734 |
| Screen | 379 x 415 | | | 758 x 830 |
| Keyboard & Lid | 691 x 386 | | | 1382 x 771 |
| System | 691 x 255 | | | 1382 x 510 |

(logical px). At 200 % text the largest window, Fans & Power, is 1590 px wide: it fits a 1707 px
wide logical screen at scale 1 or a 3200 px wide display at scale 2. Buffers are
`round(logical x scale)` pixels, e.g. 972 x 1282 for the 14 pt x2 popup, with a viewport back to
logical size, so 1.25 and 1.5 are sharp (crops checked).

## Corners

The popup and OSD are ARGB surfaces; the rounded shape (0.6 rem, 1 px border) is drawn by the
software renderer with anti-aliased edge coverage over a fully transparent background (checked at
10x zoom over a checkerboard in the headless render, and live). Secondary windows are square.

## Performance (release build, real daemon)

| | before | after |
|---|---|---|
| Binary, as built | 16.5 MB (and 14.0 MB stripped) | 9.5 MB (symbols stripped in the crate's profile) |
| Process start to first frame | 84-91 ms (winit, exec to frame) | 28-30 ms (`main` to frame, 5 runs 28-45 under load) |
| RSS / PSS | 24 / 15 MB | 19.1-19.2 / 12.7-12.9 MB |
| Threads | 8 | 6 |
| GL/Vulkan libraries, DRI/NVIDIA fds | none | none |
| Idle CPU, popup open | 0.00 s / 10 s | 0.01-0.06 s / 10 s (the one-second live readout of fan rpm and battery watts, plus the daemon's own signals) |
| Frame cost (software renderer, scale 1.5) | | avg 1.4 ms, max 4.2 ms |
| Animation frame gaps (60 Hz output) | | median 15.5 ms, p95 16.6-37 ms |

"Before" is the tree at `8023d70`; the 16.5 MB figure is the new UI on the old winit backend,
the first measured binary of this work. The popup has no timer of its own: redraws are paced by
`wl_surface.frame` callbacks and stop when nothing animates; nothing at all runs once the last
window is closed, because the process exits.

How the size went down: no winit (and with it X11 and a second smithay-client-toolkit; the own
Wayland platform uses one), `opt-level = "s"` for the GUI crate and for Slint, the Wayland client and
the text stack only (zbus stays at 3 for the daemon), symbols stripped for the GUI crate. Not
worth it: `opt-level = "z"` (11.19 vs 11.26 MB unstripped). What remains is text shaping (parley,
harfrust, skrifa, ICU data) and zbus.

### Under an Eco+-like CPU cap

Package PL1 written to 5 W on `intel-rapl:0` and `intel-rapl-mmio:0`
(`constraint_0_power_limit_uw` 15 W before, read first, restored right after by a `trap`; the
read-back after the run was 15 W on both):

| | uncapped | 5 W cap |
|---|---|---|
| Process start to first frame (5 runs) | 30.4 ms | 30.8-52.5 ms (median 39.7) |
| Animation: draw avg / max | 1.36 / 4.19 ms | 1.41 / 2.93 ms |
| Animation: frame gap median / p95 | 15.5 / 37.0 ms | 15.3 / 16.6 ms |

The popup is not CPU-bound at 5 W; animation stays at the 60 Hz pace.

## Input

All of this was driven through uinput on the live desktop (keyboard, absolute pointer, wheel),
scripted in the scratchpad: keys `1`-`4`, Tab / Shift+Tab, arrows without applying, Enter and
Space, clicks, wheel direction, slider drag, slider keys, curve drag and keys, Esc. The mock
daemon's log of received calls is the check (`SetProfile(eco-plus, power-saver, balanced,
performance)` for keys 1 to 4, and so on). Slint
receives Shift/Control/Alt/Meta as key events, repeat as `KeyPressRepeated`, text from xkbcommon,
so non-US layouts type as they should.

## Open

- A KEY_PROG1 press against this build (needs the package installed); see handoff.
- Multi-monitor "output with the cursor" relies on KWin choosing its active output for a layer
  surface without an output; one output here.
- `--features winit` (X11) compiles but was not run.
- Doctor findings in System need a daemon method; see handoff "API requests".
