# G-Helper parity (ASUS)

Status: done / partial / no (reason). Checked against G-Helper's feature list and the GU605MI.

| Feature | Status | Notes |
|---|---|---|
| Performance modes (Silent/Balanced/Turbo), per power source | done | `platform_profile`, Eco+ as fourth mode (package B) |
| Fan curves, per mode, 8 points, CPU/GPU/mid | done | `Fans1`; reset to firmware |
| Fan RPM display | done | `Fans1.Rpm` |
| Power limits (PL1/PL2/PL3, APU, platform, Dynamic Boost, temp target, TGP) | done | `Power1`; bounds from armoury |
| Fan/power coupling (IsFanRequired/IsPowerRequired) | done | `NeedsFanCurve` |
| Re-send mode after sleep (IsSleepReset) | done | re-applied on resume |
| Battery charge limit | done | 60-80 steps and 100; GU605 quirk |
| Charge to 100 % once | done | event-driven |
| Battery health, cycles, power | done | |
| GPU Eco / Standard / Ultimate / Optimized | done | package B; MUX change needs reboot |
| Panel overdrive | done | `Hardware1` |
| Boot sound | done | `Hardware1` |
| Mini-LED mode | done | where the model has it |
| Keyboard brightness | done | |
| Keyboard effects, colours | partial | GU605MI is single-colour: Static, Breathe, Pulse only (detected) |
| Keyboard lighting per power state, timeout | done | |
| Aura key cycles effect | done | |
| Slash lighting (modes, brightness, interval, show on boot/sleep/battery/lid) | done | |
| AniMe Matrix | partial | packets tested only; no hardware here; GA402 geometry |
| Fn-lock | partial | software hwdb layout (`Hardware1.FnLock`). Fn+F5 stays kernel-handled; Fn+F6/F9 send key combos and keep them. Fn+Esc is a plain Esc on this keyboard, so no hotkey toggle |
| M-keys, ROG key | partial | keycodes published as `Key` events; actions bound by the GUI/desktop |
| Fan key OSD | done | |
| Refresh rate auto (60 Hz on battery) | done | package B |
| Flicker-free OLED dimming | partial | package B |
| Visual modes (gamut, eye care) | partial | package B; no ASUS Splendid on Linux |
| Touchpad toggle key | no | desktop binds `XF86TouchpadToggle`; no daemon feature |
| Fn+F12 airplane (WMI 0x88) | no | event never arrives on this model |
| Camera toggle (disable webcam) | no | no kernel interface found here |
| Microphone / audio DSP (Dolby) | no | Windows driver feature |
| CPU undervolt | no | locked on Meteor Lake |
| Turbo boost off on battery | no | not in the ASUS backend; package B evaluates |
| BIOS / driver update check | no | ASUS web API; out of scope for the daemon |
| Auto-apply on app start / per-game profiles | no | would need process watching (idle cost) |
| Screenpad | no | no such hardware |
| USB-C charger / barrel detection | done | `Hardware1.charge_mode` read-only |
| Startup minimised, tray, dark theme | no | GUI package |
