# Display

`tspmod` controls the laptop's panel through KDE: it runs `kscreen-doctor` (and `busctl` for the
OSD) as the logged-in user, one short command per change, with that user's environment taken from
logind. Nothing stays resident and nothing polls: the daemon sleeps until a power event, a
backlight change or a D-Bus call. Other desktops are not covered yet (the OSD falls back to
`tspmo-gui --osd`).

CLI: `tspmo display [refresh|brightness|dimming|floor|color|eyecare]`.

## Refresh rate

Modes at the current resolution only; scale and resolution are never touched. Policy:

- `max`: the highest rate.
- `60`: the mode nearest 60 Hz.
- `auto` (default): 60 Hz on battery, the highest rate on AC.
- Eco+ is always 60 Hz, whatever the policy.

Applied on every power event, resume and login, and does nothing if the panel is already there.

## Flicker-free dimming

OLED panels dim with pulse-width modulation at low backlight levels. Above some level they dim by
current instead, which does not flicker. G-Helper (Windows) handles this by leaving the panel
brightness alone and dimming in ASUS's Splendid layer between 40 and 100%. tspmo does the same
split, but only below a floor:

- level at or above the floor: the backlight is the level, nothing else changes;
- level below the floor: the backlight is held at the floor and the picture is darkened to
  `level / floor`.

The floor is `FlickerFloor`, 30% by default. Samsung publishes no number for the ATNA60DL01 and
there is no way to measure flicker without a scope, so **30% is an estimate; set it from your own
eyes or a phone camera in slow motion** (`tspmo display floor <percent>`). It is on by default for
OLED panels (EDID name `ATN...`/`AMS...`, or an `OLED` DMI model) and off for LCDs (`auto`).

How the darkening works. KWin 6.8 has an output `brightness` and a `dimming` factor, but with a
backlight device both only move the backlight (`brightness x dimming`), so they cannot dim in
software. What KWin does apply in software is the `vcgt` ramp of an output's ICC profile. So
`tspmod` writes a small matrix/TRC profile (`/var/lib/tspmo/icc/<mode>-<percent>.icc`: the panel's
primaries plus a ramp scaled by the dimming) and loads it with
`kscreen-doctor output.<name>.iccprofile.<path> output.<name>.colorProfileSource.ICC`. When nothing
is dimmed and the colour mode is native the compositor goes back to its built-in sRGB.
Measured on the GU605MI: with the 40% profile loaded, platform power was about 1.9 W lower in
both of two runs (60 Hz, AC; 22.0 to 20.0 W and 20.3 to 18.5 W; a busy desktop), so the darkening
reaches the panel. Screenshots do not show it (they are taken before the colour transform).

How the level is tracked. KDE's brightness keys and slider (`org.kde.ScreenBrightness`) set a
target and then ease the backlight towards it in steps about 24 ms apart, writing
`/sys/class/backlight/intel_backlight/brightness` through its helper. `tspmod` has two threads
for this, because the two jobs have different deadlines:

- `backlight` sleeps in `poll(2)` on `actual_brightness`. For every value that is not its own
  write and is below the floor, it writes the floor back at once (a plain sysfs write, about
  1 ms after KDE's) and hands the newest value on.
- `backlight-apply` asks KDE for its target (`Brightness` of the internal display, one `busctl`
  call), loads the dimming for it (about 50 ms) and writes the backlight. A profile already in
  force is not loaded again. When KDE's steps stop, it checks KDE's value once more: KDE's last
  write can land exactly on the floor, where the backlight does not change and nothing is seen.

Brightness is `backlight x dimming`, and each step only ever lowers it on the way down.

### The pulse (fixed)

Symptom: at low brightness each key press made the screen go darker and then settle at the
intended level. Measured with `tools/test/dim-trace.py` (samples the backlight about every
millisecond while KDE moves from 40 to 25 percent; floor 30 %):

```
before:   40.0  38.5  30.9  25.4 ........... (261 ms at 25 %, below the floor) ......  30.1
after:    40.0  38.5  33.8  30.1 (29.3 for 1.4 ms)  30.1  (25.2 for 1.2 ms)  30.1
```

Cause. The daemon waited for KDE's easing to stop (120 ms) and then loaded the profile (50 to
100 ms) *before* putting the backlight back at the floor. For about 260 ms the backlight was at
KDE's value, below the floor, with the old dimming still in force: darker than the level asked
for (and in the flickering range), then the profile and the floor arrived and the screen came
back up to the level.

