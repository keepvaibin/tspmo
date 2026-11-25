#!/bin/sh
# tspmo installer.
#   curl -fsSL https://raw.githubusercontent.com/keepvaibin/tspmo/master/install.sh | sh
# Options (after `sh -s --` when piped): --dry-run  --force  --uninstall  --version vX.Y.Z
set -eu

REPO=keepvaibin/tspmo
RAW=https://raw.githubusercontent.com/$REPO/master
GH=https://github.com/$REPO

DRY=0 FORCE=0 UNINSTALL=0 TAG=${TSPMO_VERSION:-}
while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) DRY=1 ;;
    --force) FORCE=1 ;;
    --uninstall) UNINSTALL=1 ;;
    --version) shift; TAG=${1:?--version needs a tag} ;;
    -h | --help) sed -n '2,4p' "$0" 2>/dev/null || true; exit 0 ;;
    *) echo "install.sh: unknown option '$1'" >&2; exit 2 ;;
  esac
  shift
done

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# run CMD...: execute, or only print under --dry-run.
run() {
  if [ "$DRY" = 1 ]; then printf '+ %s\n' "$*"; else "$@"; fi
}

if [ "$(id -u)" = 0 ]; then
  SUDO=
elif have sudo; then
  SUDO=sudo
elif [ "$DRY" = 1 ]; then
  SUDO=sudo
else
  die "needs root: run as root or install sudo"
fi
# as_root CMD...
as_root() { run $SUDO "$@"; }

if [ "$UNINSTALL" = 1 ]; then
  args=
  [ "$DRY" = 1 ] && args="--dry-run"
  here=$(dirname -- "$0" 2>/dev/null || echo .)
  if [ -f "$here/uninstall.sh" ]; then
    # shellcheck disable=SC2086
    exec sh "$here/uninstall.sh" $args
  fi
  have curl || die "curl is required"
  # shellcheck disable=SC2086
  curl -fsSL "$RAW/uninstall.sh" | sh -s -- $args
  exit $?
fi

# ---- checks ------------------------------------------------------------------------------
[ "$(uname -s)" = Linux ] || die "tspmo runs on Linux only"
[ "$(uname -m)" = x86_64 ] || die "only x86_64 builds are published (build from source: see README)"
have curl || die "curl is required"
have sha256sum || die "sha256sum is required"

vendor=$(cat /sys/class/dmi/id/sys_vendor 2>/dev/null || echo unknown)
case $vendor in
  *ASUS*) ;;
  *)
    if [ "$FORCE" = 1 ]; then
      warn "this is not an ASUS laptop ($vendor); continuing because of --force"
    else
      die "this is not an ASUS laptop ($vendor). tspmo targets ASUS; use --force to install anyway"
    fi
    ;;
esac

ARCH_BASED=0
have pacman && ARCH_BASED=1

if [ "$ARCH_BASED" = 0 ]; then
  have systemctl || die "systemd is required"
  [ -d /run/systemd/system ] || [ "$DRY" = 1 ] || die "systemd is not the running init"
  have busctl || have dbus-daemon || have dbus-broker || die "D-Bus is required"
  have pkexec || [ -d /usr/share/polkit-1 ] || die "polkit is required"
fi

# ---- version -----------------------------------------------------------------------------
if [ -z "$TAG" ]; then
  TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1) || TAG=
  if [ -z "$TAG" ]; then
    [ "$DRY" = 1 ] || die "could not find the latest release of $REPO"
    TAG=v0.9.0
    warn "no release found; --dry-run assumes $TAG"
  fi
