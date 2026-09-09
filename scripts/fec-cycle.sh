#!/usr/bin/env bash
# fec-cycle.sh — revive a latched 25G SFP28 port on dsw1 (Dell S5148F, OS10).
#
# Why this exists: a 25G-SR port showing good light but `line protocol is down`
# is often NOT fixed by shutdown/no shutdown. On 2026-09-08 port 1/1/7 survived
# four bounces, an identical config to a working port, two transceivers and a
# proven fibre. What revived it was cycling the FEC:
#
#     no fec -> fec CL91-RS -> fec CL74-FC -> fec CL108-RS
#
# each stage with a shut/no-shut. A bounce only re-runs link training; only a
# FEC *change* re-programs the serdes. Always ends on CL108-RS, which is what
# the ConnectX-4 Lx device default negotiates. Never leave a port on `off`:
# no FEC on 25GBASE-SR is out of spec and cost the fabric days in 2026-09-06.
#
# Syntax note (this cost a day): the keyword is a standalone, UPPERCASE
# interface command. `cl108-rs`, `no fec off` and putting it under `speed` are
# all rejected as "Illegal parameter".
#
# Usage:  scripts/fec-cycle.sh [port ...]      # default: 1/1/5 1/1/7
# Env:    DSW1 (default 192.168.11.2), DSW1_READ (default gwest@dsw1.g11.lo),
#         DELLPASSWORD (sourced from ~/.env if unset)
set -uo pipefail

SW=${DSW1:-192.168.11.2}
READ_HOST=${DSW1_READ:-gwest@dsw1.g11.lo}
PORTS=("${@:-}")
[ -z "${PORTS[0]:-}" ] && PORTS=(1/1/5 1/1/7)

if [ -z "${DELLPASSWORD:-}" ]; then
  # shellcheck disable=SC1090
  set -a; . ~/.env; set +a
fi
: "${DELLPASSWORD:?DELLPASSWORD not set and not found in ~/.env}"

# OS10's clish does not exit on EOF over a pty, so the session is backgrounded
# and killed. Output is kept so a rejected keyword is visible, not swallowed.
apply() {
  local fec="$1" log; log=$(mktemp)
  { printf 'configure terminal\n'
    for p in "${PORTS[@]}"; do
      printf 'interface ethernet %s\nshutdown\n%s\nno shutdown\nexit\n' "$p" "$fec"
    done
    printf 'end\nexit\n'
    sleep 10
  } | sshpass -p "$DELLPASSWORD" ssh -tt -o StrictHostKeyChecking=no \
        -o PubkeyAuthentication=no "admin@$SW" >"$log" 2>&1 &
  local pid=$!
  sleep 18
  kill "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  if grep -qiE 'illegal|invalid|error|%' "$log"; then
    echo "  !! switch rejected something:"; grep -iE 'illegal|invalid|error|%' "$log" | head -5
  fi
  rm -f "$log"
}

# Link state straight from the Linux side; needs no password and never blocks.
link_state() {
  ssh -o BatchMode=yes "$READ_HOST" 'ip -br link' 2>/dev/null \
    | awk '/^e101-0(05|07)-0/ {printf "  %s %s\n", $1, $2}'
}

up_count() {
  ssh -o BatchMode=yes "$READ_HOST" 'ip -br link' 2>/dev/null \
    | awk '/^e101-0(05|07)-0/ && $2=="UP"' | wc -l | tr -d ' '
}

echo "ports: ${PORTS[*]}   switch: $SW"
echo "before:"; link_state

for F in "no fec" "fec CL91-RS" "fec CL74-FC" "fec CL108-RS"; do
  echo "=== $F ==="
  apply "$F"
  sleep 12
  link_state
  if [ "$(up_count)" -gt 0 ] && [ "$F" = "fec CL108-RS" ]; then
    echo "link is up on the final stage"
  fi
done

echo "=== settled (waiting 30s for RS training) ==="
sleep 30
link_state
ssh -o BatchMode=yes "$READ_HOST" \
  '/opt/dell/os10/bin/cps_get_oid.py -qua observed dell-base-if-cmn/if/interfaces-state/interface 2>/dev/null' 2>/dev/null \
  | awk '/e101-0(05|07)-0/{n=1} /configured-fec|interface\/name|oper-status|interface\/speed/{print "  " $0}' \
  | grep -E 'e101-0(05|07)-0|configured-fec|oper-status|speed' | head -12

echo
echo "fec enum: 2=off 3=cl91-rs 4=cl74-fc 5=cl108-rs   oper-status: 1=up 2=down"
echo "final FEC must be 5 (CL108-RS) on every port above."
