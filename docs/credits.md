# Credits

tspmo is written from scratch in Rust. We studied these projects to learn how the hardware
behaves. Where we use their protocol knowledge, this file says so.

| Project | License | What we learned from it |
|---|---|---|
| [asusctl / asusd](https://github.com/OpenGamingCollective/asusctl) | MPL-2.0 | ASUS HID protocols (Aura, AniMe, Slash), and the idle-power lesson of holding hidraw open (PR #404) |
| [G-Helper](https://github.com/seerge/g-helper) | GPL-3.0 | ASUS model quirks, fan/power coupling, flicker-free dimming behaviour |
| [g-helper-linux](https://github.com/utajum/g-helper-linux) | GPL-3.0 | Mapping G-Helper features to Linux sysfs |
| [LenovoLegionLinux](https://github.com/johnfanv2/LenovoLegionLinux) | GPL-2.0 | Legion model compatibility and fan-curve semantics |
| [L5P-Keyboard-RGB](https://github.com/4JX/L5P-Keyboard-RGB) | GPL-3.0 | Lenovo 4-zone keyboard protocol |
| [zephyrus-g16-linux](https://github.com/GenuineMoron/zephyrus-g16-linux) | MIT / CC BY 4.0 | Measured GU605MI power findings (USB autosuspend, GTK renderer, NVIDIA wakes) |