fi
VER=${TAG#v}
BASE=$GH/releases/download/$TAG
say "tspmo $VER ($([ "$ARCH_BASED" = 1 ] && echo 'Arch package' || echo 'tarball'))"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# fetch FILE...: download into $TMP, verify against SHA256SUMS.
fetch() {
  if [ "$DRY" = 1 ]; then
    for f in "$@"; do say "+ download $BASE/$f"; done
    say "+ verify SHA256SUMS"
    return 0
  fi
  curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" || die "no SHA256SUMS for $TAG"
  for f in "$@"; do
    curl -fSL --progress-bar -o "$TMP/$f" "$BASE/$f" || die "download failed: $f"
    grep " \**$f\$" "$TMP/SHA256SUMS" >"$TMP/one.sum" || die "$f is not listed in SHA256SUMS"
    (cd "$TMP" && sha256sum -c one.sum >/dev/null) || die "checksum mismatch: $f"
  done
}

ppd_note() {
  for u in power-profiles-daemon tuned-ppd; do
    if systemctl list-unit-files "$u.service" 2>/dev/null | grep -q "^$u"; then
      warn "$u is installed: it will be disabled, tspmod replaces it (uninstall re-enables it)"
    fi
  done
}

# ---- install -----------------------------------------------------------------------------
if [ "$ARCH_BASED" = 1 ]; then
  main="tspmo-$VER-1-x86_64.pkg.tar.zst"
  dk="tspmo-hid-asus-dkms-$VER-1-any.pkg.tar.zst"
  pkgs="$main"
  if pacman -Q dkms >/dev/null 2>&1; then
    pkgs="$main $dk"
  else
    say "dkms not installed: skipping tspmo-hid-asus-dkms (keyboard autosuspend, ~0.6 W)"
  fi
  # shellcheck disable=SC2086
  fetch $pkgs
  ppd_note
  # shellcheck disable=SC2086
  set -- $pkgs
  if [ "$DRY" = 1 ]; then
    say "+ $SUDO pacman -U $*"
  else
    cd "$TMP"
    $SUDO pacman -U "$@"
  fi
else
  tb="tspmo-$VER-x86_64-linux.tar.gz"
  fetch "$tb"
  ppd_note
  if [ "$DRY" = 1 ]; then
    say "+ tar -xzf $tb"
    root="$TMP/tspmo-$VER-x86_64-linux"
  else
    tar -xzf "$TMP/$tb" -C "$TMP"
    root="$TMP/tspmo-$VER-x86_64-linux"
    [ -f "$root/install-files.sh" ] || die "unexpected tarball layout"
  fi
  # tspmod provides the power-profiles D-Bus names; keep the originals for uninstall.
  for f in /usr/share/dbus-1/system-services/net.hadess.PowerProfiles.service \
    /usr/share/dbus-1/system-services/org.freedesktop.UPower.PowerProfiles.service; do
    if [ -f "$f" ] && ! grep -q tspmod "$f" 2>/dev/null && [ ! -e "$f.tspmo-orig" ]; then
      as_root cp -p "$f" "$f.tspmo-orig"
    fi
  done
  as_root sh "$root/install-files.sh" core "$root" / /usr
  if have dkms; then
    as_root sh "$root/install-files.sh" dkms "$root" / /usr
    as_root dkms install tspmo-hid-asus/1 ||
      warn "DKMS build failed (kernel headers installed?); tspmo works without it"
  else
    say "dkms not found: skipping the keyboard autosuspend driver"
  fi
  as_root systemctl daemon-reload
fi

for u in power-profiles-daemon tuned-ppd; do
  if systemctl is-enabled "$u.service" >/dev/null 2>&1; then
    as_root systemctl disable --now "$u.service" || true
  fi
done
as_root systemctl reload dbus.service || true
as_root systemctl enable --now tspmod.service

# ---- ROG-key shortcut for the person who ran this ---------------------------------------
if [ "$(id -u)" != 0 ]; then
  run tspmo-gui --install-shortcut || warn "could not install the ROG-key shortcut; run: tspmo-gui --install-shortcut"
elif [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
  run runuser -u "$SUDO_USER" -- tspmo-gui --install-shortcut ||
    warn "could not install the ROG-key shortcut; run as your user: tspmo-gui --install-shortcut"
else
  say "ran as root: run 'tspmo-gui --install-shortcut' as your own user to bind the ROG key"
fi

cat <<EOF

tspmo $VER is installed and tspmod is running.
Next:
  1. Log out and back in once (session environment, ROG-key shortcut).
  2. tspmo info      what this laptop exposes
     tspmo doctor    what wastes battery, with fixes
  3. Press the ROG key, or run tspmo-gui.
Uninstall: curl -fsSL $RAW/install.sh | sh -s -- --uninstall
EOF
