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

- **Doctor findings** for the System window: nothing over D-Bus today. Wanted:
  `org.tspmo.Daemon1.Doctor() -> a(sss)` (`severity`, `topic`, `text`), the same list `tspmo
  doctor` prints. The GUI must not read sysfs or run the CLI.
- **Temperatures** next to the fan rpm (the G-Helper mode header shows CPU/GPU temperature):
  `Fans1` could expose `Temps -> a(us)` read on demand like `Rpm`.
- **Per-mode firmware curve for fans with no custom curve** is already shown from `Factory`;
  nothing needed.

## Not done / limits

- ROG key end to end with this build: the shortcut binding is unchanged (`Exec=tspmo-gui
  --toggle`, `X-KDE-Shortcuts=Launch (1)`), the toggle protocol was verified by running
  `--toggle` twice, but installing a user shortcut or the package was not done here, so a real
  KEY_PROG1 press was not run against the new binary. Do it after `makepkg` + `pacman -U`.
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
