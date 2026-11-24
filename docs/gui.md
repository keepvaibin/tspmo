# tspmo-gui, the Plasma widget and the ROG key

Visual rules and tokens: [ui.md](ui.md). State and numbers: [review-gui.md](review-gui.md).

## Program (`tspmo-gui`)

```
tspmo-gui [--toggle]                          open the popup, or close it if it is open
tspmo-gui --window fans|screen|keys|system    open that window (also from the popup)
tspmo-gui --osd <icon> <text> [0-100]         1.5 s overlay with an optional bar
tspmo-gui --install-shortcut                  bind the ROG key for this user (unpackaged builds)
tspmo-gui --snapshot f.ppm [--window w] [--scale 1.5]   render once, headless
```

### Windows and placement

- **Popup:** a `wlr-layer-shell` surface (layer top, anchored bottom-right, 8 px margin, exclusive
  zone 0). KWin keeps it above the panel and puts it on the active output, the one with the
  cursor, every time. Keyboard interactivity is *on demand*: the popup gets the keyboard when
  it opens and closes when focus moves elsewhere (a click outside, another window).
- **Fans & Power, Screen, Keyboard & Lid, System:** ordinary `xdg_toplevel` windows with
  server-side decorations, fixed to the size their layout asks for (min = max). They stay when
  the popup closes; the process ends with its last window.
- **OSD:** a layer surface (layer overlay, bottom centre, no keyboard, empty input region).
- Without layer-shell the popup is a plain toplevel. Without Wayland the program needs a build
  with `--features winit` (Slint's winit backend, X11); the default build has no winit at all.
- One instance: the first owns the abstract socket `@tspmo-gui-<uid>`. A second launch sends
  `toggle` (popup) or `open <window>` and exits. Esc, a second ROG press and focus loss close the
  popup. Nothing stays resident once the last window is closed.

### Platform (`src/wl`)

A Slint platform on `smithay-client-toolkit` 0.19 with the software renderer:

- Wayland callbacks only queue work; the loop then feeds Slint with nothing borrowed
  (`wl/mod.rs`, `wl/state.rs`). One calloop loop, timers from Slint, jobs from
  `invoke_from_event_loop`.
- Buffers: `wl_shm` `Argb8888`, premultiplied, through a `SlotPool`; the renderer draws into a
  reused canvas (only changed regions) and the canvas is copied into a fresh buffer. Frame
  callbacks pace redraws, so animations run at the output's refresh rate and nothing is drawn
  while idle. No GL, no Vulkan, no EGL: the dGPU cannot be woken.
- Scale: `wp_fractional_scale_v1` + `wp_viewporter`: the buffer is `round(logical * scale)`
  pixels and the viewport maps it back, so 1.25x and 1.5x are sharp.
- Input: keyboard through xkbcommon (sctk, with key repeat), pointer with `wp_cursor_shape_v1`
  (no cursor theme is loaded), wheel as `PointerScrolled`.
- Sizes: the window asks for its layout's preferred size (`update_window_properties`); the
  surface follows when the content changes (a section appears).

### Text and scale

Font and size come from `kdeglobals` (`[General] font=`, `fixed=`; `TSPMO_FONT="Family,pt"`
overrides). 10 pt is 13.33 px at 96 dpi; every length in the UI is in `rem`. Dark or light from
`[Colors:Window]`'s background (`TSPMO_THEME=light|dark` overrides).

### The daemon

**The GUI reads nothing from sysfs or /proc.** Everything comes from `tspmod` over the system
bus (`docs/dbus.md`): `GetAll` for each interface in parallel, then `PropertiesChanged`. Live
fan speed and battery power are two `Properties.Get` calls once a second while a window exists
(the daemon reads them on demand). Interfaces that are absent hide their window or section.

- Start: the layer surface exists at once, but the first frame waits for the daemon's first
  answer (a few ms, 120 ms at most) so the popup opens at its final size, filled in.
- Screen names are in `crates/tspmo-gui/src/bindings.rs`; values are read leniently and written
  in the type the introspected method declares.

### Keys

Popup: `1`-`4` modes, `F` `S` `K` `Y` open Fans & Power, Screen, Keyboard & Lid, System, Tab and
Shift+Tab move focus, Left/Right move inside a tile row or slider (tile rows: Enter or Space
applies), Shift makes slider steps five times larger, Home/End jump, the wheel steps sliders and
the mode and refresh rows, Esc closes. Fan curve: arrows move the selected point, Ctrl+Left/Right
(or `,` `.`) select another, Shift is the fine step, Enter or leaving the editor sends it.

### Trying every window without the hardware

```
cargo run -p tspmo-gui --example mockd &       # a tspmod look-alike on the session bus
TSPMOD_BUS=session TSPMO_GUI_ID=x tspmo-gui --snapshot /tmp/a.ppm --window fans --scale 1.5
TSPMOD_BUS=session TSPMO_GUI_ID=x TSPMO_FONT="Noto Sans,16" tspmo-gui
```

`TSPMO_GUI_ID=x` gives a test instance its own socket; `TSPMO_TRACE=1` prints timings;
`TSPMO_DUMP=f.ppm` writes every presented frame to a file.

## ROG key

- Key: ASUS Armoury / ROG key. On this laptop (GU605MI) the USB keyboard `0b05:19b6` (hid-asus)
  reports it as `KEY_PROG1` (148), which xkb (`<I156>`) turns into `XF86Launch1` and Qt calls
  `Launch (1)`. The `Asus WMI hotkeys` device reports the same key as `KEY_PROG3` (202) on some
  models, but on the USB keyboard `KEY_PROG3` is the Aura key, so only `KEY_PROG1` is bound.
- Binding: `org.tspmo.gui.desktop` with `X-KDE-Shortcuts=Launch (1)` in `/usr/share/applications`
  and `/usr/share/kglobalaccel` (KDE reads shortcuts from both). `tspmo-gui --install-shortcut`
  writes the same files under `~/.local/share` and runs `kbuildsycoca6`.
- The desktop entry runs `tspmo-gui --toggle`: the first press opens the popup, the second closes
  it.

## Plasma widget (`data/plasma/org.tspmo.power`)

Plasma 6 plasmoid replacing the power-profile applet: four modes (keys `1`-`4`) and the GPU mode
with the dGPU state, in the same tile language as the popup (`Tile.qml`, `Head.qml`; accents per
mode, dark and light variants chosen from the Plasma theme). It uses
`org.kde.plasma.workspace.dbus` (`Properties` follows `PropertiesChanged`, `SystemBus.asyncCall`
sends changes), so there is no polling. Middle click or "All settings" opens the popup. Install
by package, or `kpackagetool6 -t Plasma/Applet -i data/plasma/org.tspmo.power`; try it with
`plasmawindowed org.tspmo.power`.
