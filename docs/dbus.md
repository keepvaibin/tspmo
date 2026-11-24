# D-Bus interfaces

All on the system bus, service `org.tspmo.Daemon1`, object `/org/tspmo`, unless noted. Every
interface has a `GetState()` returning all its properties as `a{sv}` and emits
`PropertiesChanged` for every change, so a client calls `GetState` once and then follows signals.
Changes go through polkit; the active local user is allowed.

## Power profiles (`org.freedesktop.UPower.PowerProfiles`, `net.hadess.PowerProfiles`)

The standard API (KDE, GNOME, `powerprofilesctl`, games), served by `tspmod`.

- `ActiveProfile`: `power-saver`, `balanced` or `performance`. While Eco+ is active it reads
  `power-saver`. Setting `power-saver` while Eco+ is active keeps Eco+; setting anything else
  leaves it.
- `HoldProfile` / `ReleaseProfile` / `ActiveProfileHolds`: as upstream. A `power-saver` hold
  never lowers Eco+.

## `org.tspmo.Daemon1`

Power mode and GPU. (The GPU members are described in the GPU section of the plan.)

| Member | Type | |
|---|---|---|
| `Profile` | `s` property | Mode in force: `eco-plus`, `power-saver`, `balanced`, `performance` |
| `ProfileAc`, `ProfileBattery` | `s` property | Chosen mode per power source |
| `OnBattery` | `b` property | |
| `SetProfile(s)` | method | Choose a mode for the current power source |
| `CycleProfile() -> s` | method | eco-plus, power-saver, balanced, performance, eco-plus |

Eco+ is `power-saver` plus: no turbo, package power limits (RAPL, both `intel-rapl` and
`intel-rapl-mmio`), optional platform and uncore limits, and 60 Hz. The limits it replaces are
restored when it ends, when the daemon stops, and after a daemon crash (they are kept in
`/run/tspmo/rapl-orig` while Eco+ is active). See [power.md](power.md).

Event: `ProfileApplied { profile, eco_plus, on_battery }`. For Eco+, `profile` is `power-saver` and
`eco_plus` is true.

## `org.tspmo.Display1`

The laptop's panel (internal output). Properties:

| Property | Type | |
|---|---|---|
| `Connector` | `s` | Output name, e.g. `eDP-1` |
| `Oled` | `b` | Detected from the EDID panel name (`ATN...`) or the DMI model |
| `Rates` | `au` | Refresh rates at the current resolution |
| `Refresh` | `d` | Current rate in Hz |
| `RefreshPolicy` | `s` | `max`, `60` or `auto` |
| `FlickerFree` | `b` | Flicker-free dimming in force |
| `FlickerSetting` | `s` | `auto` (on for OLED), `on`, `off` |
| `FlickerFloor` | `u` | Lowest panel brightness in percent that is still flicker-free |
| `Brightness` | `u` | Level, 1..100 |
| `Backlight`, `Dimming` | `u` | How the level is split (percent of panel / of image) |
| `ColorMode` | `s` | `native`, `srgb`, `p3` |
| `ColorModes` | `as` | The modes this panel supports |
| `EyeCare` | `u` | Warm white point in kelvin, 0 = off |
| `Message` | `s` | Last error |

Methods (all return a short message): `SetRefreshPolicy(s)`, `SetFlickerFree(s)`,
`SetFlickerFloor(u)`, `SetBrightness(u)`, `SetColorMode(s)`, `SetEyeCare(u)`.
Polkit action: `org.tspmo.set-display`.
