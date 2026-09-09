#!/usr/bin/env bash
# fec-watchdog.sh — clear the S5148F 25G port latch without a human.
#
# The latch is not going to be fixed upstream: the S5148F is an XPliant-based
# switch whose last OS10 build is 10.4.3.8, and an upgrade is off the table.
# So the recovery runs on a timer instead.
#
# Deliberately conservative, because the recovery writes switch config:
#
#   * A latched port must be seen on TWO consecutive passes before anything is
#     written. One reading is never enough — that is the mistake the 0.3.4
#     self-heal made, and it cost a card a bad NV write.
#   * A port that was cycled less than COOLDOWN seconds ago is left alone, so a
#     port the cycle cannot fix produces one attempt per cooldown, not a
#     config write every time cron fires.
#   * A powered-off peer is invisible to this: the detector requires real
#     light, and a dark peer reads the -40.0 floor. Shutting a node down does
#     not trigger a cycle; only a node that came back to a latched port does.
#
# Install (on dev.g8.lo, not on the switch — a user crontab there does not
# survive an OS10 image install, and a watchdog on the failing device cannot
# report that it stopped watching):
#
#   */2 * * * * /root/work/stormbootx/scripts/fec-watchdog.sh >> /var/log/fec-watchdog.log 2>&1
#
# Env: STATE_DIR (default /var/tmp/fec-watchdog), COOLDOWN (default 900s)
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
STATE_DIR=${STATE_DIR:-/var/tmp/fec-watchdog}
COOLDOWN=${COOLDOWN:-900}
mkdir -p "$STATE_DIR"

log() { echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') $*"; }

LATCHED=$("$HERE/port-latched.sh" -q 2>/dev/null)
now=$(date -u +%s)

# Forget ports that recovered, so a later latch starts from one sighting again.
for f in "$STATE_DIR"/seen-*; do
  [ -e "$f" ] || continue
  p=$(basename "$f"); p=${p#seen-}; p=${p//_//}
  case " $LATCHED " in *" $p "*) ;; *) rm -f "$f" ;; esac
done

[ -z "$LATCHED" ] && exit 0

to_cycle=""
for p in $LATCHED; do
  key=${p//\//_}
  seen="$STATE_DIR/seen-$key"
  last="$STATE_DIR/cycled-$key"

  if [ ! -e "$seen" ]; then
    log "$p latched (first sighting; waiting for confirmation)"
    : > "$seen"
    continue
  fi

  if [ -e "$last" ]; then
    age=$(( now - $(cat "$last") ))
    if [ "$age" -lt "$COOLDOWN" ]; then
      log "$p still latched, cycled ${age}s ago (cooldown ${COOLDOWN}s) - leaving it"
      continue
    fi
    log "$p still latched ${age}s after the last cycle - the cycle is not fixing it"
  fi

  to_cycle="$to_cycle $p"
  echo "$now" > "$last"
done

[ -z "$to_cycle" ] && exit 0

log "cycling FEC on$to_cycle"
# shellcheck disable=SC2086
"$HERE/fec-cycle.sh" $to_cycle 2>&1 | sed 's/^/    /'

sleep 20
STILL=$("$HERE/port-latched.sh" -q 2>/dev/null)
for p in $to_cycle; do
  case " $STILL " in
    *" $p "*) log "$p STILL LATCHED after the cycle - needs a human" ;;
    *)        log "$p recovered"; rm -f "$STATE_DIR/seen-${p//\//_}" ;;
  esac
done