Also found with a backlight sampler and the kernel's function tracer on
`backlight_device_set_brightness` (writers: KDE's `backlighthelper` and `tspmod` only):

- `colorProfileSource.ICC` was sent with every profile. KWin re-applies the output then and
  writes the backlight to 100 % for about 1 ms, 150 to 250 ms later: a flash on every step. It
  is now sent once; later loads send only the profile path (verified: no write at all, and
  `kscreen-doctor -o` shows the new profile). It is sent again after login, resume and refresh
  changes (a reset output).
- KDE's own easing writes transient values (down to 0 % and up to 100 % were seen) for one
  sample each, with or without tspmo. Those are pinned within about 1 ms now.
- KDE shapes its next steps by what it reads back from the backlight, so the steps seen while
  tspmod pins the floor are not the user's level (a target of 25 produced steps down to 7.6 %).
  The level is read from KDE's `Brightness` property instead, which holds the target from the
  first moment.
- The state lock was held while a new profile was built (about 10 ms): the thread that pins
  never waits for it (`try_lock`, last known floor).

Result (six steps, 40 to 25, 25 to 15, 15 to 8, 8 to 22, 22 to 30, 30 to 45): the backlight is
below the floor for 1 to 1.5 ms per KDE step (4 to 5 steps), the final level is exact every time,
the dimming goes straight to the target in one load, nothing changes above the floor. A step that
KDE makes exactly onto the floor from below (30 from 25) is caught by the check of KDE's value.

`TSPMOD_TRACE_DIM=1` in the daemon's environment logs every backlight write and profile load with a
timestamp (`journalctl -u tspmod -o short-precise`).

The level is remembered in `/var/lib/tspmo/display`; after a restart, a backlight still where
the daemon left it keeps the remembered level, any other value (KDE restoring its own at login)
wins.

Limits:

- The daemon cannot change what KDE's slider shows; it always shows the level, which is right.
- If something other than KDE rewrites the backlight, it becomes the new level.
- The profile change goes through the compositor, about 100 ms per step.
- `dimming off` restores the plain backlight and the built-in sRGB.

## Colour modes

| Mode | What it is |
|---|---|
| `native` | KWin's default: sRGB content is sent to the panel as is, so it looks saturated on a wide-gamut OLED (this is also "Vivid") |
| `srgb` | Colour-accurate sRGB: an ICC profile built from the panel's EDID primaries |
| `p3` | A Display-P3 profile (D65, P3 primaries); close to `srgb` on a panel that is nearly P3 |

The GU605MI's EDID red is (0.680, 0.320), the DCI-P3 red, so the panel is close to P3 and `native`
is the saturated look. Cinema and other game-style modes of G-Helper are Splendid presets with no
KWin counterpart and are not offered. Modes needing primaries are hidden when the EDID has none.

Profiles carry the D50 colorants, a `chad` tag (so KWin finds the true white point) and the TRC of
a 2.2 gamma, the curve KWin assumes for sRGB.

## Eye care

`tspmo display eyecare 4500` turns on KWin Night Light in constant mode at that temperature
(1000 to 6500 K) by setting `Active`, `Mode` and `NightTemperature` in `kwinrc` with
`kwriteconfig6`, which also tells the running KWin. `eyecare off` puts back what the user had
before (keys that were unset are deleted). Night Light is independent of the dimming profile.

## OLED care

Pixel shifting needs the compositor and is not available on Wayland. Static content is best
handled by a short screen-off timeout (System Settings, Power Management, "Turn off screen") and
the dimming above; tspmo does not invent more. The panel's own care features (pixel refresh,
logo detection) are not exposed to the OS.
