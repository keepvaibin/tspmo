# UI design

The reference is G-Helper (flat, dense, one column of labelled rows) and Lenovo Legion Toolkit for
how a settings window should be organised. The goal is a tool that looks like it was made for this
one job: crisp, aligned, with state shown the way an instrument shows it, not a card-and-icon
dashboard.

## Reasoning

- **Popup = the day-to-day set, no scrolling.** Mode, GPU, screen (refresh, brightness),
  keyboard backlight, charge limit, and four buttons. Everything else opens in its own window:
  Fans & Power, Screen, Keyboard & Lid, System. A window is the right container for a curve
  editor or a table of limits; a flyout that scrolls is not a flyout.
- **Labels, not sentences.** A section is a dim caption, a hairline, and a mono readout:
  `GPU ───────── dGPU asleep`. The hairline makes the caption and the value read as one record
  and gives every section the same left and right edge without boxes around them.
- **State is read, not decorated.** Live values (rpm, W, %, kelvin) are in the monospace face
  of the system (kdeglobals `fixed`), right aligned, so digits do not jitter: Slint has no
  tabular-figure feature, a fixed face is the honest substitute. There are no decorative icons.
  The few glyphs left (check mark, stepper arrows, reset) carry a meaning.
- **One accent, and it means something.** The accent is the colour of the mode in force
  (Eco+ lime, Silent teal, Balanced blue, Turbo orange). It colours the selection bars, slider
  fills, checkboxes and focus-adjacent marks everywhere, so the whole UI tells you the mode at
  a glance. The four mode tiles also carry their own colour as a thin bar at the bottom, dim when
  unchosen, so the colour coding is learned before it is needed. Nothing else is coloured, apart
  from red for errors.
- **Selection = tint + bar.** The chosen tile gets a flat 20 % accent tint and a 3 px bar on its
  bottom edge, bold text. No outline that doubles as a focus ring, no fill swap that reads as
  pressed. Focus is a 2 px ring in the text colour, drawn inside the control; hover and pressed
  step the surface; disabled is 40 % opacity.
- **No gradients, glow, blur, shadows or pastel.** Flat fills, a 1 px border, 3 px radius. The
  flyout itself has an 8 px-ish corner (0.6 rem) drawn with anti-aliasing on a transparent
  surface, so there is never a square behind a rounded shape; windows are square (the
  compositor draws the frame).
- **Everything is relative to the text.** All sizes are in `rem` (the system font size read from
  kdeglobals) so layout grows with it; Wayland fractional scale is applied to the buffer. The
  spacing unit is 0.6 rem, which is 8 px at the default 10 pt. Windows are as big as their layout
  asks and cannot be resized, so nothing scrolls and nothing clips, from 100 % to 200 %.
- **Keyboard first.** `1`-`4` choose the mode, `F` `S` `K` `Y` open the windows (hints are shown
  on the buttons), Tab walks the controls, arrows move inside a control, Enter or Space apply,
  Esc closes. Arrows in a tile row move a highlight and do not apply: a GPU mode switch should
  never happen by passing over it.

## Windows

| Window | Content |
|---|---|
| Popup | Mode (4 tiles, wheel), GPU (4 tiles + dGPU state), Screen (Auto / 60 Hz / Max, brightness), keyboard backlight, charge limit steps, window buttons |
| Fans & Power | mode tabs, fan tabs with live rpm, 8-point curve editor (drag, arrows), Auto / Full / Factory; AC / Battery limits with sliders and per-limit reset |
| Screen | refresh policy, brightness, flicker-free, colour mode, eye care kelvin, panel overdrive and Mini-LED |
| Keyboard & Lid | backlight, effect, colour, speed, power states, idle timeout; Slash enabled, brightness, mode, show-on; AniMe |
| System | charge limit and charge once, battery table, Fn lock, firmware toggles, reboot, dGPU users |

A section exists only if the machine has the hardware or the daemon exposes the interface.

## Tokens

Sizes: `rem` = system font size (default 10 pt = 13.33 px). `u` = 0.6 rem.

| | dark | light |
|---|---|---|
| window | `#1b1c1e` | `#f4f4f5` |
| raised (controls) | `#26282b` | `#e8e9eb` |
| hover | `#303338` | `#dcdee1` |
| pressed | `#3b3f45` | `#cdd0d4` |
| line | `#34373c` | `#cdd0d4` |
| text | `#e9eaec` | `#16171a` |
| dim | `#9a9fa6` | `#5b5f66` |
| focus ring | `#f2f3f5` | `#16171a` |
| error | `#ff7070` | `#c32f2f` |
| on accent | `#0f1113` | `#ffffff` |
| Eco+ | `#8cd44a` | `#4f9a14` |
| Silent | `#35c9a6` | `#0e9d7e` |
| Balanced (default) | `#4d9cf7` | `#1f74dc` |
| Turbo | `#f2673b` | `#d6481c` |

| Element | Size |
|---|---|
| control height | 2.4 rem (mode tiles 2.8 rem) |
| radius | 3 px; flyout 0.6 rem |
| text | 1 rem; captions and readouts 0.85 rem; key hints 0.7 rem |
| slider | 0.3 rem track, 0.45 x 1.3 rem thumb, 2 rem hit height |
| checkbox | 1.1 rem, 2 px radius |
| flyout | min 26 rem wide, padding 1.2 rem, sections 1.1 rem apart, 0.4 rem inside a section |
| tile gap | 0.3 rem |

Motion: 100 ms surface, 140 ms selection bar, nothing animates while idle. The theme (light or
dark) comes from `kdeglobals`' window colour; the mode accent from the daemon.

## Reference comparison

G-Helper's main window is the layout model (mode, GPU, screen, lighting, battery, in that order)
but it scrolls on a small display and mixes outline and fill selection. Here the same sections
fit one 26 rem column, the selection language is one rule, and the long tail moved to windows
like G-Helper's "Fans + Power" and "Extra". Legion Toolkit's two-column settings pages are the
model for the windows (Keyboard & Lid, System).
