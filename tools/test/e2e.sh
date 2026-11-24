#!/usr/bin/env bash
# End-to-end test of the installed tspmod on the real machine.
# Reads every property of every org.tspmo.* interface, sets and restores settings (checking
# sysfs readback), then measures daemon CPU and dGPU runtime_active_time over an idle window.
# Never reads NVIDIA dGPU sysfs beyond power/runtime_* . Always restores state.
# Usage: tools/test/e2e.sh [--idle SECONDS] [--fnlock]   (needs busctl; sudo for sysfs reads)
set -u
IDLE=60
FNLOCK=0
while [ $# -gt 0 ]; do
  case $1 in
    --idle) IDLE=$2; shift ;;
    --fnlock) FNLOCK=1 ;;
  esac
  shift
done

SVC=org.tspmo.Daemon1
OBJ=/org/tspmo
DGPU=/sys/bus/pci/devices/0000:01:00.0
ARM=/sys/class/firmware-attributes/asus-armoury/attributes
declare -a RESULTS
declare -a RESTORE
FAILS=0

record() { # name status detail
  RESULTS+=("$(printf '%-44s %-5s %s' "$1" "$2" "$3")")
  [ "$2" = FAIL ] && FAILS=$((FAILS + 1))
}
check() { # name cmd...
  local n=$1; shift
  if out=$("$@" 2>&1); then record "$n" PASS ""; else record "$n" FAIL "${out:0:60}"; fi
}
bus() { busctl --system "$@"; }
prop() { bus get-property $SVC $OBJ "$1" "$2" 2>/dev/null; }
call() { bus call $SVC $OBJ "$@" 2>&1; }
eq() { [ "$1" = "$2" ]; }

