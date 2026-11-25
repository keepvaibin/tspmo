#!/usr/bin/env bash
FANHW=$(dirname "$(grep -lx asus_custom_fan_curve /sys/class/hwmon/*/name 2>/dev/null | head -1)")  # hwmon numbers change per boot
# End-to-end test of the installed tspmod on the real machine.
# Reads every property of every org.tspmo.* interface, sets and restores settings (checking
# sysfs readback), then measures daemon CPU and dGPU runtime_active_time over an idle window.
# Never reads NVIDIA dGPU sysfs beyond power/runtime_* . Always restores state.
# Cycles every power mode (checking sysfs, fans, the Eco+ guard) unless --quick.
# Usage: tools/test/e2e.sh [--idle SECONDS] [--fnlock] [--quick]   (needs busctl; sudo for sysfs reads)
set -u
IDLE=60
FNLOCK=0
MODES=1
while [ $# -gt 0 ]; do
  case $1 in
    --idle) IDLE=$2; shift ;;
    --fnlock) FNLOCK=1 ;;
    --quick) MODES=0 ;;
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
str() { sed 's/^[a-z]* "\(.*\)"$/\1/'; }
prop() { bus get-property $SVC $OBJ "$1" "$2" 2>/dev/null; }
call() { bus call $SVC $OBJ "$@" 2>&1; }
eq() { [ "$1" = "$2" ]; }
# A GPU switch may be running or waiting after a profile change (Eco+ switches the GPU first and
# holds its thermal-mode write back until it is over): wait until Daemon1.GpuSwitching is empty,
# at most 120 s.
gpu_idle() {
  local i
  for ((i = 0; i < 240; i++)); do
    [ "$(prop org.tspmo.Daemon1 GpuSwitching | str)" = "" ] && return 0
    sleep 0.5
  done
  return 1
}
# After SetProfile: let the daemon start whatever it starts, then wait for the GPU switch.
settle() { sleep "${1:-2.5}"; gpu_idle; }

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

# --- read-only checks
if has org.tspmo.Fans1; then
  s=$(date +%s%N)
  prop org.tspmo.Fans1 Rpm >/dev/null
  ms=$((($(date +%s%N) - s) / 1000000))
  record "Fans1.Rpm read" "$([ $ms -le 100 ] && echo PASS || echo FAIL)" "${ms} ms"
fi
has org.tspmo.Power1 && check "Power1 Limits readable" prop org.tspmo.Power1 Limits
has org.tspmo.Lid1 && check "Lid1 Devices readable" prop org.tspmo.Lid1 Devices

