#!/bin/bash
S=/tmp/claude-1000/-home-keepvaibin/ed2a3f7b-7c8d-4097-90df-dbb2b3fd48b1/scratchpad
P=/sys/class/powercap/intel-rapl:1; T=/sys/class/power_supply/BAT1/charge_control_end_threshold
echo 40 | sudo tee $T >/dev/null   # stop charging so platform power isn't polluted
rm -f $S/chunks.* $S/results3.txt
python3 -I $S/load.py $S/chunks & LOAD=$!
psys(){ echo $1 | sudo tee $P/constraint_0_power_limit_uw >/dev/null; echo $2 | sudo tee $P/constraint_1_power_limit_uw >/dev/null; }
hz(){ kscreen-doctor output.eDP-1.mode.$1 >/dev/null 2>&1; }
cond(){ psys 190000000 260000000; hz 1; case $1 in A) ;; E) psys 5000000 7000000; hz 2 ;; F) psys 6000000 8000000; hz 2 ;; esac; }
measure(){ sleep 8; t0=$(date +%s.%N)
  w=$(sudo turbostat --quiet -S -i 20 -n 1 2>/dev/null | awk 'NR==1{for(i=1;i<=NF;i++)h[$i]=i} NR==2{print $h["SysWatt"], $h["PkgWatt"]}')
  t1=$(date +%s.%N)
  ms=$(cat $S/chunks.* | awk -v a=$t0 -v b=$t1 '$1>=a && $1<=b {print $2}' | sort -n | awk '{v[NR]=$1} END{print v[int(NR/2)+1], v[int(NR*0.95)]}')
  echo "$1 $w $ms" >> $S/results3.txt; }
tspmo profile silent >/dev/null
for round in 1 2 3; do for c in A E F; do cond $c; measure $c; done; done
cond A; kill $LOAD; for p in $(pgrep -x python3); do grep -q load.py /proc/$p/cmdline 2>/dev/null && kill $p; done
echo 100 | sudo tee $T >/dev/null
tspmo profile balanced >/dev/null
echo DONE >> $S/results3.txt
