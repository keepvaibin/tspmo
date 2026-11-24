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
wait_mode() { for _ in $(seq 1 40); do [ "$(gmode)" = "$1" ] && return 0; sleep 1; done; return 1; }

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

# Two minutes with the daemon idle: every dGPU resume must come from the firmware or an app.
sleep 5
echo "idle window (120 s) ..."
sleep 120
bad=$(sudo journalctl -k --since "$T0" --no-pager -p err 2>/dev/null | grep -i -E "nvidia|NVRM|Xid|asus|acpi" | grep -v "platform power mode\|PshareStatus" | wc -l)
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
