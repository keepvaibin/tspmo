# Power/system handoff

State at the end of the session. Branch: this worktree (master with the GUI merged in).

## Done and verified on the GU605MI (AC, KDE Wayland)

- **Eco+ redesign** (docs/power.md "Eco+ design", measured table): turbo kept,
  `balance_performance`, PL1 12 W / 10 s window, PL2 28 W, efficiency cores 1.2 GHz, background
  guard (`crates/tspmod/src/guard.rs`, `data/systemd/tspmobg.slice`), fans stopped while cool
  (`fans::eco_curve`), keyboard dark after 10 s, Slash dark, 60 Hz, dGPU forced off.
  Interactive p95 against balanced: 0.95x without background load, 0.69x with 20 busy threads.
- **dGPU wakes in Hybrid**: found. The firmware's NVPCF ACPI notification (device `NVDA0820`)
  every ~22 s, not tspmo, not an app, not `nvidia-powerd`. `tspmo doctor` names it
  (`nvidia-nvpcf`), apps (`nvidia-held`), and a desktop holding the driver (`compositor-nvidia`).
- **Idle cost**: tspmod woke 8 times a second because upstream `blocking` pool threads never
  exit. Patched copy in `vendor/blocking` (`[patch.crates-io]`): 0.1/s now (events), 0.35/s in
  Eco+ (the guard's 4 s sample).
- **API for the GUI**: `Daemon1.Doctor() -> a(sss)`, `Fans1.Temps -> a(us)`.
- **Fn+F5 with Fn-lock**: the daemon cycles the mode when the kernel did not (`FanKey`).
- `tools/test/e2e.sh` covers modes, KDE API, guard, fans, power limits, display, Slash,
  lighting, battery, latency, idle. `tools/test/gpu-live.sh` is the live GPU switching test.
- docs/review-power.md: every feature, how verified, result.

## Not verified here

- **Live Eco/Hybrid switching on this build.** The running KWin (started 09:49, before the
  compositor setting existed) holds `/dev/nvidia*`, so tspmod refuses to unload the driver
  ("Log out and in once") and Eco+ cannot turn the GPU off in this session. After the next login
  run `sudo tools/test/gpu-live.sh`: it does Hybrid -> Eco -> Hybrid, Eco+ forcing Eco and
  restoring, 2 minutes idle, kernel log check.
- Battery column of `tspmo bench` (AC all session): see docs/power.md for how psys relates to
  battery draw.

## Resume

```
cd <worktree>
CARGO_BUILD_JOBS=4 nice -n 19 cargo fmt --all && CARGO_BUILD_JOBS=4 nice -n 19 cargo clippy --workspace --all-targets -- -D warnings && CARGO_BUILD_JOBS=4 nice -n 19 cargo test --workspace
cd packaging/arch && CARGO_BUILD_JOBS=4 nice -n 19 makepkg -f
sudo pacman -U --noconfirm tspmo-git-0.1.0-1-x86_64.pkg.tar.zst && sudo systemctl daemon-reload && sudo systemctl restart tspmod
tools/test/e2e.sh --idle 60            # --quick skips the mode cycling
sudo tools/test/gpu-live.sh            # after a fresh login
```

Before any test note `tspmo status` and put the mode back afterwards. `sudo tspmo bench ...`
restores what it changes through a guard process (if killed, `/run/tspmo-bench-<pid>` lists what
is left). Never `pkill -f "bench --burn"` from a shell whose own command line contains it.

State to leave: balanced on AC and battery, refresh policy max, GPU policy hybrid, charge limit
100, fans auto, RAPL 90/104 W, efficiency cores uncapped, no `tspmobg.slice`.
