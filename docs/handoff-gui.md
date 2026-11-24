# Handoff: tspmo-gui (work package C)

## Done (all committed on the work-package-C branch)

- `crates/tspmo-gui`: Slint popup, software renderer on winit, no GL, nothing read from sysfs
  (everything over D-Bus), one instance via `@tspmo-gui-<uid>`, `--toggle`, `--section main|fans`,
  `--osd`, `--install-shortcut`, `--snapshot`.
- G-Helper style UI (see `docs/ui.md`): one flat page (Mode, GPU, Screen, Dimming/Visual mode, Slash,
  Keyboard, Battery, System, footer), second page Fans + Power (draggable 8-point curves, limits).
  Square 468 x 640 window, so no corner artifacts. Light and dark theme from `kdeglobals`.
- Sections follow the interfaces present at runtime; Display1 names are in `src/bindings.rs`.
- ROG key: `org.tspmo.gui.desktop` (`X-KDE-Shortcuts=Launch (1)`, `KEY_PROG1`), verified end to
  end with a uinput key press (opens, second press closes). Details in `docs/gui.md`.
- Plasma widget `data/plasma/org.tspmo.power` (D-Bus QML bindings, no polling), PKGBUILD updated,
  CI apt packages added.
- Mock daemon `examples/mockd.rs` (session bus) and headless `--snapshot` for visual review.
- Measured (software renderer, real daemon, dev machine, AC, before the redesign): exec to first
  frame 84-91 ms, RSS 24 MB (PSS 15 MB), idle CPU 0.00 s per 10 s, 8 threads, about 0.4 context
  switches/s, no DRI/NVIDIA fds or GL libraries mapped. Release binary 16 MB.

## Not done

1. **Bottom-right placement.** The popup is a normal toplevel, so KWin centres it. Plan: a custom
   Slint platform on `smithay-client-toolkit` 0.19.2 (already in the lock via winit) using
   `zwlr_layer_shell_v1` (anchor bottom|right, exclusive zone 0, keyboard interactivity
   exclusive, `output: None` so KWin uses the active output), `wp_fractional_scale_v1` +
   `wp_viewporter` for sharp 1.5x, `MinimalSoftwareWindow` rendering into a `SlotPool` buffer
   (`Argb8888`; a `TargetPixel` for it exists in `src/snapshot.rs`), a calloop loop with
   `EventLoopProxy` for `invoke_from_event_loop`, sctk keyboard (repeat) and pointer events mapped
   to `WindowEvent`s. Template: sctk `examples/simple_layer.rs`. Fall back to winit when the
   compositor lacks layer-shell. The same platform gives a top-centre OSD. A KWin window rule
   cannot express "bottom-right of the work area", so it was not used.
2. Eco+ measurements under a RAPL cap (CPU capped to 5 W: first-frame time, drag smoothness):
   not run; restore limits after each run (read them first).
3. Renderer comparison (femtovg on the iGPU vs software): software only so far (`--features gl`).
4. Binary size (16 MB): try `[profile.release.package."*"] opt-level = "s"` (needs agreement,
   affects the daemon) or drop X11 (`backend-winit-wayland` once the layer platform exists).
5. Keyboard focus does not scroll the page; light-theme and OSD not checked on the live desktop.

## Resume

```
cd <worktree>
CARGO_BUILD_JOBS=4 nice -n 19 cargo build --release -p tspmo-gui --example mockd   # slow: fat LTO
cargo run --release -p tspmo-gui --example mockd &          # fake tspmod on the session bus
TSPMOD_BUS=session TSPMO_GUI_ID=x target/release/tspmo-gui --snapshot /tmp/a.ppm [--section fans]
ffmpeg -y -i /tmp/a.ppm /tmp/a.png                           # view it
TSPMO_THEME=light ...                                        # light theme
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Known issues

- Never test input injection or screenshots on the live desktop while the user works there
  (`spectacle -a` captures the focused window, which may be their terminal).
- `TSPMO_GUI_ID` isolates test instances from the real `@tspmo-gui-<uid>` socket.
- Cargo.lock: slint pulled zbus 5.19 (was 5.13); other branches will need a lock merge.

## Update after merge of master (8ef3a58)

Done since: dGPU line now reads "dGPU off" (Eco) / "dGPU asleep" / "dGPU active" (unit-tested);
Screen bindings already match `docs/dbus.md` Display1 (Rates, Refresh, RefreshPolicy,
FlickerFree/FlickerSetting, Brightness, ColorMode(s), EyeCare); the four modes incl. Eco+ work.
Nothing was run that needed restoring (no RAPL changes, no test processes left).

Still open, in this order:
1. Layer-shell platform (new `src/layer.rs`, add `smithay-client-toolkit = "0.19"` default
   features). sctk 0.19.2 API checked: `LayerShell::bind`, `create_layer_surface(qh, surface,
   Layer::Top, Some("tspmo"), None)`, `set_anchor(Anchor::BOTTOM | Anchor::RIGHT)`,
   `set_margin(0, 12, 12, 0)`, `set_exclusive_zone(0)`, `set_keyboard_interactivity`,
   `set_size`; keyboard repeat via `seat_state.get_keyboard_with_repeat(qh, &seat, None,
   loop_handle, callback)` (needs `delegate_keyboard!(State)`); cursor via
   `get_pointer_with_theme` (see sctk `examples/themed_window.rs`); fractional scale and
   viewporter via `reexports::protocols::wp::{fractional_scale,viewporter}` with
   `GlobalData` Dispatch impls. Render with `MinimalSoftwareWindow` and the `Px` TargetPixel in
   `src/snapshot.rs`; use `slint::platform::set_platform` only when layer-shell binds, else winit.
2. Re-verify input after the redesign (keys, wheel, drag, ROG key) only when the user is away
   (`loginctl show-session` IdleHint); light theme and OSD via `--snapshot`/`--osd`.
3. Binary size experiment, then Eco+-cap timing (read RAPL limits first, restore after).
