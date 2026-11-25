#!/bin/sh
# The one list of files tspmo installs. Used by the Arch PKGBUILDs, install.sh and the
# release tarball, so it exists exactly once.
#
#   install-files.sh core|dkms SRC DEST [PREFIX]
#
#   SRC     tree holding data/ (and packaging/dkms/ for dkms; LICENSE for core)
#   DEST    staging root (a pkgdir, or / for a real install)
#   PREFIX  default /usr
#   BIN_SRC env: directory holding tspmo tspmod tspmo-gui (default SRC/bin)
#   LICENSE_NAME env: directory name under share/licenses (default tspmo)
#
# Prints every installed path (relative to DEST) on stdout and records it in
# PREFIX/share/tspmo/files-<group>.list so uninstall knows exactly what to remove.
set -eu

group=${1:?usage: install-files.sh core|dkms SRC DEST [PREFIX]}
src=${2:?}
dest=${3:?}
prefix=${4:-/usr}
bin_src=${BIN_SRC:-$src/bin}
lic=${LICENSE_NAME:-tspmo}

out=$(mktemp)
trap 'rm -f "$out"' EXIT

# put MODE FROM TO   (TO relative to the prefix, or absolute when it starts with /)
put() {
  to=$3
  case $to in /*) ;; *) to=$prefix/$to ;; esac
  install -Dm"$1" "$2" "$dest$to"
  echo "$to" >>"$out"
}

core() {
  for b in tspmo tspmod tspmo-gui; do put 755 "$bin_src/$b" "bin/$b"; done
  put 755 "$src/data/bin/tspmo-run" bin/tspmo-run

  A=$src/data/applications
  put 644 "$A/org.tspmo.gui.desktop" share/applications/org.tspmo.gui.desktop
  put 644 "$A/org.tspmo.gui-toggle.desktop" share/applications/org.tspmo.gui-toggle.desktop
  put 644 "$A/org.tspmo.gui-toggle.desktop" share/kglobalaccel/org.tspmo.gui-toggle.desktop

  # Plasma 6 widget
  plasmoid=$(cd "$src/data/plasma" && find org.tspmo.power -type f)
  for f in $plasmoid; do
    put 644 "$src/data/plasma/$f" "share/plasma/plasmoids/$f"
  done

  S=$src/data/systemd
  put 644 "$S/tspmod.service" lib/systemd/system/tspmod.service
  put 644 "$S/tspmobg.slice" lib/systemd/system/tspmobg.slice
  put 644 "$S/plymouth-quit-timeout.conf" lib/systemd/system/plymouth-quit.service.d/50-tspmo.conf
  put 644 "$S/user/plasma-kwin-tspmo.conf" lib/systemd/user/plasma-kwin_wayland.service.d/50-tspmo.conf
  put 755 "$S/user-environment-generator" lib/systemd/user-environment-generators/60-tspmo
  put 755 "$S/system-sleep" lib/systemd/system-sleep/tspmo

  put 644 "$src/data/dbus/org.tspmo.Daemon1.conf" share/dbus-1/system.d/org.tspmo.Daemon1.conf
  for s in org.tspmo.Daemon1 org.freedesktop.UPower.PowerProfiles net.hadess.PowerProfiles; do
    put 644 "$src/data/dbus/$s.service" "share/dbus-1/system-services/$s.service"
  done
  put 644 "$src/data/polkit/org.tspmo.policy" share/polkit-1/actions/org.tspmo.policy
  put 644 "$src/LICENSE" "share/licenses/$lic/LICENSE"
}

dkms() {
  g=$src/packaging/dkms/hid-asus
  t=src/tspmo-hid-asus-1
  for f in dkms.conf Makefile Kbuild 0001-HID-asus-wake-the-keyboard-for-feature-reports.patch; do
    put 644 "$g/$f" "$t/$f"
  done
  put 755 "$g/fetch-source.sh" "$t/fetch-source.sh"
}

case $group in
  core | dkms) "$group" ;;
  *) echo "install-files.sh: unknown group '$group'" >&2; exit 2 ;;
esac

# Manifest last, so it lists itself.
man=$prefix/share/tspmo/files-$group.list
echo "$man" >>"$out"
install -Dm644 "$out" "$dest$man"
cat "$out"
