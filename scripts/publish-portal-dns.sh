#!/bin/bash
# Publish the records stormbootx discovers: _nvme-disc._tcp in a zone that every
# network answers for itself.
#
# The service name is NVMe-oF TP8009's DNS-SD binding. The zone is deliberately
# NOT per-network: the DNS server already comes from DHCP and microdns already
# runs one per network, so one fixed name answered differently by each network's
# resolver is what makes a machine survive being moved. A box on g16 asks g16's
# resolver and gets g16's portal; carry it to g8 and the same question gets the
# other answer, with nothing on the machine edited.
#
# Run this once per network, against that network's microdns:
#
#   ./scripts/publish-portal-dns.sh --dns 192.168.8.252  --portal 192.168.8.150
#   ./scripts/publish-portal-dns.sh --dns 192.168.16.252 --portal 192.168.31.202
#
# The portal gets an A record inside the zone rather than the SRV pointing at a
# name in some other zone: it keeps each network's answer self-contained, so
# discovery does not depend on that resolver being able to forward.
set -euo pipefail

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

ZONE="storm.lo"
HOST="portal"
SERVICE="_nvme-disc._tcp"
DNS=""
PORTAL=""
PORT="4420"
NQN="nqn.2026-09.lo.g16:stormcos"
NSID="2"
TTL="300"

usage() { sed -n '2,20p' "$0" | sed 's/^# \?//'; cat <<'USAGE'

Options:
  --dns ADDR       the microdns for this network (required)
  --portal ADDR    the NVMe/TCP portal on this network (required)
  --port N         portal port (default 4420)
  --nqn NQN        subsystem NQN published in TXT
  --nsid N         namespace published in TXT (default 2)
  --zone NAME      service zone (default storm.lo — must match DEFAULTS.zone)
  --ttl N          record TTL (default 300)
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dns)    DNS="$2"; shift 2 ;;
        --portal) PORTAL="$2"; shift 2 ;;
        --port)   PORT="$2"; shift 2 ;;
        --nqn)    NQN="$2"; shift 2 ;;
        --nsid)   NSID="$2"; shift 2 ;;
        --zone)   ZONE="$2"; shift 2 ;;
        --ttl)    TTL="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1 (--help for usage)" ;;
    esac
done

[[ -n "$DNS"    ]] || die "--dns is required"
[[ -n "$PORTAL" ]] || die "--portal is required"
command -v curl >/dev/null || die "curl not installed"
command -v python3 >/dev/null || die "python3 not installed"

API="http://$DNS:8080/api/v1"

jq_get() { python3 -c 'import json,sys; print(json.load(sys.stdin).get(sys.argv[1],""))' "$1"; }

say "zone $ZONE on $DNS"
ZONE_ID="$(curl -sf "$API/zones" \
    | python3 -c 'import json,sys
z=[x for x in json.load(sys.stdin) if x.get("name")==sys.argv[1]]
print(z[0]["id"] if z else "")' "$ZONE" || true)"

if [[ -z "$ZONE_ID" ]]; then
    say "creating zone $ZONE"
    ZONE_ID="$(curl -sf -X POST "$API/zones" -H 'Content-Type: application/json' \
        -d "{\"name\": \"$ZONE\"}" | jq_get id)"
    [[ -n "$ZONE_ID" ]] || die "could not create zone $ZONE"
fi
say "zone id $ZONE_ID"

post() {
    # Duplicates are rejected and the existing record returned, so this is
    # safe to re-run — which matters, because the portal address is exactly
    # the thing that changes.
    curl -sf -X POST "$API/zones/$ZONE_ID/records" \
        -H 'Content-Type: application/json' -d "$1" >/dev/null \
        || die "record POST failed: $1"
}

say "A     $HOST.$ZONE -> $PORTAL"
post "{\"name\":\"$HOST\",\"ttl\":$TTL,\"enabled\":true,\"data\":{\"type\":\"A\",\"data\":\"$PORTAL\"}}"

say "SRV   $SERVICE.$ZONE -> $HOST.$ZONE:$PORT"
post "{\"name\":\"$SERVICE\",\"ttl\":$TTL,\"enabled\":true,\"data\":{\"type\":\"SRV\",\"data\":{\"priority\":10,\"weight\":10,\"port\":$PORT,\"target\":\"$HOST.$ZONE\"}}}"

# Two TXT records rather than one with two strings: the microdns API takes a
# single string per record, and stormbootx reads key=value across every TXT
# record and every string within one.
say "TXT   $SERVICE.$ZONE -> nqn=$NQN"
post "{\"name\":\"$SERVICE\",\"ttl\":$TTL,\"enabled\":true,\"data\":{\"type\":\"TXT\",\"data\":\"nqn=$NQN\"}}"
say "TXT   $SERVICE.$ZONE -> nsid=$NSID"
post "{\"name\":\"$SERVICE\",\"ttl\":$TTL,\"enabled\":true,\"data\":{\"type\":\"TXT\",\"data\":\"nsid=$NSID\"}}"

cat <<EOF2

  Published. Check it the way stormbootx will:
    ./tests/dns-wire/run.sh $DNS

  A machine on this network now boots nvme-tcp://$PORTAL:$PORT/$NQN?nsid=$NSID
  with nothing on its stick naming any of that.
EOF2
