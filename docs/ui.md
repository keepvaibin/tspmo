# UI design

The reference is G-Helper's main window (`~/Downloads/ui1.png`): flat, dense, one column of
sections, each a small icon and a bold title that carries the state, tiles with an icon over the
label, and rows of drop-downs and checkboxes for the secondary options. Lenovo Legion Toolkit is the
model for how a settings window should be organised. The goal is a tool that looks like it was made
for this one job: small type, tight spacing, nothing decorative.

## Reasoning

- **The main window is the whole day-to-day set, about 470 x 615 dp, no scrolling.** Mode (with
  Fans + Power as the fifth tile, like G-Helper), GPU, laptop screen, brightness and visual
  options, Slash lighting, keyboard, charge limit, and three buttons (Screen, Keyboard & Lid,
  System). Curve editors and tables of limits live in their own windows; a flyout that scrolls is
  not a flyout.
- **Headers carry state.** `Mode: Balanced` with `CPU: 52°C  Fan: 2100RPM` right aligned,
  `GPU Mode: iGPU + dGPU` with `dGPU asleep  Fan: 1900RPM`, `Laptop Screen: 240Hz + Overdrive`,
  `Slash Lighting: Bounce`, `Battery Charge Limit: 80%` with the charge line. Live values are in
  the monospace face of the system (kdeglobals `fixed`) so digits do not jitter. Readings come
  only from `Fans1.Rpm` and `Fans1.Temps`, which the daemon reads without waking the dGPU; the GPU
  header shows a GPU temperature only if the daemon lists one and the dGPU is awake, otherwise
  just its state and fan.
- **Selection = outline.** The chosen tile gets a 2 dp outline in its colour (a mode's own colour;
  the accent for everything else) and bold text; its icon takes the colour too. There is no fill
  swap. Keyboard focus is a separate 1.5 dp ring in the text colour, inset inside the control and
  drawn only for keyboard focus (a click draws none). Hover and pressed step the surface;
  unavailable is 38 % opacity and not clickable.
- **One accent, and it means something.** The accent is the colour of the mode in force (Eco+
  lime, Silent teal, Balanced blue, Turbo orange). It colours outlines, slider fills, checkboxes
  and the OSD. Nothing else is coloured, apart from red for errors.
- **Eco+ and the GPU row.** Eco+ is a mode (max battery) and forces the dGPU off, so while it is
  in force the GPU header reads `GPU Mode: Eco (Eco+)`, the Eco tile is outlined and the other
  three are dimmed and inert. Leaving Eco+ shows whatever the daemon reports (`GpuPolicy` picks
  the tile, `GpuMode` the title). No tooltip.
- **Drop-downs** (visual mode, eye care, Slash brightness/mode/interval, keyboard backlight and
  effect) open a list inside the window, down or up (`up`) so it never leaves the surface; lists
  over eight entries flow into columns (17 Slash modes make two). Arrows on the closed control step
  the value, Enter or Space opens the list, Esc closes only the list. The keyboard colour opens a
  row of swatches. An effect is never sent empty: with no effect set the control reads `Choose
  effect` and colour and speed are disabled until one is picked.
- **No gradients, glow, blur or shadows.** Flat fills, a 4 dp radius. The main window and the OSD
  have a 8 dp / 6 dp corner drawn with anti-aliasing on a transparent surface, so there is never a
  square behind a rounded shape; secondary windows are square (the compositor draws the frame).
- **Everything is relative to the text.** All sizes are in `dp` = 1/13.33 of the system font size
  (1 dp is 1 px at the default 10 pt), so layout grows with the font; Wayland fractional scale is
  applied to the buffer. Windows are as big as their layout asks and cannot be resized, so
  nothing scrolls and nothing clips from 100 % to 200 %.
- **Keyboard first.** `1`-`4` choose the mode, `F` `S` `K` `Y` open the windows, Tab walks the
  controls, arrows move inside a control, Enter or Space apply, Esc closes. Arrows in a tile row
  move a highlight and do not apply: a GPU mode switch should never happen by passing over it.

## Windows

