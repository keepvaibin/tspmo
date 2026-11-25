#!/usr/bin/env bash
# Live dGPU switching on the real machine: Hybrid -> Eco -> Hybrid, Eco+ forcing Eco and
# restoring the policy, kernel log clean, no dGPU wake caused by tspmo.
# Needs a desktop session that started after tspmo's compositor setting existed (KWin must not
# hold /dev/nvidia*; `tspmo doctor` says `compositor-nvidia` otherwise). Run with sudo.
# Restores the GPU policy and the power mode it found, also on failure.
# Reads only power/runtime_* and power_state of the dGPU (never nvidia-smi, never its sensors).
set -u
D=/sys/bus/pci/devices/0000:01:00.0
RES=()
FAILS=0
rec() { RES+=("$(printf '%-52s %s %s' "$1" "$2" "${3:-}")"); [ "$2" = FAIL ] && FAILS=$((FAILS + 1)); }
gmode() { tspmo status | sed -n 's/^gpu: *\([a-z]*\).*/\1/p'; }
policy() { tspmo status | sed -n 's/^gpu:.*(policy \([a-z]*\)).*/\1/p'; }
wait_mode() { for _ in $(seq 1 "${2:-40}"); do [ "$(gmode)" = "$1" ] && return 0; sleep 1; done; return 1; }
gmsg() { tspmo status | sed -n '/^gpu:/{n;s/^ *//p}'; }
stuck() { pgrep -x rmmod >/dev/null || [ -e /run/tspmo/gpu-restart ]; }

if sudo sh -c "ls -l /proc/\$(pidof kwin_wayland)/fd 2>/dev/null" | grep -q /dev/nvidia; then
  echo "KWin holds the NVIDIA driver (the session predates tspmo's compositor setting)."
  echo "Log out and in once, then run this again."
  exit 2
fi

P0=$(policy); M0=$(tspmo profile)
restore() {
  tspmo profile "$M0" >/dev/null 2>&1
  tspmo gpu "$P0" >/dev/null 2>&1
}
trap restore EXIT
T0=$(date '+%Y-%m-%d %H:%M:%S')

tspmo gpu hybrid >/dev/null; wait_mode hybrid; rec "start in hybrid" "$([ "$(gmode)" = hybrid ] && echo PASS || echo FAIL)"
tspmo gpu eco >/dev/null
rec "hybrid -> eco" "$(wait_mode eco && echo PASS || echo FAIL)"
rec "dGPU gone from the PCI bus" "$([ ! -e $D ] && echo PASS || echo FAIL)"
tspmo gpu hybrid >/dev/null
rec "eco -> hybrid" "$(wait_mode hybrid && echo PASS || echo FAIL)"
sleep 8
tspmo profile eco-plus >/dev/null
rec "eco-plus forces eco" "$(wait_mode eco && echo PASS || echo FAIL)"
tspmo profile balanced >/dev/null
rec "leaving eco-plus restores hybrid" "$(wait_mode hybrid && echo PASS || echo FAIL)"
rec "policy unchanged by eco-plus" "$([ "$(policy)" = hybrid ] && echo PASS || echo FAIL)" "$(policy)"

# The deadlock case: Eco+ entered from Hybrid right after a thermal-mode change (the EC notifies
# the NVIDIA driver after every platform_profile write; an unload that meets that notification
# hangs `rmmod` in D state for good). tspmod must switch the GPU first, wait out 30 s since the
# last thermal write, unload only while the dGPU is runtime-suspended, and never hang.
for i in 1 2; do
  tspmo profile performance >/dev/null; sleep 2     # thermal write, dGPU about to be notified
  tspmo profile eco-plus >/dev/null                 # thermal write + switch to Eco
  rec "eco-plus entry $i right after a thermal change -> eco" "$(wait_mode eco 100 && echo PASS || echo FAIL)" "$(gmsg)"
  rec "no rmmod stuck, no restart marker ($i)" "$(stuck && echo FAIL || echo PASS)"
  tspmo profile balanced >/dev/null
  rec "leaving eco-plus restores hybrid ($i)" "$(wait_mode hybrid 60 && echo PASS || echo FAIL)"
  stuck && break
done
hung=$(sudo journalctl -k --since "$T0" --no-pager | grep -c -E "blocked for more than|rm_acpi_nvpcf_notify.*blocked|nv_acpi_methods_uninit")
rec "no hung-task report (rmmod / NVPCF notify)" "$([ "$hung" = 0 ] && echo PASS || echo FAIL)" "$hung"
tspmo status | grep -q "Restart to recover" && rec "no restart-required state" FAIL || rec "no restart-required state" PASS

# Two minutes with the daemon idle: every dGPU resume must come from the firmware or an app.
sleep 5
echo "idle window (120 s) ..."
sleep 120
bad=$(sudo journalctl -k --since "$T0" --no-pager -p err 2>/dev/null | grep -i -E "nvidia|NVRM|Xid|asus|acpi" | grep -v "platform power mode\|PshareStatus\|target temp from SBIOS\|loading NVIDIA UNIX" | wc -l)
rec "no kernel errors (nvidia, asus, acpi)" "$([ "$bad" = 0 ] && echo PASS || echo FAIL)" "$bad lines"
xid=$(sudo journalctl -k --since "$T0" --no-pager | grep -c "Xid")
rec "no Xid" "$([ "$xid" = 0 ] && echo PASS || echo FAIL)" "$xid"
popup=$(journalctl --user --since "$T0" --no-pager 2>/dev/null | grep -ci "displayconfiguration\|output chooser")
rec "no display-chooser popup logged" "$([ "$popup" = 0 ] && echo PASS || echo INFO)" "$popup"
echo
echo "tspmo gpu live"
printf '%s\n' "${RES[@]}"
echo "failures: $FAILS"
[ "$FAILS" -eq 0 ]
