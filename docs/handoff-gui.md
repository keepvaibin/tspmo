# Handoff: tspmo-gui

Second pass on the GUI (redesign, layer-shell platform, secondary windows). Numbers and how each
control was verified are in [review-gui.md](review-gui.md); the design reasoning and tokens in
[ui.md](ui.md); behaviour and keys in [gui.md](gui.md).

## State

- `crates/tspmo-gui`: a Slint UI on its own Wayland platform (`src/wl`, smithay-client-toolkit
  0.19, software renderer, no winit in the default build). Popup = layer-shell surface, bottom
  right, active output; Fans & Power, Screen, Keyboard & Lid, System = fixed-size toplevels;
  OSD = overlay layer surface. Fractional scale through `wp_fractional_scale_v1`.
- UI in `ui/`: `theme.slint` (tokens), `controls.slint`, `curve.slint`, `popup.slint`,
  `windows.slint`, `main.slint` (exports, OSD). Every length is in `rem`.
- `src/app.rs`: one `App` (thread local) owns the store and every open window; `push_to` fills a
  window's `Bk` global (each window has its own copy of the globals).
- `--snapshot f.ppm [--window w] [--scale s]` renders headless; `TSPMO_FONT`, `TSPMO_THEME`,
  `TSPMO_TRACE=1` (timings), `TSPMO_DUMP=f.ppm` (presented frames) help testing.
- Plasma widget `data/plasma/org.tspmo.power` in the same language (`Tile.qml`, `Head.qml`).
- Release profile: GUI crate and its UI/Wayland/text dependencies at `opt-level = "s"` (see root
  `Cargo.toml`); zbus stays at 3 for the daemon.

## API requests (daemon)

- **On-screen display:** call tspmo's own OSD instead of plasmashell's (which appears mid-screen).
  Exact command line, icon is a theme icon name or a mode id, `[value]` 0-100 is optional:
  `tspmo-gui --osd <icon> <text> [value]`, for example
  `tspmo-gui --osd battery-profile-performance "Turbo"`,
  `tspmo-gui --osd eco-plus "Eco+"`, `tspmo-gui --osd display-brightness "Brightness 40%" 40`,
  `tspmo-gui --osd input-keyboard "Backlight High" 100`. Run it as the user (the session's
  `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` must be set, as for the existing session OSD helper); it
  shows in about 40 ms, stays 1.2 s and exits, nothing else to clean up. No focus, no taskbar
  entry, clicks pass through; several calls simply overlap, so debounce rapid changes.
- **Eco+ must force the GPU to Eco.** The GUI shows Eco selected and the other GPU tiles
  unavailable whenever `Daemon1.Profile == "eco-plus"`, but the daemon (checked live) leaves
  `GpuMode` at `hybrid` and `GpuPolicy` unchanged while Eco+ is on, and does not refuse
  `SetGpuMode` then. Wanted: while Eco+ is in force the dGPU is off, `GpuMode`/`GpuPending`
  reflect it (or the daemon rejects `SetGpuMode` with a short message), and leaving Eco+ restores
  the user's `GpuPolicy`.
- **Lighting:** `Lighting1.Effect` is empty (`""`) until an effect is chosen and the GUI will
  not send an empty mode; `SetEffect` should accept every entry of `Modes` and the daemon should
  report the effect it applied. `Lid1.SlashModes` lists 16-17 modes: all are shown. Nothing else
  is missing for the Slash row (brightness `SlashBrightness` 0-255 is shown as Off/Low/Medium/High
  = 0/85/170/255; `SlashInterval` 0-5 as Interval Off/1-5; show-on flags `battery` and `lid` in the
  main window, all six in Keyboard & Lid). If the intended range of `SlashInterval` is not 0-5 or
  its unit is not seconds, say so and the labels change.
- **GPU temperature:** `Fans1.Temps` lists only `CPU`. G-Helper shows a GPU temperature too; the
  GUI shows `GPU: NN°C` in the GPU header only if an entry labelled `GPU` appears and the dGPU is
  awake. If the daemon has a source that never wakes the dGPU (an iGPU or EC sensor), list it.
- **Power-mode power limit** (`Mode: Turbo+ 25W` in G-Helper): the GUI does not show a wattage in
  the mode header; expose the effective sustained limit (`Power1`) if wanted.

- **Doctor findings** for the System window: nothing over D-Bus today. Wanted:
  `org.tspmo.Daemon1.Doctor() -> a(sss)` (`severity`, `topic`, `text`), the same list `tspmo
  doctor` prints. The GUI must not read sysfs or run the CLI.
- **Temperatures** next to the fan rpm (the G-Helper mode header shows CPU/GPU temperature):
  `Fans1` could expose `Temps -> a(us)` read on demand like `Rpm`.
- **Per-mode firmware curve for fans with no custom curve** is already shown from `Factory`;
  nothing needed.

## Not done / limits

- ROG key end to end with this build: the shortcut moved to the hidden helper
  `org.tspmo.gui-toggle.desktop` (silent: `StartupNotify=false`, `X-KDE-StartupNotify=false`,
  `NoDisplay=true`); the launcher `org.tspmo.gui.desktop` has no shortcut. The old behaviour (a
  launch-feedback placeholder left in the task manager after the closing press) was reproduced
  live; the fix could not be checked live because installing desktop files was not done here.
  After `makepkg` + `pacman -U` (and a kglobalacceld restart or re-login, because the old
  `org.tspmo.gui.desktop` component may linger with the same key): `qdbus6 org.kde.kglobalaccel
  /component/org_tspmo_gui_toggle_desktop invokeShortcut _launch` twice, screenshot the taskbar
  after each (no bounce, no entry), or press the real ROG key.
- X11: only with `--features winit` (not built by default). Without layer-shell the popup is a
  plain toplevel (compositor places it); untested on GNOME.
- Fonts: Slint has no tabular-figure feature; live numbers use the `fixed` face instead.
- No `femtovg`/GL renderer is built by default (`--features gl` still compiles it).

## Resume

```
cd <worktree>
CARGO_BUILD_JOBS=4 nice -n 19 cargo build --release -p tspmo-gui      # slow: fat LTO
cargo run -p tspmo-gui --example mockd &                              # fake tspmod, session bus
TSPMOD_BUS=session TSPMO_GUI_ID=x target/release/tspmo-gui --snapshot /tmp/a.ppm --window fans
ffmpeg -y -i /tmp/a.ppm /tmp/a.png
cargo fmt -p tspmo-gui && cargo clippy -p tspmo-gui --all-targets -- -D warnings && cargo test -p tspmo-gui
```

## Known issues

- Never test input injection or screenshots on the live desktop while the user works there;
  `spectacle -a` captures the focused window. Crop full-screen captures to the popup only.
- `TSPMO_GUI_ID` isolates test instances from the real `@tspmo-gui-<uid>` socket.
- Slint only re-evaluates a window's size after the window has been drawn once; a platform that
  never renders will never see layout changes (the headless snapshot renders in its wait loop for
  this reason).