# --- Daemon1: every mode in turn, sysfs checked, then the user's mode back
if has org.tspmo.Daemon1 && [ "$MODES" = 1 ]; then
  op=$(prop org.tspmo.Daemon1 ProfileAc | str)
  ob=$(prop org.tspmo.Daemon1 ProfileBattery | str)
  onb=$(prop org.tspmo.Daemon1 OnBattery | awk '{print $2}')
  RESTORE+=("sed -i 's/^profile_ac=.*/profile_ac=$op/; s/^profile_battery=.*/profile_battery=$ob/' /var/lib/tspmo/state; systemctl restart tspmod")
  pl1=/sys/class/powercap/intel-rapl:0/constraint_0_power_limit_uw
  e14=/sys/devices/system/cpu/cpu14/cpufreq/scaling_max_freq
  ppd() { bus get-property org.freedesktop.UPower.PowerProfiles /org/freedesktop/UPower/PowerProfiles org.freedesktop.UPower.PowerProfiles ActiveProfile | str; }
  base_pl1=$(cat $pl1); base_e14=$(cat $e14)
  for m in eco-plus silent balanced performance; do
    call org.tspmo.Daemon1 SetProfile s $m >/dev/null; settle 2.5
    check "SetProfile $m -> Profile" eq "$(prop org.tspmo.Daemon1 Profile | str)" "$m"
    # KDE's three names: Eco+ is power-saver; Silent has none and reads balanced.
    case $m in eco-plus) want=power-saver ;; silent) want=balanced ;; *) want=$m ;; esac
    check "SetProfile $m -> power-profiles ActiveProfile" eq "$(ppd)" "$want"
    pp=$(cat /sys/firmware/acpi/platform_profile)
    case $m in eco-plus|silent) w=low-power ;; balanced) w=balanced ;; performance) w=performance ;; esac
    [ "$pp" = quiet ] && pp=low-power
    check "$m -> platform_profile $w" eq "$pp" "$w"
    if [ $m = eco-plus ]; then
      check "eco-plus -> PL1 12 W" eq "$(cat $pl1)" 12000000
      check "eco-plus -> efficiency cores 1.2 GHz" eq "$(cat $e14)" 1200000
      check "eco-plus -> custom fan curve in force" eq "$(cat $FANHW/pwm1_enable 2>/dev/null)" 1
      check "eco-plus -> fan curve is 0% when cool" eq "$(cat $FANHW/pwm1_auto_point2_pwm 2>/dev/null)" 0
      check "eco-plus -> 60 Hz" eq "$(prop org.tspmo.Display1 Refresh | awk '{printf "%d", $2}')" 60
      check "eco-plus -> tspmobg.slice active" eq "$(systemctl is-active tspmobg.slice)" active
    else
      check "$m -> PL1 back" eq "$(cat $pl1)" "$base_pl1"
      check "$m -> efficiency cores uncapped" eq "$(cat $e14)" "$base_e14"
      if [ $m = silent ]; then
        # Silent is about noise: the fans are stopped while cool, nothing else of Eco+.
        check "silent -> custom fan curve in force" eq "$(cat $FANHW/pwm1_enable 2>/dev/null)" 1
        check "silent -> fan curve is 0% when cool" eq "$(cat $FANHW/pwm1_auto_point2_pwm 2>/dev/null)" 0
        check "silent -> not 60 Hz-locked by Eco+" test "$(prop org.tspmo.Display1 RefreshPolicy | str)" != ""
      fi
    fi
  done
  # KDE: power-saver selects Eco+ (and Eco+ reads power-saver); balanced leaves Eco+ but keeps
  # Silent (which KDE reads as balanced); performance leaves either.
  kde() { bus set-property org.freedesktop.UPower.PowerProfiles /org/freedesktop/UPower/PowerProfiles org.freedesktop.UPower.PowerProfiles ActiveProfile s "$1" 2>/dev/null; sleep 1.5; gpu_idle; }
  call org.tspmo.Daemon1 SetProfile s balanced >/dev/null; settle 1.5
  kde power-saver
  check "KDE power-saver selects eco-plus" eq "$(prop org.tspmo.Daemon1 Profile | str)" eco-plus
  check "KDE reads eco-plus as power-saver" eq "$(ppd)" power-saver
  kde balanced
  check "KDE balanced leaves eco-plus" eq "$(prop org.tspmo.Daemon1 Profile | str)" balanced
  call org.tspmo.Daemon1 SetProfile s silent >/dev/null; settle 1.5
  kde balanced
  check "KDE balanced keeps silent" eq "$(prop org.tspmo.Daemon1 Profile | str)" silent
  kde performance
  check "KDE performance leaves silent" eq "$(prop org.tspmo.Daemon1 Profile | str)" performance
  call org.tspmo.Daemon1 SetProfile s balanced >/dev/null; settle 1.5
  # Aliases of the CLI.
  for a in power-saver saver eco; do
    tspmo profile $a >/dev/null; settle 1.5
    check "tspmo profile $a -> eco-plus" eq "$(prop org.tspmo.Daemon1 Profile | str)" eco-plus
  done
  tspmo profile quiet >/dev/null; settle 1.5
  check "tspmo profile quiet -> silent" eq "$(prop org.tspmo.Daemon1 Profile | str)" silent
  # Fn+F5 handler (tspmo debug fan-key): one step from the latest mode, eco-plus -> silent ->
  # balanced -> performance -> eco-plus; the hardware ends in the chosen thermal mode.
  tspmo profile eco-plus >/dev/null; settle 2
  for want in silent balanced performance eco-plus; do
    tspmo debug fan-key >/dev/null; sleep 1.5; gpu_idle
    check "fan key -> $want" eq "$(prop org.tspmo.Daemon1 Profile | str)" "$want"
  done
  # Rapid presses each advance one step (presses 50 ms apart or more).
  tspmo profile eco-plus >/dev/null; settle 2
  for _ in 1 2 3; do tspmo debug fan-key >/dev/null; sleep 0.12; done
  sleep 1.5; gpu_idle
  check "fan key x3 rapid -> performance" eq "$(prop org.tspmo.Daemon1 Profile | str)" performance
  call org.tspmo.Daemon1 SetProfile s balanced >/dev/null; settle 1.5
  call org.tspmo.Daemon1 SetProfile s warp >/dev/null 2>&1 && record "SetProfile rejects 'warp'" FAIL "" || record "SetProfile rejects 'warp'" PASS ""
  # Cycle: four steps return to the start.
  s0=$(prop org.tspmo.Daemon1 Profile | str)
  for _ in 1 2 3 4; do call org.tspmo.Daemon1 CycleProfile >/dev/null; settle 1.5; done
  if [ "$(prop org.tspmo.Daemon1 OnBattery | awk '{print $2}')" = "$onb" ]; then
    check "CycleProfile x4 returns" eq "$(prop org.tspmo.Daemon1 Profile | str)" "$s0"
  else
    record "CycleProfile x4 returns" SKIP "power source changed during the test"
  fi
  # The guard: a busy job outside the desktop moves to the efficiency cores in Eco+ and back.
  call org.tspmo.Daemon1 SetProfile s eco-plus >/dev/null; settle 2
  sudo systemd-run --quiet --collect --unit=tspmo-e2e-burn --slice=tspmo-e2e.slice /usr/bin/tspmo bench --burn 4 2>/dev/null
  moved=0
  for _ in $(seq 1 14); do sleep 1.5; [ "$(wc -l < /sys/fs/cgroup/tspmobg.slice/cgroup.procs 2>/dev/null)" -gt 0 ] && { moved=1; break; }; done
  check "guard moves a busy job to tspmobg.slice" eq "$moved" 1
  call org.tspmo.Daemon1 SetProfile s balanced >/dev/null; settle 3
  sudo systemctl stop tspmo-e2e-burn 2>/dev/null
  check "guard: slice stopped after eco-plus" eq "$(systemctl is-active tspmobg.slice)" inactive
  check "guard: nothing left running" eq "$(pgrep -fc 'bench --burn 4' | awk '{print ($1 > 2) ? 1 : 0}')" 0
