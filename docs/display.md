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

How the level is tracked. KDE's brightness keys and slider write the backlight
(`/sys/class/backlight/intel_backlight/brightness`). `tspmod` sleeps in `poll(2)` on
`actual_brightness`, waits for the value to stop moving (KDE eases it in 20 ms steps), and treats
the final value as the new level. KDE keeps its own copy of the value and does not adopt the
backlight the daemon holds at the floor, so the next key press steps from the level, not from the
floor. Measured through KDE's own API (`org.kde.ScreenBrightness`): setting 10, 7, 20, 15, 35 and
10 percent gave backlight 30, 30, 30, 30, 35, 30 percent and levels 10, 7, 20, 15, 35, 10; six
`AdjustBrightnessStep` presses moved the level 15, 20, 25, 20, 15, 10. The brightness OSD is
KDE's own.

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
