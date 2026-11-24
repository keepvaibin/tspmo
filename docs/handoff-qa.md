# QA handoff (package D)

## Done
- Fn-lock: `core/asus/fnlock.rs` (layout, measured GU605 scancodes), `tspmod/asus/fnlock.rs` (hwdb write, `systemd-hwdb update`, `udevadm trigger --settle` on the keyboard node, stale hwdb.bin refresh at start), `Hardware1.FnLock`, `FnLockAvailable`, `SetFnLock`. Off restores native keys explicitly, then removes the file and `/etc/udev/hwdb.bin`. `tspmod.service` gets `ReadWritePaths=/etc/udev`.
- Hotkeys: keycodes checked against cachyos-7.3.5-2 `hid-asus.c` and `asus-nb-wmi.c` (all match). Kernel uevent netlink socket in the input thread rescans devices when an input device is added or removed (event-driven).
- `tools/test/e2e.sh [--idle N] [--fnlock]`, `docs/parity.md`.

## Known issues
- Fn+Esc on 19b6 is a plain Esc: no hotkey toggle, no OSD. Fn+F5 is consumed by the kernel when the WMI call succeeds (source says the event is blocked); with Fn-lock on, plain F5 gives `KEY_PROG4` and nothing cycles the thermal mode yet. Next step: on `Hotkey::Fan` from the keyboard, compare `platform_profile` after 300 ms and call `Daemon1.CycleProfile` if unchanged.
- e2e dGPU check fails here: `runtime_active_time` grows about 20 s per 30 s even with tspmod stopped (something else uses the dGPU). Compare against a baseline with tspmod stopped, or run it on a quiet system.
- Fn-lock keymap on the live keyboard was verified through `systemd-hwdb query`; key presses were not tested.
- Slash, AniMe, power limits and fan curves are not exercised by e2e (read only).

## Resume
```
cd packaging/arch && CARGO_BUILD_JOBS=4 nice -n 19 makepkg -f && sudo pacman -U --noconfirm tspmo-git-0.1.0-1-x86_64.pkg.tar.zst && sudo systemctl daemon-reload && sudo systemctl restart tspmod
tools/test/e2e.sh --fnlock
```