fi

# --- Fans1 / Power1: the settings of a mode that is not active (nothing reaches the hardware)
if has org.tspmo.Fans1 && [ "$MODES" = 1 ]; then
  cur=$(prop org.tspmo.Fans1 Mode | str); other=balanced; [ "$cur" = balanced ] && other=performance
  RESTORE+=("tspmo fans $other all auto")
  tspmo fans $other cpu 30:0,40:0,50:0,60:5,70:20,75:70,80:85,100:100 >/dev/null 2>&1
  check "Fans1 SetCurve stored for $other" eq "$(tspmo fans | grep -A3 "^$other" | grep -c 'custom')" 1
  tspmo fans $other cpu 30:0,40:0,50:0,60:0,70:0,80:0,90:0,100:0 >/dev/null 2>&1 && record "Fans1 refuses a stopped fan at 90 C" FAIL "" || record "Fans1 refuses a stopped fan at 90 C" PASS ""
  tspmo fans $other all auto >/dev/null 2>&1
  check "Fans1 SetAuto" eq "$(tspmo fans | grep -A3 "^$other" | grep -c 'custom')" 0
  check "Fans1.Temps has the CPU" eq "$(bus get-property $SVC $OBJ org.tspmo.Fans1 Temps | awk '{print $2}')" 1
fi
if has org.tspmo.Power1 && [ "$MODES" = 1 ]; then
  RESTORE+=("tspmo limits silent reset")
  tspmo limits silent ac ppt_pl1_spl 60 >/dev/null 2>&1
  check "Power1 SetLimit stored" eq "$(tspmo limits | grep -c ' 60')" 1
  tspmo limits silent reset >/dev/null 2>&1
  check "Power1 Reset" eq "$(tspmo limits | grep -c ' 60')" 0
fi

