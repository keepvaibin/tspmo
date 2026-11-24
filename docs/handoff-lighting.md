# Lighting handoff (keyboard Aura, Slash)

Scope: `tspmo-core/src/asus/{lighting,slash,hid,anime}.rs`, `tspmod/src/asus/{lighting,lid}.rs`,
the `light` and `lid` CLI, their sections of `docs/dbus.md`, and the GUI's lighting/lid bindings.

## What this hardware is (GU605MI)

Read from the machine, not assumed:

- Keyboard: USB `0b05:19b6`, `hid-asus` (the patched DKMS build), hidraw0. Its report
  descriptor has report `0x5d` (62 data bytes: Aura commands, input, output and feature) and
  `0x5a` (the driver's brightness/init reports). `/sys/class/leds/asus::kbd_backlight` is
  **asus-wmi's** LED (kernel 7: it writes the ACPI backlight devstate and forwards the level to
  `hid-asus` as `5a ba c5 c4 <level>`), max 3.
- Slash: USB `0b05:193b`, hidraw1, report `0x5e` (127 bytes, output and feature). `0b05:193b`
  is not the keyboard: it is the Slash's own ITE device.
- The Aura controller answers a capability query (`5d` init string, then `5d 05 20 31 00 20`
  and a feature-report read, as G-Helper's `AuraProbe` does):
  `type 04 (single zone), year 0x23, layout 01, feat1 00 (no logo, no lightbar), feat2 0x46,
  family 04 (Zephyrus)`. feat2 bit `0x10` ("one zone red effect", a white-only keyboard) is
  **not** set. So this is a **single-zone RGB** backlight, not a white-only one: G-Helper offers
  static, breathe, colour cycle and strobe for it; asusctl's data for `GU605M` lists static,
  breathe, rainbow cycle, rainbow wave, pulse. We offer the intersection that makes sense on
  one zone: `static breathe rainbow-cycle pulse` (a wave on one zone is a cycle).
- asusctl's `aura_support.ron` has `GU605M` with `power_zones: [Keyboard]`, so the power-state
  mask is the keyboard bits only (boot 1, awake 3, sleep 5, shutdown 7 of byte 0).

## What was wrong

1. **The API lied.** `Effect` was `("", 0,0,0,...)` until something set it, `Profiles` carried
   `-1` and empty modes for "untouched", and clients that sent those back got
   `unknown ... ''`. Now nothing is empty: `Effect` starts as the factory's static white,
   `Profiles` lists what applies, `SetEffect` takes an empty speed/direction as "keep".
2. **Effects for a board that does not have them.** The mode list was a fixed five. It now comes
   from the controller's answer (`Kind`, `Modes`, `ModeParams`).
3. **The LED and the controller could diverge.** `Backlight::set` skipped the write when the
   LED already read the requested level, but that value is the kernel's idea (asus-wmi caches
   it; reading it does not ask the keyboard). A keyboard that missed the `5a ba c5 c4` report
   (asleep, reset, an earlier raw write) stayed dark at "3/3" and could never be fixed by
   asking for 3 again. The write is now unconditional, and start, resume and a power-source
   change send the whole state in one open: wake-up string, power states, effect, then the LED.
4. Power-state packet: now `5d bd 01 <mask> 00 00 00 ff` like G-Helper (asusctl sends zeros in
   the last byte). Effect packet: byte 9 is `01` for breathe and `ff` for a black colour, as in
   G-Helper (black is refused by the daemon; use brightness 0).
5. **Slash**: writes were fine (checked by reading records back), but the daemon wrote whole
   canned records over the firmware's, the interval range was 0-255 (the firmware's apps use
   0-5 s) and `SlashEnabled` said "on" while the Slash was dark on battery. Now animation and
   flags are read-modify-write, `SlashLit` says whether it is lighting, `SlashIntervalMax` and
   `SlashBrightnessLevels` give the real values, and the first start adopts the firmware's
   settings.
6. **Sleep flag polarity.** asusctl inverts the sleep flag; G-Helper reads/writes it as 1 = on,
   and the other four records (`a0 a2 a4`, `a3`, `a5`) read 1 on this unit. We follow G-Helper
   (1 = show on sleep). Not observable without suspending; flip `When::Sleep` in
   `slash::show_when` and `Session::set_flag` handling if the sleep animation turns out
   inverted.

## How to look at the hardware (all read only unless noted)

- Aura capability answer: write `5d` + `ASUS Tech.Inc.` + `00`, write `5d 05 20 31 00 20`,
  `HIDIOCGFEATURE(63)` with byte 0 = `5d` on `/dev/hidraw0` (as root).
- Slash records: `HIDIOCSFEATURE(128)` with `5e d2 02 01 08 <region>`, then
  `HIDIOCGFEATURE(128)`: regions `ab` (display options: byte 8 on/off, 10 brightness, 12
  interval), `a0` boot, `a1` sleep, `a2` low battery, `a3` battery indicator, `a4` shutdown,
  `a5`, `a8`. Mode record: `5e d2 03 00 0c`, byte 6 = animation id.
- The daemon logs `tspmod: slash: wrote ... but the firmware reports ...` if the firmware
  disagrees after an apply.

## State of the work

See the commit log. Open items are under "Ask the user".

## Ask the user

(none pending)
