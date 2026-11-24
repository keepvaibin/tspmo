#!/bin/sh
# DKMS PRE_BUILD: fetch hid-asus.c / hid-ids.h for the exact kernel being built, then patch.
# Any failure aborts the build: the stock hid-asus stays in use and tspmod keeps keyboard
# autosuspend off (it only enables it when the patched driver is loaded).
set -eu
kver="$1"                                  # e.g. 7.3.5-2-cachyos, 7.3.5-arch1-1
base="${kver%%-*}"                         # 7.3.5
case "$kver" in
  *-cachyos-lts*) echo "tspmo: no source mapping for $kver" >&2; exit 1 ;;
  *-cachyos*)
    rel="${kver#*-}"; rel="${rel%%-*}"     # 2
    url="https://raw.githubusercontent.com/CachyOS/linux/cachyos-$base-$rel/drivers/hid" ;;
  *)
    url="https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/plain/drivers/hid"
    q="?h=v$base" ;;
esac
q="${q:-}"
curl -fsSL --retry 2 -o hid-asus.c "$url/hid-asus.c$q"
curl -fsSL --retry 2 -o hid-ids.h "$url/hid-ids.h$q"
patch -p3 --forward --no-backup-if-mismatch hid-asus.c < 0001-HID-asus-wake-the-keyboard-for-feature-reports.patch
