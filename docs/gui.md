# tspmo-gui, the Plasma widget and the ROG key

## Popup (`tspmo-gui`)

```
tspmo-gui [--toggle] [--section <id>]   open; if one is open, close it
tspmo-gui --osd <icon> <text> [0-100]    1.5 s overlay, optional bar (desktops without their own OSD)
tspmo-gui --install-shortcut             bind the ROG key for this user (unpackaged builds)
```

- Slint with the **software renderer** on winit (Wayland, X11 fallback). It never creates a GL or
  Vulkan context, so the NVIDIA GPU cannot be woken; `__EGL_VENDOR_LIBRARY_FILENAMES` (Mesa),
  `DRI_PRIME=0` and friends are set before anything starts anyway. `--features gl` builds the
  femtovg renderer instead.
- **The GUI reads nothing from sysfs or /proc.** Everything comes from `tspmod` over the system
  bus (`docs/dbus.md`). Live fan speed and battery power are `Properties.Get` calls once a second,
  only while the Fans or Battery page is open; every other page is event driven.
- One instance: the first owns the abstract socket `@tspmo-gui-<uid>`; a second launch sends it
  `quit` and exits. Esc and the close button also quit. Nothing stays resident.
- Start-up: the window is shown at once with a skeleton. A thread connects, subscribes to
  `PropertiesChanged`, then `GetAll`s every interface in parallel and introspects the object.
  Interfaces that are absent (older daemon, other hardware) hide their page.
- Pages: GPU, Screen, Battery, Fans, Power, Light, Lid, System. Eco+ hides itself if the daemon
  rejects it as an unknown profile.
- Keys: `1`-`4` modes, `PgUp`/`PgDn` pages, `Tab` moves focus, arrows change segments, sliders and
  steppers, `Enter`/`Space` press. Fan curve: arrows move the selected point, `Ctrl+←/→` (or `,`
  `.`) select another, `Shift` is the fine step, `Enter` or leaving the editor sends it.

### Screen bindings

All `org.tspmo.Display1` names are in `crates/tspmo-gui/src/bindings.rs`. Values are read
leniently (`FlickerFree` bool or `FlickerSetting` string, `EyeCare` kelvin or bool,
`Rates`/`RefreshModes`) and written in the type the introspected method declares.

### Trying every page without the hardware

```
cargo run -p tspmo-gui --example mockd &       # a tspmod look-alike on the session bus
TSPMOD_BUS=session cargo run -p tspmo-gui
```

`TSPMO_GUI_ID=x` gives a test instance its own socket. `TSPMO_THEME=light|dark` forces a theme
(otherwise `kdeglobals`' window colour decides).

## ROG key

- Key: ASUS Armoury / ROG key. On this laptop (GU605MI) the USB keyboard `0b05:19b6` (hid-asus)
  reports it as `KEY_PROG1` (148), which xkb (`<I156>`) turns into `XF86Launch1` and Qt calls
  `Launch (1)`. The `Asus WMI hotkeys` device reports the same key as `KEY_PROG3` (202) on some
  models, but on the USB keyboard `KEY_PROG3` is the Aura key, so only `KEY_PROG1` is bound.
- Binding: `org.tspmo.gui.desktop` with `X-KDE-Shortcuts=Launch (1)` in `/usr/share/applications`
  and `/usr/share/kglobalaccel` (KDE reads shortcuts from both). `tspmo-gui --install-shortcut`
  writes the same files under `~/.local/share` and runs `kbuildsycoca6`, which makes the running
  shortcut service register it.
- Verified: both input devices advertise `KEY_PROG1` (`/proc/bus/input/devices`);
  `xkb/keycodes/evdev` and `symbols/inet` map it to `XF86Launch1`; kglobalaccel reports the
  component with key `0x10000a3` (`Qt::Key_Launch1`); and a uinput-injected `KEY_PROG1` opened the
  popup, the second one closed it.

## Plasma widget (`data/plasma/org.tspmo.power`)

Plasma 6 plasmoid replacing the power-profile applet: four modes and the GPU mode. It uses
`org.kde.plasma.workspace.dbus` (`Properties` follows `PropertiesChanged`, `SystemBus.asyncCall`
sends changes), so there is no polling. Middle click or "More" opens the popup. Install by
package, or `kpackagetool6 -t Plasma/Applet -i data/plasma/org.tspmo.power`.