| Window | Content |
|---|---|
| Main | Mode (Eco+, Silent, Balanced, Turbo, Fans + Power), GPU (4 tiles), refresh (Auto, 60 Hz, max, Overdrive), brightness + visual mode, eye care, flicker-free, Slash (brightness, mode, interval, show on battery / lid close), keyboard (backlight, effect, color, Fn-Lock), charge limit, window buttons |
| Fans & Power | mode tabs, fan tabs with live rpm, 8-point curve editor (drag, arrows), Auto / Full / Factory; AC / Battery limits with sliders and per-limit reset |
| Screen | refresh policy, brightness, flicker-free, colour mode, eye care kelvin, panel overdrive and Mini-LED |
| Keyboard & Lid | backlight, effect, colour, speed, power states, idle timeout; Slash enabled, brightness, mode, interval, every show-on flag; AniMe |
| System | charge limit and charge once, battery table, Fn lock, firmware toggles, reboot, dGPU users |

A section exists only if the machine has the hardware or the daemon exposes the interface.

Writing: values read `82%`, `240Hz` (headers, tiles) or `240 Hz` (windows), `41.9 W`, `87 °C`,
`2100RPM` in the live headers like G-Helper; US spelling (Color); a value the firmware has not
reported is shown as `Choose effect` or `Firmware`, never as an empty control. Sustained <= boost <=
fast boost is kept by the UI.

## Tokens

Sizes: `dp` = system font size / 13.33 (1 px at 10 pt). `rem` = the font size itself.

| | dark | light |
|---|---|---|
| window | `#1d1e20` | `#f3f3f4` |
| raised (controls) | `#2d2f33` | `#e4e5e7` |
| hover | `#383b40` | `#d6d8db` |
| pressed | `#474b52` | `#c0c4c9` |
| selected (list row) | `#393d43` | `#cdd0d5` |
| line | `#383b40` | `#cfd1d5` |
| text | `#eceded` | `#17181a` |
| dim | `#a1a6ad` | `#5a5e65` |
| focus ring | `#f4f5f6` | `#17181a` |
| error | `#ff7070` | `#c32f2f` |
| on accent | `#0f1113` | `#ffffff` |
| Eco+ | `#8cd44a` | `#4f9a14` |
| Silent | `#35c9a6` | `#0e9d7e` |
| Balanced (default) | `#4d9cf7` | `#1f74dc` |
| Turbo | `#f2673b` | `#d6481c` |

| Element | Size |
|---|---|
| body text | 0.9 rem (9 pt at 10 pt); headings 0.98 rem bold; live readings and small print 0.85 rem |
| main window | 470 dp wide, 16 dp side padding, 14 dp between sections, 6 dp inside |
| section head | 20 dp, icon 15 dp |
| mode / GPU tile | 58 dp high, icon 18 dp, 6 dp gap |
| refresh tile, button | 26 dp |
| drop-down | 24 dp, list rows 21 dp |
| checkbox | 14 dp, 2 px radius |
| slider | 4 dp track, 14 dp round thumb, 18 dp hit height |
| radius | 4 dp; main window 8 dp; OSD 6 dp |

Motion: 100 ms surface, 140 ms selection, nothing animates while idle. The theme (light or dark)
comes from `kdeglobals`' window colour; the mode accent from the daemon.

## On-screen display

`tspmo-gui --osd <icon> <text> [0-100]`: a layer-shell overlay surface anchored bottom centre,
10 % of the output height above the usable area (so above a panel), no keyboard, empty input
region (clicks pass through), no taskbar entry. Icon (the UI's own line icons, picked from the
theme icon name or mode id), text, an optional 3 dp bar in the accent of the mode. 90 ms fade in,
hold, 220 ms fade out, gone after 1.2 s. It is deliberately slim: 7 dp padding, 16 dp icon.

## Reference comparison

`docs/img/gui/` and the review screenshots put the main window next to G-Helper's. Same order of
sections, same tile language and outline selection, same rows of drop-downs and checkboxes; the
differences are deliberate: Eco+ is a fifth mode with its own colour, the GPU row is locked while
Eco+ is on, Slash and keyboard options come from the daemon's API, and everything scales with the
system font.
