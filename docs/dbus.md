# D-Bus interfaces

All on the system bus, service `org.tspmo.Daemon1`, object `/org/tspmo`, unless noted. Every
interface has a `GetState()` returning all its properties as `a{sv}` and emits
`PropertiesChanged` for every change, so a client calls `GetState` once and then follows signals.
Changes go through polkit; the active local user is allowed. `tspmo ping` times round trips.

## Power profiles (`org.freedesktop.UPower.PowerProfiles`, `net.hadess.PowerProfiles`)

The standard API (KDE, GNOME, `powerprofilesctl`, games), served by `tspmod`.

- `ActiveProfile`: `power-saver`, `balanced` or `performance`. While Eco+ is active it reads
  `power-saver`. Setting `power-saver` while Eco+ is active keeps Eco+; setting anything else
  leaves it. Setting `eco-plus` here is refused.
- `HoldProfile` / `ReleaseProfile` / `ActiveProfileHolds`: as upstream. A `power-saver` hold
  never lowers Eco+; a `performance` hold wins over it.

## `org.tspmo.Daemon1` (power mode, GPU)

| Member | Type | |
|---|---|---|
| `Profile` | `s` property | Mode in force: `eco-plus`, `power-saver`, `balanced`, `performance` |
| `ProfileAc`, `ProfileBattery` | `s` property | Chosen mode per power source |
| `OnBattery` | `b` property | |
| `SetProfile(s)` | method | Choose a mode for the current power source |
| `CycleProfile() -> s` | method | eco-plus, power-saver, balanced, performance, eco-plus |

The GPU members (`GpuMode`, `SetGpuMode`, ...) are as before.

Eco+ is `power-saver` plus: no turbo, package power limits (RAPL, both `intel-rapl` and
`intel-rapl-mmio`), a lower uncore clock, and 60 Hz. What it replaces is restored when it ends,
when the daemon stops, and after a daemon crash (kept in `/run/tspmo/rapl-orig` meanwhile).
See [power.md](power.md). `SetProfile` and `CycleProfile` show an on-screen note when the change
is invisible to KDE (to or from Eco+ and power-saver).

In-process event: `ProfileApplied { profile, eco_plus, on_battery }`. For Eco+, `profile` is
`power-saver` and `eco_plus` is true. `session::osd(icon, text, value)` shows a note in the
user's session.

## `org.tspmo.Display1` (the laptop's panel)

Polkit action `org.tspmo.set-display`. Properties:

| Property | Type | |
|---|---|---|
| `Connector` | `s` | Output name, e.g. `eDP-1` |
| `Oled` | `b` | From the EDID panel name (`ATN...`) or the DMI model |
| `Rates` | `au` | Refresh rates at the current resolution |
| `Refresh` | `d` | Current rate in Hz |
| `RefreshPolicy` | `s` | `max`, `60` or `auto` (60 Hz on battery, max on AC; Eco+ is always 60) |
| `FlickerFree` | `b` | Flicker-free dimming in force |
| `FlickerSetting` | `s` | `auto` (on for OLED), `on`, `off` |
| `FlickerFloor` | `u` | Lowest backlight percent that is still flicker-free |
| `Brightness` | `u` | The level, 1..100 (what KDE's slider shows) |
| `Backlight`, `Dimming` | `u` | How the level is split: panel percent, image percent |
| `ColorMode` | `s` | `native`, `srgb`, `p3` |
| `ColorModes` | `as` | The modes this panel supports |
| `EyeCare` | `u` | Warm white point in kelvin, 0 = off |
| `Message` | `s` | Last error |

Methods (each returns a short message): `SetRefreshPolicy(s)`, `SetFlickerFree(s)`,
`SetFlickerFloor(u)`, `SetBrightness(u)`, `SetColorMode(s)`, `SetEyeCare(u)`.

Flicker-free dimming follows the backlight: whatever KDE's slider or keys set is the level; the
daemon holds the backlight at the floor and darkens the picture with a `vcgt` ramp in the output's
ICC profile. Details in [display.md](display.md).
