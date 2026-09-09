#!/usr/bin/env bash
# port-latched.sh — name the dsw1 SFP28 ports that are in the latched state.
#
# After the peer powers off, an S5148F 25G port can come back with the laser
# lit, the optic seated and the far end transmitting — and never achieve PCS
# lock again. It survives shutdown/no shutdown; only a FEC transition
# re-programs the serdes (scripts/fec-cycle.sh). It is not a dead cage, a dead
# optic or a dark peer, and it looks like all three. See scripts/port-latched.py
# for the signature that separates them.
#
# Reads over the passwordless gwest login: no switch config, no password, safe
# to run from cron. Exits 1 when at least one port is latched, so it can gate
# the recovery:
#
#     P=$(scripts/port-latched.sh -q) && [ -n "$P" ] && scripts/fec-cycle.sh $P
#
# Usage:  scripts/port-latched.sh [-q]      -q: print the bare port list only
# Env:    DSW1_READ (default gwest@dsw1.g11.lo), RX_FLOOR (default -20.0 dBm)
set -uo pipefail

READ_HOST=${DSW1_READ:-gwest@dsw1.g11.lo}
HERE=$(cd "$(dirname "$0")" && pwd)

DATA=$(ssh -o BatchMode=yes "$READ_HOST" '
  /opt/dell/os10/bin/cps_get_oid.py -qua observed dell-base-if-cmn/if/interfaces-state/interface 2>/dev/null
  echo "@@@SPLIT@@@"
  /opt/dell/os10/bin/cps_get_oid.py -qua observed base-pas/media-channel 2>/dev/null
' 2>/dev/null)

[ -z "$DATA" ] && { echo "cannot read $READ_HOST" >&2; exit 2; }

QUIET=0
[ "${1:-}" = "-q" ] && QUIET=1
printf '%s' "$DATA" | RX_FLOOR="${RX_FLOOR:--20.0}" QUIET="$QUIET" python3 "$HERE/port-latched.py"
