#!/bin/bash
# Interleaved battery measurement on battery power: Eco+ / Silent / Balanced, 3 rounds,
# 60 s of current_now x voltage_now after each mode settles. Restores the starting mode.
# Usage: tools/test/battery-modes.sh   (laptop unplugged and idle)
B=/sys/class/power_supply/BAT1
start=$(busctl --system get-property org.tspmo.Daemon1 /org/tspmo org.tspmo.Daemon1 Profile | awk '{print $2}' | tr -d '"')
sw(){ busctl --system get-property org.tspmo.Daemon1 /org/tspmo org.tspmo.Daemon1 GpuSwitching | awk '{print $2}' | tr -d '"'; }
gm(){ busctl --system get-property org.tspmo.Daemon1 /org/tspmo org.tspmo.Daemon1 GpuMode | awk '{print $2}' | tr -d '"'; }
out=()
for r in 1 2 3; do for m in eco-plus silent balanced; do
  [ "$(cat /sys/class/power_supply/ACAD/online)" = 1 ] && { echo "AC plugged in: stopping"; break 2; }
  tspmo profile $m >/dev/null 2>&1
  for i in $(seq 1 60); do [ -z "$(sw)" ] && break; sleep 2; done
  sleep 40
  s=0; for i in $(seq 1 60); do s=$(awk -v s=$s -v c=$(cat $B/current_now) -v v=$(cat $B/voltage_now) 'BEGIN{print s+c*v/1e12}'); sleep 1; done
  line="$r $m $(awk -v s=$s 'BEGIN{printf "%.2f", s/60}') gpu=$(gm)"; echo "$line"; out+=("$line")
done; done
tspmo profile "$start" >/dev/null 2>&1
for m in eco-plus silent balanced; do
  printf '%s\n' "${out[@]}" | awk -v m=$m '$2==m{print $3}' | sort -n | sed -n 2p | sed "s/^/median $m: /"
done
