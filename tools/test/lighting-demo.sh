#!/usr/bin/env bash
# Walk the keyboard backlight or the Slash through what the daemon offers, a step every few
# seconds, announcing each one, so that someone watching can say what they saw. Ends on the
# settings it started with (keyboard: static white level 1; Slash: what `tspmo lid` showed).
#   lighting-demo.sh keyboard | slash
set -u

step() { echo "== $*"; }

keyboard() {
  local s
  for s in "brightness 0" "brightness 3" "brightness 1" "rainbow-cycle" \
           "breathe ff0000 0000ff med" "pulse 00ff00 high" "static ffffff"; do
    step "tspmo light $s"
    # shellcheck disable=SC2086
    tspmo light $s || echo "   (refused)"
    sleep 4
  done
  tspmo light brightness 1
}

slash() {
  local mode0 bright0 int0 battery0 m i b
  mode0=$(tspmo lid | sed -n 's/.*mode \([a-z-]*\)$/\1/p' | head -n1)
  bright0=$(tspmo lid | sed -n 's/.*brightness \([0-9]*\) .*/\1/p' | head -n1)
  int0=$(tspmo lid | sed -n 's/.*interval \([0-9]*\) s.*/\1/p' | head -n1)
  battery0=$(tspmo lid | sed -n 's/.*battery \(on\|off\).*/\1/p' | head -n1)
  echo "(started with mode $mode0, brightness $bright0, interval $int0, show on battery $battery0)"
  tspmo lid show battery on
  for m in static bounce flux buzzer slash; do
    step "mode $m"; tspmo lid mode "$m" || echo "   (refused)"; sleep 4
  done
  for i in 5 0; do
    step "interval $i s"; tspmo lid interval "$i"; sleep 8
  done
  for b in low medium high; do
    step "brightness $b"; tspmo lid brightness "$b"; sleep 3
  done
  tspmo lid mode "${mode0:-slash}"
  tspmo lid brightness "${bright0:-255}"
  tspmo lid interval "${int0:-0}"
  tspmo lid show battery "${battery0:-off}"
}

case "${1:-}" in
  keyboard) keyboard ;;
  slash) slash ;;
  *) echo "usage: $0 keyboard|slash" >&2; exit 2 ;;
esac
