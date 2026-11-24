# D-Bus interfaces

All interfaces live on the **system bus**, service `org.tspmo.Daemon1`, object `/org/tspmo`.
Each exposes its full state as properties, with `PropertiesChanged` on change: call
`org.freedesktop.DBus.Properties.GetAll` once, then follow the signal. Changes are methods,
checked with polkit (actions in `data/polkit/org.tspmo.policy`; root always passes).
An interface is present only if the machine has the hardware. Errors are
`org.freedesktop.DBus.Error.InvalidArgs` with a one-line message.

Property reads never touch firmware, except `Fans1.Rpm`.

<!-- Package B documents Daemon1 (profiles, GPU) and Display1 here. -->

## ASUS hardware

Modes are `power-saver`, `balanced`, `performance`. Power sources are `ac`, `battery`.
Types use D-Bus signatures: `(yyy)` is a colour (r, g, b).

### `org.tspmo.Fans1` (polkit `org.tspmo.set-fans`)

Present when the `asus_custom_fan_curve` hwmon device exists. Curves are 8 points of
`(°C, percent)`.

| Property | Type | |
|---|---|---|
| `Fans` | `a(us)` | `(index, label)`: CPU, GPU, Mid |
| `Mode` | `s` | thermal mode in force |
| `Settings` | `a(susa(yy))` | `(mode, fan, kind, points)` for every mode and fan. `kind`: `auto`, `custom`, `full`. `auto` lists the firmware curve once known |
| `Factory` | `a(sua(yy))` | `(mode, fan, points)`: the firmware's curves, read from the EC |
| `Rpm` | `a(uu)` | `(fan, rpm)`, read on each query, no signal |

| Method | |
|---|---|
| `SetCurve(mode s, fan u, points a(yy))` | `fan` 0 is all fans |
| `SetAuto(mode s, fan u)` | firmware curve |
| `SetFull(mode s, fan u)` | 100 % |
| `Reset(mode s, fan u)` | firmware default, curve re-read |

Rules:
- Temperatures 20-100 °C, non-decreasing; speeds 0-100 %, non-decreasing.
- From 75 °C a fan may not run slower than the firmware's curve at that temperature (50 % at
  85 °C and 30 % from 75 °C until the firmware curve of that mode has been read).
- The EC drops custom curves on every thermal-mode change and on resume: tspmod writes them again.
- Stored in `/var/lib/tspmo/fans`.

### `org.tspmo.Power1` (polkit `org.tspmo.set-power`)

Present when `asus-armoury` exposes power limits (`ppt_pl1_spl`, `ppt_pl2_sppt`,
`ppt_pl3_fppt`, `ppt_apu_sppt`, `ppt_platform_sppt`, `nv_dynamic_boost`, `nv_temp_target`,
`nv_tgp`: whichever exist).

| Property | Type | |
|---|---|---|
| `Limits` | `a(sssxxxx)` | `(name, label, unit, min, max, default, current)`. Bounds are the kernel's for the power source in use; `min == max` means fixed. `current` is the last value written |
| `Custom` | `a(sssx)` | `(mode, source, name, value)`; absent means firmware default |
| `Mode`, `Source` | `s` | |
| `NeedsFanCurve` | `b` | this model refuses limit writes without an active custom fan curve; tspmod then runs the firmware's own curve as a custom one |

| Method | |
|---|---|
| `SetLimit(mode s, source s, name s, value x)` | negative value: firmware default |
| `Reset(mode s)` | firmware defaults for the mode |