# --- Display1: refresh policy and back
if has org.tspmo.Display1 && [ "$MODES" = 1 ]; then
  pol=$(prop org.tspmo.Display1 RefreshPolicy | str)
  RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Display1 SetRefreshPolicy s $pol")
  call org.tspmo.Display1 SetRefreshPolicy s 60 >/dev/null; sleep 2.5
  check "Display1 policy 60 -> 60 Hz" eq "$(prop org.tspmo.Display1 Refresh | awk '{printf "%d", $2}')" 60
  call org.tspmo.Display1 SetRefreshPolicy s "$pol" >/dev/null; sleep 2.5
  check "Display1 policy restored ($pol)" eq "$(prop org.tspmo.Display1 RefreshPolicy | str)" "$pol"
  call org.tspmo.Display1 SetRefreshPolicy s warp >/dev/null 2>&1 && record "Display1 rejects policy 'warp'" FAIL "" || record "Display1 rejects policy 'warp'" PASS ""
  # Colour mode, eye care and dimming: set, read back, put back.
  cm=$(prop org.tspmo.Display1 ColorMode | str)
  RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Display1 SetColorMode s $cm")
  call org.tspmo.Display1 SetColorMode s srgb >/dev/null; sleep 1.5
  check "Display1 colour srgb" eq "$(prop org.tspmo.Display1 ColorMode | str)" srgb
  call org.tspmo.Display1 SetColorMode s "$cm" >/dev/null; sleep 1.5
  check "Display1 colour restored ($cm)" eq "$(prop org.tspmo.Display1 ColorMode | str)" "$cm"
  call org.tspmo.Display1 SetColorMode s neon >/dev/null 2>&1 && record "Display1 rejects colour 'neon'" FAIL "" || record "Display1 rejects colour 'neon'" PASS ""
  ec=$(prop org.tspmo.Display1 EyeCare | awk '{print $2}')
  RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Display1 SetEyeCare u $ec")
  call org.tspmo.Display1 SetEyeCare u 4500 >/dev/null; sleep 1.5
  check "Display1 eye care 4500 K" eq "$(prop org.tspmo.Display1 EyeCare | awk '{print $2}')" 4500
  call org.tspmo.Display1 SetEyeCare u "$ec" >/dev/null; sleep 1.5
  check "Display1 eye care restored ($ec)" eq "$(prop org.tspmo.Display1 EyeCare | awk '{print $2}')" "$ec"
  br=$(prop org.tspmo.Display1 Brightness | awk '{print $2}')
  RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Display1 SetBrightness u $br")
  call org.tspmo.Display1 SetBrightness u "$br" >/dev/null; sleep 1
  check "Display1 brightness unchanged by its own value" eq "$(prop org.tspmo.Display1 Brightness | awk '{print $2}')" "$br"
  call org.tspmo.Display1 SetBrightness u 0 >/dev/null 2>&1 && record "Display1 rejects brightness 0" FAIL "" || record "Display1 rejects brightness 0" PASS ""
fi

# --- Lid1 / Lighting1 / Battery1: settings that reach hardware, flipped and restored
if has org.tspmo.Lid1 && [ "$(prop org.tspmo.Lid1 Devices | grep -c slash)" = 1 ]; then
  se=$(prop org.tspmo.Lid1 SlashEnabled | awk '{print $2}')
  RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Lid1 SetSlashEnabled b $se")
  nse=$([ "$se" = true ] && echo false || echo true)
  call org.tspmo.Lid1 SetSlashEnabled b $nse >/dev/null
  check "Lid1.SetSlashEnabled $nse" eq "$(prop org.tspmo.Lid1 SlashEnabled | awk '{print $2}')" $nse
  call org.tspmo.Lid1 SetSlashEnabled b $se >/dev/null
  check "Lid1 Slash restored ($se)" eq "$(prop org.tspmo.Lid1 SlashEnabled | awk '{print $2}')" $se
fi
if has org.tspmo.Lighting1; then
  idle=$(prop org.tspmo.Lighting1 IdleTimeout | awk '{print $2}')
  RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Lighting1 SetIdleTimeout u $idle")
  call org.tspmo.Lighting1 SetIdleTimeout u 45 >/dev/null
  check "Lighting1.SetIdleTimeout 45" eq "$(prop org.tspmo.Lighting1 IdleTimeout | awk '{print $2}')" 45
  call org.tspmo.Lighting1 SetIdleTimeout u "$idle" >/dev/null
  check "Lighting1 idle restored ($idle)" eq "$(prop org.tspmo.Lighting1 IdleTimeout | awk '{print $2}')" "$idle"
fi
if has org.tspmo.Battery1; then
  co=$(prop org.tspmo.Battery1 ChargeOnce | awk '{print $2}')
  RESTORE+=("busctl --system call $SVC $OBJ org.tspmo.Battery1 SetChargeOnce b $co")
  call org.tspmo.Battery1 SetChargeOnce b true >/dev/null
  check "Battery1.SetChargeOnce true" eq "$(prop org.tspmo.Battery1 ChargeOnce | awk '{print $2}')" true
  call org.tspmo.Battery1 SetChargeOnce b false >/dev/null
  check "Battery1 ChargeOnce off" eq "$(prop org.tspmo.Battery1 ChargeOnce | awk '{print $2}')" false
fi

# --- latency of the daemon's answers
pg=$(tspmo ping 200 2>&1)
p95=$(echo "$pg" | sed -n 's/.*p95 \([0-9.]*\) ms.*/\1/p')
record "D-Bus round trip p95 < 10 ms" "$(awk -v p="${p95:-99}" 'BEGIN{print (p < 10) ? "PASS" : "FAIL"}')" "p95 ${p95} ms"
s=$(date +%s%N); bus call $SVC $OBJ org.tspmo.Daemon1 Doctor >/dev/null 2>&1; ms=$((($(date +%s%N) - s) / 1000000))
record "Daemon1.Doctor" "$([ $ms -le 500 ] && echo PASS || echo FAIL)" "${ms} ms"

