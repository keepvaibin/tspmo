#!/bin/sh
# Remove tspmo: stop the daemon, delete everything it installed or wrote outside /var/lib, and
# put power-profiles-daemon back. Options: --dry-run  --purge (also delete /var/lib/tspmo)
set -eu

DRY=0 PURGE=0
for a in "$@"; do
  case $a in
    --dry-run) DRY=1 ;;
    --purge) PURGE=1 ;;
    *) echo "uninstall.sh: unknown option '$a'" >&2; exit 2 ;;
  esac
done

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
have() { command -v "$1" >/dev/null 2>&1; }
run() { if [ "$DRY" = 1 ]; then printf '+ %s\n' "$*"; else "$@"; fi; }

if [ "$(id -u)" = 0 ]; then SUDO=; elif have sudo || [ "$DRY" = 1 ]; then SUDO=sudo
else echo "error: needs root" >&2; exit 1; fi
as_root() { run $SUDO "$@"; }

# 1. Stop the daemon. Stopping runs ExecStopPost, which hands back what Eco+ changed.
as_root systemctl disable --now tspmod.service 2>/dev/null || true

# 2. Remove the files.
owned_by_pacman=0
if have pacman; then
  pk=
  for p in tspmo tspmo-git; do pacman -Q "$p" >/dev/null 2>&1 && pk="$pk $p"; done
  pacman -Q tspmo-hid-asus-dkms >/dev/null 2>&1 && pk="$pk tspmo-hid-asus-dkms"
  if [ -n "$pk" ]; then
    owned_by_pacman=1
    # shellcheck disable=SC2086
    as_root pacman -R $pk
  fi
fi
if [ "$owned_by_pacman" = 0 ]; then
  if have dkms && dkms status 2>/dev/null | grep -q tspmo-hid-asus; then
    as_root dkms remove tspmo-hid-asus/1 --all || true
  fi
  for l in /usr/share/tspmo/files-core.list /usr/share/tspmo/files-dkms.list; do
    [ -f "$l" ] || continue
    while read -r f; do
      [ -n "$f" ] && as_root rm -f "$f"
    done <"$l"
  done
  as_root rm -rf /usr/share/plasma/plasmoids/org.tspmo.power /usr/share/licenses/tspmo \
    /usr/src/tspmo-hid-asus-1 /usr/share/tspmo
  for d in /usr/lib/systemd/system/plymouth-quit.service.d \
    /usr/lib/systemd/user/plasma-kwin_wayland.service.d; do
    as_root rmdir "$d" 2>/dev/null || true
  done
  # Put back the power-profiles D-Bus files that install.sh moved aside.
  for f in /usr/share/dbus-1/system-services/net.hadess.PowerProfiles.service \
    /usr/share/dbus-1/system-services/org.freedesktop.UPower.PowerProfiles.service; do
    [ -f "$f.tspmo-orig" ] && as_root mv -f "$f.tspmo-orig" "$f"
  done
fi

# 3. Everything tspmod wrote under /etc: the Fn-lock keymap (hwdb), then the hwdb binary.
if [ -e /etc/udev/hwdb.d/90-tspmo-fnlock.hwdb ]; then
  as_root rm -f /etc/udev/hwdb.d/90-tspmo-fnlock.hwdb
  if ls /etc/udev/hwdb.d/*.hwdb >/dev/null 2>&1; then
    as_root systemd-hwdb update || true
  else
    as_root rm -f /etc/udev/hwdb.bin
  fi
  as_root udevadm trigger --action=change --subsystem-match=input || true
fi
# Drop-ins and environment files that `tspmo doctor` suggests creating by hand.
as_root rm -f /etc/environment.d/50-tspmo-gtk.conf /etc/modprobe.d/tspmo-nvidia-pm.conf \
  /etc/systemd/system/plymouth-quit.service.d/50-tspmo.conf
as_root rmdir /etc/systemd/system/plymouth-quit.service.d 2>/dev/null || true
for f in /etc/udev/rules.d/*tspmo*; do
  [ -e "$f" ] && as_root rm -f "$f"
done

# 4. State.
if [ "$PURGE" = 1 ]; then
  as_root rm -rf /var/lib/tspmo
else
  say "settings stay in /var/lib/tspmo (rerun with --purge, or rm -r it)"
fi

# 5. The ROG-key shortcut, for the user running this.
if [ "$(id -u)" != 0 ] || [ -n "${SUDO_USER:-}" ]; then
  if [ "$(id -u)" = 0 ]; then
    home=$(getent passwd "$SUDO_USER" | cut -d: -f6)
  else
    home=$HOME
  fi
  base=${XDG_DATA_HOME:-$home/.local/share}
  [ "$(id -u)" = 0 ] && base=$home/.local/share
  for d in applications kglobalaccel; do
    run rm -f "$base/$d/org.tspmo.gui.desktop" "$base/$d/org.tspmo.gui-toggle.desktop"
  done
fi

# 6. Hand the machine back to power-profiles-daemon (or tuned-ppd) if present.
as_root systemctl daemon-reload || true
as_root systemctl reload dbus.service || true
for u in power-profiles-daemon tuned-ppd; do
  if systemctl list-unit-files "$u.service" 2>/dev/null | grep -q "^$u"; then
    as_root systemctl enable --now "$u.service" && say "re-enabled $u" || true
    break
  fi
done

say "tspmo removed. Log out and back in to drop the session environment."