Limits are written after each mode change, power-source change and resume. Going back to
the default reloads the thermal mode (the only way to restore the firmware's values).
Stored in `/var/lib/tspmo/power`.

### `org.tspmo.Battery1` (polkit `org.tspmo.set-battery`)

| Property | Type | |
|---|---|---|
| `Limit` | `y` | limit in force, percent (0: none) |
| `UserLimit` | `y` | the limit the user chose (0: never set) |
| `Steps` | `ay` | limits the firmware honours (GU605: 60 65 70 75 80 100) |
| `ChargeOnce` | `b` | charging to 100 % once |
| `Status` | `s` | |
| `Percent` | `y` | |
| `Health` | `d` | full / design capacity, percent (-1 unknown) |
| `Cycles` | `u` | |
| `Power` | `d` | watts in or out (-1 unknown) |
| `TimeLeft` | `x` | seconds (-1 unknown) |

| Method | |
|---|---|
| `SetLimit(percent y)` | |
| `SetChargeOnce(enable b)` | back to the limit after unplugging or when full |

Updated when UPower reports a state or percentage change. The limit is left alone until the
user sets one. Stored in `/var/lib/tspmo/battery`.

### `org.tspmo.Hardware1` (polkit `org.tspmo.set-hardware`)

Firmware toggles from `asus-armoury` (everything except power limits and GPU modes), driven by
each attribute's own description.

| Property | Type | |
|---|---|---|
| `Items` | `a(ssssasxxb)` | `(name, label, kind, current, values, min, max, writable)`. Kind: `toggle` (0/1), `choice` (one of `values`), `number`, `text` |
| `RebootPending` | `b` | |
| `FnLock` | `b` | software Fn-lock: F1-F11 are the media keys, Fn+key gives the F-key |
| `FnLockAvailable` | `b` | a measured ASUS USB keyboard is present (GU605: 0b05:19b6) |

| Method | |
|---|---|
| `Set(name s, value s)` | validated against the attribute's description |
| `SetFnLock(on b)` | writes `/etc/udev/hwdb.d/90-tspmo-fnlock.hwdb`, `systemd-hwdb update`, `udevadm trigger` on the keyboard only; about 1 s. Off restores the native keys, then removes the files |

`charge_mode` is read-only: 0 none, 1 barrel, 2 USB-C charger. Choices are written again at
start and after resume. Stored in `/var/lib/tspmo/hardware`.

### `org.tspmo.Lighting1` (polkit `org.tspmo.set-lighting`)

Present with a keyboard backlight (`asus::kbd_backlight`). Effects need the Aura USB
controller.

| Property | Type | |
|---|---|---|
| `MaxBrightness`, `Brightness` | `y` | |
| `Modes` | `as` | effects this keyboard has (empty: brightness only) |
| `Effect` | `(s(yyy)(yyy)ss)` | `(mode, colour, colour2, speed, direction)` showing |
| `Source` | `s` | |
| `Profiles` | `a(si(s(yyy)(yyy)ss))` | per source: brightness (-1: untouched) and effect (empty mode: untouched) |
| `PowerStates` | `(bbbb)` | lit in boot, awake, sleep, shutdown |
| `IdleTimeout` | `u` | seconds without input before the backlight goes dark; 0 never |

| Method | |
|---|---|
| `SetBrightness(source s, level y)` | `source` empty: the one in use |
| `SetEffect(source s, effect (s(yyy)(yyy)ss))` | speed `low` `med` `high`; direction `right` `left` `up` `down` |
| `SetPowerStates(boot b, awake b, sleep b, shutdown b)` | |
| `SetIdleTimeout(seconds u)` | |
| `CycleEffect(forward b) → s` | what the Aura key does |

Brightness set with the keyboard's own keys becomes the level of the current source. The
Aura controller is opened per write, never held. Stored in `/var/lib/tspmo/lighting`.

### `org.tspmo.Lid1` (polkit `org.tspmo.set-lid`)

Present when the model has a Slash light bar or an AniMe Matrix. `Devices` lists which
(`slash`, `anime`); properties of an absent device are inert.

| Property | Type | |
|---|---|---|
| `Devices` | `as` | `slash` |
| `SlashEnabled` | `b` | |
| `SlashBrightness` | `y` | 0-255 |
| `SlashInterval` | `y` | pause between animations, 0-255 |
| `SlashMode`, `SlashModes` | `s`, `as` | |
| `SlashShow` | `a(sb)` | `boot`, `sleep`, `shutdown`, `battery`, `lid`, `warning` |
| `AnimeEnabled` | `b` | |
| `AnimeBrightness` | `y` | 0-3 |
| `AnimeBuiltin` | `b` | the firmware's own animations |
| `AnimeBuiltinChoice` | `(yyyy)` | animation (0 or 1) for boot, awake, sleep, shutdown |

| Method | |
|---|---|
| `SetSlashEnabled(on b)`, `SetSlashBrightness(level y)`, `SetSlashInterval(n y)` | |
| `SetSlashMode(mode s)` | |
| `SetSlashShow(what s, on b)` | `battery` defaults to off |
| `SetAnimeEnabled(on b)`, `SetAnimeBrightness(level y)`, `SetAnimeBuiltin(on b)`, `SetAnimeBuiltinChoice(boot y, awake y, sleep y, shutdown y)` | |
| `ShowAnimeText(text s)` | 5x7 font, upper case, about 5 characters |
| `ShowAnimeImage(pgm ay)` | PGM (P2 or P5), scaled to the display |

The AniMe Matrix is tested against packet layouts only (no such hardware here), and only
the GA402 family's dot geometry is implemented; other models accept the settings but not images.

Stored in `/var/lib/tspmo/lid`.

## Hotkeys

tspmod reads the ASUS input devices (`Asus WMI hotkeys` and ASUS USB keyboards, read only,
never grabbed) and publishes each key press as an in-process `events::Key(code)` with the raw
Linux key code. Mapping (kernel 7.3 `asus-nb-wmi` keymap and `hid-asus`):

| Key | USB keyboard (`hid-asus`) | `Asus WMI hotkeys` |
|---|---|---|
| ROG / Armoury | `KEY_PROG1` | `KEY_PROG3` |
| Aura next | `KEY_PROG3` | `KEY_PROG4` |
| Aura previous (Fn+Left) | `KEY_PROG2` | |
| Fan (Fn+F5) | `KEY_PROG4` | `KEY_FN_F5` |

The Aura keys cycle the effect (brightness on keyboards without effects) and show an OSD.
Fn+F5 is cycled by the kernel (it changes `platform_profile`); the OSD follows the thermal
mode change. The ROG key is bound by the GUI package.
