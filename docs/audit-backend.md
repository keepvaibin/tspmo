# Backend audit

Independent audit of `tspmod`, `tspmo-core`, `tspmo-sys`, `tspmo` (CLI), `data/` and
`packaging/`, at 96e8f31. Method: every line read; races reasoned against the single daemon
mutex and the watcher threads; unit tests for each fix; the installed build run through
`tools/test/e2e.sh`. Never touched: NVIDIA dGPU sysfs beyond the allowed attributes, nvidia-smi,
dGPU power (the boot and GPU switching logic is unit-tested only), reboot. User settings were
saved before and restored after every run.

Severity: H = wrong state or exploitable, M = wrong in a rare path, L = hygiene.

## Found and fixed

| # | Sev | Issue | Fix | Verified |
|---|---|---|---|---|
| 1 | H | Fn+F5 / `platform_profile` watcher read the sysfs value before taking the daemon lock: a `SetProfile` running at that moment was undone by the stale value | value read under the lock (`watch.rs`) | code; e2e mode cycling |
| 2 | H | `Hardware1.Set` name went into a sysfs path unchecked: `../` reached any attribute directory with a `current_value`, bypassing the `gpu_mux_mode` exclusion | names must be one plain path component (`tspmo_sys::fwattr::read`) | unit test `names_are_never_paths` |
| 3 | M | A GPU switch in progress dropped a policy change that arrived meanwhile (Eco+ ended, plug event): the dGPU stayed in the old mode until the next event | after a successful switch the policy is checked again; only when its target differs from what just ran (no loop) | code (GPU logic not run live) |
| 4 | M | GPU precheck scanned `/proc/*/fd` under the daemon mutex (blocks every property read for tens of ms) | precheck outside the lock, state re-checked after | code |
| 5 | M | Eco+ guard: Eco+ off then on quickly started a second guard thread while the first was still restoring, then `systemctl stop tspmobg.slice` of the first hit the second | one guard thread at a time (`LIFE` lock); `shutdown` also stops the slice | code; e2e guard checks |
| 6 | M | Stopping or crashing while Eco+ was active left the fan curve (0 % when cool), the dark Slash and the 60 Hz refresh in place | `ExecStopPost` (`tspmod --restore-limits`) now also restores fans (the mode's own settings), the Slash, the refresh rate (not at shutdown) | unit tests for the pieces; live `systemctl stop` in Eco+ (below) |
| 7 | M | Stopping or crashing while the idle timeout had the keyboard dark left it dark; a restart with no saved level then kept it at 0 | level kept in `/run/tspmo/kbd-dimmed` and in the profile; the stop hook and a restart light it again | unit test `a_stop_or_restart_while_dark_brings_the_level_back` |
| 8 | M | `HoldProfile` unbounded (count, string size): a local client could grow the daemon's memory; a client that exited during authorisation left its hold for ever | 32 holds per client, texts clipped to 256 characters, owner checked after the hold is taken | unit test `holds_are_bounded_per_client_and_clipped` |
| 9 | M | Eye care read `~/.config/kwinrc` as root (a symlink there made root read any file) | read as the user with `kreadconfig6` | code |
| 10 | M | `session::run` joined the output threads: a grandchild keeping a pipe open hung the caller, which holds the compositor lock | output arrives on a channel with a 0.5 s grace; output capped at 1 MiB | code |
| 11 | M | ICC profile written in place: a crash left an empty file that `path.exists()` then accepted for ever | write to `.tmp`, rename | code |
| 12 | L | Events subscribed by `main` (Eco+ GPU, Fn+F5) came after the watcher threads started: an early event was missed | all subscribers registered before the display and the watchers start | code |
| 13 | L | `Fans1`/`Power1` accepted `eco-plus` as a mode (stored under power-saver but compared as a different mode) | normalised to power-saver | code |
| 14 | L | `display-op` thread spawn failure panicked the daemon (`panic = "abort"`) | returns "busy; try again" | code |
| 15 | L | The touchpad was read (13 wake-ups/s while in use) even with no idle timeout | not polled unless the idle timer runs or the keyboard is dark; brightness changes re-arm the timer | idle measurement |
| 16 | L | Eco+ snapshot, guard state and `compositor.env` written in place | `tspmo_sys::write_atomic` | unit test |
| 17 | L | CLI: unknown command printed the whole usage; `tspmo <cmd> --help` unsupported for most commands; `ping x` and `doctor --help` silently ignored; `display bogus` printed usage | one short line and exit 2; per-command help; arguments checked | `help_lines` test, manual |
| 18 | L | Package: no install hooks (daemon-reload; uninstall left the service enabled and the Fn-lock keymap in `/etc/udev`); no `libkscreen`/`kconfig` optdepends | `tspmo.install` | `makepkg` |
| 19 | L | Unit hardening | `ProtectHostname`, `ProtectClock`, `RestrictNamespaces` | `systemd-analyze verify`, e2e |

## Checked, no change

- Every mutating method authorises with polkit (`polkit::check`); getters and `Doctor` are read-only. Arguments: fans (points count, ranges, firmware floor at 75 C), limits (bounds from the kernel), battery (quirk table), hardware (kernel attribute description), lighting (modes from the controller), lid (ranges), display (ranges). PGM images are bounded to 4096 x 4096.
- `panic`: no `unwrap` or `expect` on external data outside tests (one `expect` removed, see 14).
- Commands spawned: fixed program names and fixed argument vectors, no shell; user-controlled strings reach only `kscreen-doctor` mode ids read back from the compositor, and `kwriteconfig6` values built from a number.
- Lock order: daemon mutex, then the event list; subscribers only enqueue or spawn.
- `ProtectHome=no` and `ReadWritePaths=-/home -/run/user` stay: the commands the daemon runs as the user (kscreen-doctor, kwriteconfig6, tspmo-gui) need them. The daemon itself writes nothing under `/home`.

## Not fixed (needs a decision)

- `tspmo-hid-asus-dkms` downloads `hid-asus.c` over HTTPS at build time with no checksum.
- `systemd` sandboxing is not complete (`SystemCallFilter`, `CapabilityBoundingSet`): the helper commands run as the user inherit it and could not all be tested.
- `NameOwnerChanged` wakes the hold watcher for every client on the system bus (each wake is a few microseconds; a match rule per owner is not possible for unique names).
- The guard samples every 4 s in Eco+ (0.25 wake-ups/s, documented).
- Live GPU switching is untested here (needs a fresh login).