# --- idle: daemon CPU and dGPU runtime
pid=$(systemctl show -p MainPID --value tspmod)
rt() { sudo cat $DGPU/power/runtime_active_time 2>/dev/null; }
cpu() { awk '{print $14+$15}' /proc/$pid/stat; }
rs0=$(sudo cat $DGPU/power/runtime_status 2>/dev/null)
# "tid count" per thread (voluntary switches only: the ones that are wake-ups).
sw() { for t in /proc/$pid/task/*; do awk -v t="${t##*/}" '/^voluntary_ctxt_switches/{print t, $2}' $t/status 2>/dev/null; done; }
# Wake-ups between two samples, summed over the threads present in both (a thread that exits
# takes its counter with it and must not make the difference negative).
swdiff() { awk 'NR==FNR{a[$1]=$2; next} ($1 in a){d+=$2-a[$1]} END{print d+0}' <(echo "$1") <(echo "$2"); }
# Every runtime resume of the dGPU in the window, by the task that asked (kernel rpm events;
# nothing of the dGPU is read).
TR=/sys/kernel/tracing
sudo sh -c "echo 0 > $TR/tracing_on; echo > $TR/trace; echo 'name == \"0000:01:00.0\"' > $TR/events/rpm/rpm_resume/filter; echo 1 > $TR/events/rpm/rpm_resume/enable; echo 1 > $TR/tracing_on" 2>/dev/null
# No GPU switch may be running in the window (its thread would count as the daemon's cost).
gpu_idle || record "GPU switch finished before the idle window" FAIL "still switching after 120 s"
# Let the daemon finish handling this script's own D-Bus clients (connect/disconnect signals).
sleep 15
gpu_idle
a0=$(rt); c0=$(cpu); w0=$(sw)
sleep "$IDLE"
a1=$(rt); c1=$(cpu); w1=$(sw)
resumes=$(sudo cat $TR/trace 2>/dev/null | grep "rpm_resume:")
sudo sh -c "echo 0 > $TR/tracing_on; echo 0 > $TR/events/rpm/rpm_resume/enable; echo 0 > $TR/events/rpm/rpm_resume/filter; echo > $TR/trace" 2>/dev/null
tck=$(getconf CLK_TCK)
cpu_ms=$(( (c1 - c0) * 1000 / tck ))
wk=$(awk -v d="$(swdiff "$w0" "$w1")" -v s=$IDLE 'BEGIN{printf "%.2f", d / s}')
if pgrep -x tspmo-gui >/dev/null; then
  # A running window polls the daemon (fan speed, temperature): that is its cost, not idle.
  record "tspmod CPU and wake-ups over ${IDLE}s" SKIP "tspmo-gui running: ${cpu_ms} ms, ${wk}/s"
else
  record "tspmod CPU over ${IDLE}s idle" "$([ $cpu_ms -le 50 ] && echo PASS || echo FAIL)" "${cpu_ms} ms"
  record "tspmod wake-ups over ${IDLE}s idle" "$(awk -v w=$wk 'BEGIN{print (w <= 0.5) ? "PASS" : "FAIL"}')" "${wk}/s"
fi
n=$(echo "$resumes" | grep -c "rpm_resume:")
mine=$(echo "$resumes" | grep -c -E '^ *(tspmod|tspmo|asus-|eco-|gpu-|fan-key|platform-profil|battery|upower|logind|name-owners|display|backlight|zbus|async-io|blocking)')
record "dGPU resumes not caused by tspmod (${IDLE}s)" "$([ "$mine" = 0 ] && echo PASS || echo FAIL)" "$n resumes, $mine from tspmod"
if [ "$rs0" = suspended ] && [ "$n" = 0 ]; then
  record "dGPU runtime_active_time over ${IDLE}s" "$([ "$a1" = "$a0" ] && echo PASS || echo FAIL)" "${a0} -> ${a1}"
fi
rss=$(awk '/VmRSS/{print $2}' /proc/$pid/status)
record "tspmod RSS" "$([ "${rss:-0}" -le 20000 ] && echo PASS || echo FAIL)" "${rss} kB"

trap - EXIT
restore_all
gpu_idle  # leave no GPU switch running behind the script
echo
echo "tspmo e2e ($ver)"
printf '%s\n' "${RESULTS[@]}"
echo
echo "failures: $FAILS"
[ "$FAILS" -eq 0 ]
