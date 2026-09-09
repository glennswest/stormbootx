"""Classify dsw1 SFP28 ports from two CPS dumps on stdin, split by @@@SPLIT@@@.

The latched state (confirmed 2026-09-08 and 2026-09-09) is the one that looks
like three other faults at once:

    admin up  +  optic present  +  receiving real light  +  oper down

A peer that is genuinely powered off reads rx-power -40.0, the floor. A cage
with no optic produces no media record at all. Only the latch shows good light
with no PCS lock, and only a FEC transition clears it.
"""
import os
import re
import sys

FLOOR = float(os.environ.get("RX_FLOOR", "-20.0"))
QUIET = os.environ.get("QUIET") == "1"


def records(text):
    """CPS prints one record per port separated by a dashed line.

    Fields inside a record are unordered: admin-status arrives before the name
    and oper-status after it, so parsing field-by-field pairs a name with the
    previous record's oper-status. Split on the dashed line first.
    """
    buf = []
    for line in text.splitlines():
        line = line.strip()
        if len(line) > 8 and set(line) == {"-"}:
            if buf:
                yield buf
            buf = []
        elif line:
            buf.append(line)
    if buf:
        yield buf


def field(rec, key):
    """Match on the whole attribute name, not a substring of it.

    base-pas/media-channel carries both rx-power and rx-power-state, so a
    substring match on the former silently returns the latter: every port then
    reads +1.00 dBm, which looks like real light and marks healthy ports as
    latched.
    """
    for line in rec:
        name, _, value = line.partition(" = ")
        if name.strip().endswith(key):
            return value.strip()
    return None


ifs, optics = sys.stdin.read().split("@@@SPLIT@@@")

state = {}
for rec in records(ifs):
    name = field(rec, "interfaces-state/interface/name")
    if not name:
        continue
    m = re.match(r"e101-0(\d\d)-0$", name)
    if m:
        state[int(m.group(1))] = (field(rec, "interface/admin-status"),
                                  field(rec, "interface/oper-status"))

rx = {}
for rec in records(optics):
    port, val = field(rec, "media-channel/port"), field(rec, "media-channel/rx-power")
    if port is None or val is None:
        continue
    port = int(port)
    rx[port] = max(rx.get(port, -99.0), float(val))   # strongest lane wins

latched = []
for p in sorted(rx):
    if p > 48:                       # uplinks are QSFP; this fault is the SFP28 serdes
        continue
    admin, oper = state.get(p, ("?", "?"))
    if admin == "1" and oper == "2" and rx[p] > FLOOR:
        latched.append(p)

names = [f"1/1/{p}" for p in latched]
if QUIET:
    print(" ".join(names))
else:
    for p in sorted(rx):
        if p > 48:
            continue
        admin, oper = state.get(p, ("?", "?"))
        if oper == "1":
            verdict = "up"
        elif admin != "1":
            verdict = "admin down"
        elif rx[p] <= -39.0:
            verdict = "peer dark (no light)"
        elif p in latched:
            verdict = "LATCHED - light but no lock"
        else:
            verdict = "down"
        print("  1/1/%-2d  rx=%+7.2f dBm  %s" % (p, rx[p], verdict))
    if latched:
        print()
        print("latched: " + " ".join(names))
        print("recover: scripts/fec-cycle.sh " + " ".join(names))

sys.exit(1 if latched else 0)