restore_all() {
  for ((i = ${#RESTORE[@]} - 1; i >= 0; i--)); do eval "${RESTORE[$i]}" >/dev/null 2>&1; done
}
trap restore_all EXIT

# --- which build is running
ver=$(tspmo --version 2>/dev/null || echo unknown)
record "tspmo --version" PASS "$ver"

# --- every interface: introspect, read all properties
ifaces=$(bus introspect $SVC $OBJ --xml-interface 2>/dev/null | grep -o 'interface name="org.tspmo[^"]*"' | cut -d'"' -f2)
[ -n "$ifaces" ] || { record "daemon on the bus" FAIL "no org.tspmo interface"; printf '%s\n' "${RESULTS[@]}"; exit 1; }
for i in $ifaces; do
  s=$(date +%s%N)
  if out=$(bus call $SVC $OBJ org.freedesktop.DBus.Properties GetAll s "$i" 2>&1); then
    ms=$((($(date +%s%N) - s) / 1000000))
    # Fans1.Rpm reads firmware: allow more.
    lim=50; record "$i GetAll" "$([ $ms -le $lim ] && echo PASS || echo FAIL)" "${ms} ms"
  else
    record "$i GetAll" FAIL "${out:0:60}"
  fi
done

has() { echo "$ifaces" | grep -qx "$1"; }

# --- Battery1: limit
if has org.tspmo.Battery1; then
  orig=$(prop org.tspmo.Battery1 UserLimit | awk '{print $2}')
  cur=$(prop org.tspmo.Battery1 Limit | awk '{print $2}')
  RESTORE+=("[ '${orig:-0}' -gt 0 ] && busctl --system call $SVC $OBJ org.tspmo.Battery1 SetLimit y ${cur:-100}")
  for want in 80 100; do
    if [ "$want" != "$cur" ]; then
      call org.tspmo.Battery1 SetLimit y $want >/dev/null
      sleep 0.3
      got=$(cat /sys/class/power_supply/BAT*/charge_control_end_threshold 2>/dev/null | head -1)
      check "Battery1.SetLimit $want -> sysfs" eq "$got" "$want"
    fi
  done
  call org.tspmo.Battery1 SetLimit y 150 >/dev/null 2>&1 && record "Battery1 rejects 150" FAIL "" || record "Battery1 rejects 150" PASS ""
  call org.tspmo.Battery1 SetLimit y "${cur:-100}" >/dev/null
  got=$(cat /sys/class/power_supply/BAT*/charge_control_end_threshold 2>/dev/null | head -1)
  check "Battery1 limit restored ($cur)" eq "$got" "$cur"
fi

# --- Hardware1: every writable toggle, flipped and restored
if has org.tspmo.Hardware1; then
  for n in panel_overdrive boot_sound; do
    f=$ARM/$n/current_value
    [ -r "$f" ] || continue
    o=$(cat "$f")
    RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Hardware1 Set ss $n $o")
    flip=$((1 - o))
    call org.tspmo.Hardware1 Set ss $n $flip >/dev/null
    check "Hardware1.Set $n $flip -> sysfs" eq "$(cat $f)" "$flip"
    call org.tspmo.Hardware1 Set ss $n $o >/dev/null
    check "Hardware1 $n restored ($o)" eq "$(cat $f)" "$o"
  done
  call org.tspmo.Hardware1 Set ss panel_overdrive 7 >/dev/null 2>&1 && record "Hardware1 rejects value 7" FAIL "" || record "Hardware1 rejects value 7" PASS ""
  call org.tspmo.Hardware1 Set ss gpu_mux_mode 0 >/dev/null 2>&1 && record "Hardware1 refuses gpu_mux_mode" FAIL "" || record "Hardware1 refuses gpu_mux_mode" PASS ""
  if [ "$FNLOCK" = 1 ] && [ "$(prop org.tspmo.Hardware1 FnLockAvailable | awk '{print $2}')" = true ]; then
    ol=$(prop org.tspmo.Hardware1 FnLock | awk '{print $2}')
    RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Hardware1 SetFnLock b $ol")
    call org.tspmo.Hardware1 SetFnLock b true >/dev/null
    check "Hardware1.SetFnLock true" eq "$(prop org.tspmo.Hardware1 FnLock | awk '{print $2}')" true
    q=$(systemd-hwdb query 'evdev:input:b0003v0B05p19B6e0110-e0,1,4' 2>/dev/null | grep -c KEYBOARD_KEY_7003a=mute)
    check "hwdb has the media layout" eq "$q" 1
    call org.tspmo.Hardware1 SetFnLock b "$ol" >/dev/null
    check "Hardware1 FnLock restored ($ol)" eq "$(prop org.tspmo.Hardware1 FnLock | awk '{print $2}')" "$ol"
  fi
fi

# --- Lighting1: brightness
if has org.tspmo.Lighting1; then
  led=/sys/class/leds/asus::kbd_backlight/brightness
  ob=$(cat $led 2>/dev/null)
  if [ -n "$ob" ]; then
    RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Lighting1 SetBrightness sy '' $ob")
    nb=$(( (ob + 1) % 4 ))
    call org.tspmo.Lighting1 SetBrightness sy "" $nb >/dev/null
    sleep 0.3
    check "Lighting1.SetBrightness $nb -> sysfs" eq "$(cat $led)" "$nb"
    call org.tspmo.Lighting1 SetBrightness sy "" $ob >/dev/null
    sleep 0.3
    check "Lighting1 brightness restored ($ob)" eq "$(cat $led)" "$ob"
  fi
fi

# --- Fans1 / Power1 / Lid1: read-only here (curves and limits are not touched by e2e)
if has org.tspmo.Fans1; then
  s=$(date +%s%N)
  prop org.tspmo.Fans1 Rpm >/dev/null
  ms=$((($(date +%s%N) - s) / 1000000))
  record "Fans1.Rpm read" "$([ $ms -le 100 ] && echo PASS || echo FAIL)" "${ms} ms"
fi
has org.tspmo.Power1 && check "Power1 Limits readable" prop org.tspmo.Power1 Limits
has org.tspmo.Lid1 && check "Lid1 Devices readable" prop org.tspmo.Lid1 Devices

# --- idle: daemon CPU and dGPU runtime
pid=$(systemctl show -p MainPID --value tspmod)
rt() { sudo cat $DGPU/power/runtime_active_time 2>/dev/null; }
cpu() { awk '{print $14+$15}' /proc/$pid/stat; }
rs0=$(sudo cat $DGPU/power/runtime_status 2>/dev/null)
a0=$(rt); c0=$(cpu)
sleep "$IDLE"
a1=$(rt); c1=$(cpu)
tck=$(getconf CLK_TCK)
cpu_ms=$(( (c1 - c0) * 1000 / tck ))
record "tspmod CPU over ${IDLE}s idle" "$([ $cpu_ms -le 50 ] && echo PASS || echo FAIL)" "${cpu_ms} ms"
if [ "$rs0" = suspended ]; then
  record "dGPU runtime_active_time over ${IDLE}s" "$([ "$a1" = "$a0" ] && echo PASS || echo FAIL)" "${a0} -> ${a1} (was $rs0)"
else
  record "dGPU runtime_active_time over ${IDLE}s" SKIP "dGPU was $rs0 at start"
fi
rss=$(awk '/VmRSS/{print $2}' /proc/$pid/status)
record "tspmod RSS" "$([ "${rss:-0}" -le 20000 ] && echo PASS || echo FAIL)" "${rss} kB"

trap - EXIT
restore_all
echo
echo "tspmo e2e ($ver)"
printf '%s\n' "${RESULTS[@]}"
echo
echo "failures: $FAILS"
[ "$FAILS" -eq 0 ]
