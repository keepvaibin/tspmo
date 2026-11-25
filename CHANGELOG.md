# Changelog

## 0.9.0

First release. Summarised from the git history, by feature.

### Daemon and power
- `tspmod`: event-driven systemd service (no polling, 0 wakeups/s at idle) that implements the
  power-profiles-daemon D-Bus API itself, so KDE and GNOME keep working.
- Four modes: Eco+, Silent, Balanced, Turbo. Fn+F5 cycles them with one OSD per press.
- Eco+: lowest idle power. dGPU off, tuned CPU limits and EPP, lighting and lid display idle-off.
  A late firmware thermal write re-applies fan curve and limits.
- Idle cost trimmed (patched `blocking` pool whose idle threads exit).

### GPU
- Modes Eco, Standard, Ultimate, Optimized; live Eco switching with rollback.
- Names the app holding the NVIDIA GPU, and can close it for Eco (`tspmo gpu users`).
- Boot-safe: the policy waits until boot has settled; NVIDIA state is never read in ways that
  wake the dGPU; NVIDIA unload deadlock prevention (GPU first, quiet window, rmmod watchdog).
- `tspmo-run` runs a program on the NVIDIA GPU; everyday apps render on the iGPU.

### ASUS hardware
- Fan curves, power limits (PL1/PL2/PPT), battery charge limit, firmware toggles.
- Keyboard lighting profiles, Slash and AniMe Matrix lid displays.
- Software Fn-lock, hotkey and keyboard rediscovery through udev.
- `tspmo-hid-asus-dkms`: lets the keyboard and lighting USB chips autosuspend (about 0.6 W).

### Display
- Refresh rate, flicker-free dimming, colour modes, eye care (KDE).

### GUI and desktop
- `tspmo-gui`: Slint popup bottom-right (layer-shell, fractional scale) plus Fans, Screen,
  Keys and System windows; about 9.5 MB and a 30 ms first frame.
- OSD that updates in place; ROG-key shortcut (`tspmo-gui --install-shortcut`).
- Plasma 6 power-mode widget.

### CLI and diagnostics
- `tspmo info | status | profile | gpu | fans | limits | battery | light | lid | display`.
- `tspmo doctor [--apply]`: finds what breaks features or wastes battery, with exact fixes.
- `tspmo bench`: measures what a setting costs or saves.
- `tspmo dump`: anonymised hardware snapshot for support requests.

### Packaging
- Arch packages `tspmo` and `tspmo-hid-asus-dkms`, a release tarball with SHA256SUMS,
  and `install.sh` / `uninstall.sh`.
