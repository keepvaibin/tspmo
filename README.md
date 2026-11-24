# tspmo — Thermal & System Power Management Optimizer

One fast, tiny control centre for **ASUS** (ROG, TUF, Zenbook, Vivobook, ProArt, Ally) and
**Lenovo** (Legion, LOQ, IdeaPad, Yoga, ThinkPad, ThinkBook) laptops on Linux — the G-Helper /
Legion Toolkit experience, built for the longest possible battery life.

> **Status: early development.** Today `tspmo info`, `tspmo doctor` and `tspmo dump` work.
> See the [roadmap](docs/roadmap.md).

## Goals
- **Every control:** performance modes, power limits, per-fan curves, GPU modes (Eco /
  Standard / Ultimate / Optimized), screen (refresh rate, overdrive, Mini-LED, flicker-free
  OLED dimming), keyboard lighting profiles, lid displays (AniMe Matrix, Slash), charge
  limits, and vendor keys.
- **Costs nothing while you aren't using it:**
  - The daemon is event-driven: no polling and 0 wakeups/s at idle.
  - The popup opens from the ROG / Lenovo key and exits when closed.
- **Battery first:**
  - Never wakes a sleeping dGPU.
  - Fixes the known idle-power traps.
  - `tspmo doctor` explains each fix.
- **Detects, doesn't guess:** it reads what the kernel exposes (`platform_profile`,
  `firmware-attributes`, `hwmon`, `power_supply`), and quirks live in data files.

## Try it
```sh
cargo build --release
./target/release/tspmo info      # what this laptop exposes
./target/release/tspmo doctor    # what's broken or wasting battery, with fixes
```

## Add support for your laptop
Run `tspmo dump ./my-laptop`, then `tar -C my-laptop -czf dump.tar.gz .`, and attach the
archive to an issue. Dumps contain no serial numbers, UUIDs or MAC addresses. They become test
fixtures, so your model stays working.

## Layout
| Path | What |
|---|---|
| `crates/tspmo-sys` | sysfs / hwmon / firmware-attributes helpers. Every path is relocatable through `TSPMO_SYSROOT` for testing |
| `crates/tspmo-core` | capability model and `doctor` checks |
| `crates/tspmo` | command-line tool |
| `fixtures/` | anonymised dumps of real machines |

## License
GPL-3.0-or-later. See [docs/credits.md](docs/credits.md) for the projects whose research this
builds on.
