# Install

## One line
```sh
curl -fsSL https://raw.githubusercontent.com/keepvaibin/tspmo/master/install.sh | sh
```
Pass options after `sh -s --`, for example `... | sh -s -- --dry-run`.

| Option | Effect |
|---|---|
| `--dry-run` | print every step, change nothing |
| `--force` | install on a machine that is not ASUS |
| `--version vX.Y.Z` | a specific release instead of the latest |
| `--uninstall` | remove tspmo (add `--purge` to delete settings too) |

What it does:
1. Refuses if `/sys/class/dmi/id/sys_vendor` is not ASUS (unless `--force`).
2. Finds the latest GitHub Release and verifies every download against `SHA256SUMS`.
3. **Arch-based (pacman):** `pacman -U` of `tspmo` (and `tspmo-hid-asus-dkms` if `dkms` is
   installed). **Other distributions:** unpacks the tarball into `/usr` with
   `install-files.sh`. Needs systemd, D-Bus and polkit; installs the DKMS driver only if
   `dkms` exists.
4. Disables power-profiles-daemon / tuned-ppd (tspmod replaces them), enables `tspmod`.
5. Binds the ROG key for your user (`tspmo-gui --install-shortcut`).

Log out and back in once so the session picks up its environment.
Re-running is safe and upgrades in place.

## Arch, by hand
Download the `.pkg.tar.zst` files from the [release page](https://github.com/keepvaibin/tspmo/releases),
then `sudo pacman -U tspmo-*.pkg.tar.zst`. Or build the tagged source:
```sh
curl -fsSLO https://raw.githubusercontent.com/keepvaibin/tspmo/master/packaging/arch/PKGBUILD.release
curl -fsSLO https://raw.githubusercontent.com/keepvaibin/tspmo/master/packaging/arch/tspmo.install
mv PKGBUILD.release PKGBUILD && makepkg -si
```
`packaging/arch/PKGBUILD` builds the working tree as `tspmo-git`.

## Tarball
`tspmo-<version>-x86_64-linux.tar.gz` holds the binaries, `data/` and `install-files.sh`:
```sh
sudo sh install-files.sh core . / /usr
sudo systemctl daemon-reload && sudo systemctl enable --now tspmod
```
`install-files.sh` is the single list of installed files; the PKGBUILDs and `install.sh` all call it.
Each run records what it installed in `/usr/share/tspmo/files-*.list`.

## Uninstall
```sh
curl -fsSL https://raw.githubusercontent.com/keepvaibin/tspmo/master/install.sh | sh -s -- --uninstall
```
Stops and disables `tspmod` (which restores what Eco+ changed), removes the packages or the
listed files, removes the Fn-lock keymap (`/etc/udev/hwdb.d/90-tspmo-fnlock.hwdb`), `doctor`
drop-ins under `/etc`, and the ROG-key shortcut, then re-enables power-profiles-daemon if
installed. Settings in `/var/lib/tspmo` stay unless you add `--purge`.

## Why no AppImage or Flatpak
tspmo needs a root systemd service, a D-Bus system policy, polkit actions, a systemd user
environment generator, a sleep hook, udev/hwdb and DKMS; sandboxed single-user formats cannot install those.
