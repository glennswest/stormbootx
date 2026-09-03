#!/bin/bash
# Build the USB boot agent: a GPT image whose ESP holds stormbootx as
# /EFI/BOOT/BOOTX64.EFI, plus the config file that tells it where to attach.
#
# This is the whole first hop. No PXE, no DHCP boot options, no TFTP, no HTTP:
# firmware boots the removable-media path with no NVRAM entry, the agent reads
# the machine's service tag out of SMBIOS, attaches nvme-tcp:// and publishes
# the remote image as EFI_BLOCK_IO_PROTOCOL. There is no kernel on this stick.
#
#   dd if=<output> of=/dev/sdX bs=4M conv=fsync
#
# The stick names the portal — an appliance address — and nothing about which
# image to boot. That is a fleet decision living next to the images, as a
# boothost/<service tag> synonym the agent claims at boot. Moving a machine to
# another version is a PUT on its name; the stick never changes.
#
# --discover asks DNS for the portal instead (opt-in, off by default: DNS is a
# second place for the answer to live and a timeout on every boot in a zone
# nobody published). --probe builds a diagnostic stick that boots tcp4probe.
#
# Runs ON the build box (dev.g8.lo). Output goes to /build/images — never
# /tmp, which on dev is a tmpfs sized at half of RAM.
set -euo pipefail

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

PORTAL="192.168.31.202"                     # forge.g16.lo, eth1 (MTU 9000)
PORT="4420"
NQN="nqn.2026-09.lo.g16:stormcos"
NSID="2"
ZONE="storm.lo"
API_PORT="9090"                              # engine API on the portal host
DISCOVER="no"                               # DNS discovery is opt-in now
ESP_MIB="4"
OUTDIR="/build/images"
OUTPUT=""
BIN=""
PIN="no"
PROBE="no"

usage() {
    sed -n '2,20p' "$0" | sed 's/^# \?//'
    cat <<'USAGE'

Options:
  --pin            write only the portal, with no claim knobs
  --discover       ask DNS for the portal (opt-in; off by default)
  --api-port N     engine API port on the portal host (default 9090)
  --probe          boot tcp4probe instead of the agent (a diagnostic stick)
  --zone NAME      DNS zone holding _nvme-disc._tcp (default storm.lo)
  --portal ADDR    NVMe/TCP portal, with --pin (default 192.168.31.202)
  --port N         portal port (default 4420)
  --nqn NQN        subsystem NQN (default nqn.2026-09.lo.g16:stormcos)
  --nsid N         namespace (default 2)
  --size MIB       ESP size (default 4; FAT16 needs >=4085 clusters)
  --binary PATH    prebuilt .efi (default: build it)
  --output PATH    image path (default /build/images/stormbootx.img)
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --pin)    PIN="yes"; shift ;;
        --probe)  PROBE="yes"; shift ;;
        --zone)   ZONE="$2"; shift 2 ;;
        --discover) DISCOVER="yes"; shift ;;
        --api-port) API_PORT="$2"; shift 2 ;;
        --portal) PORTAL="$2"; PIN="yes"; shift 2 ;;
        --port)   PORT="$2"; shift 2 ;;
        --nqn)    NQN="$2"; shift 2 ;;
        --nsid)   NSID="$2"; shift 2 ;;
        --size)   ESP_MIB="$2"; shift 2 ;;
        --binary) BIN="$2"; shift 2 ;;
        --output) OUTPUT="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1 (--help for usage)" ;;
    esac
done

OUTPUT="${OUTPUT:-$OUTDIR/stormbootx.img}"
case "$OUTPUT" in
    /tmp/*) die "refusing to write a disk image into /tmp (tmpfs = RAM); use $OUTDIR" ;;
esac

for tool in mkfs.fat mmd mcopy sfdisk; do
    command -v "$tool" >/dev/null || die "$tool not installed on the build host"
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

WANT="stormbootx"
[[ "$PROBE" == "yes" ]] && WANT="tcp4probe"

if [[ -z "$BIN" ]]; then
    say "building $WANT for x86_64-unknown-uefi"
    ( cd "$ROOT" && cargo build --release --target x86_64-unknown-uefi --bin "$WANT" )
    BIN="${CARGO_TARGET_DIR:-$ROOT/target}/x86_64-unknown-uefi/release/$WANT.efi"
fi
[[ -f "$BIN" ]] || die "no $WANT.efi at $BIN"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [[ "$PIN" == "yes" ]]; then
    cat > "$WORK/stormboot.conf" <<CONF
# stormbootx — this stick is PINNED.
#
# A portal line disables discovery outright: this machine attaches here and
# nowhere else, wherever it is plugged in. Delete the portal line to put it
# back on DNS discovery.
#
# Read from \stormboot\stormboot.conf on the media this booted from, found via
# EFI_LOADED_IMAGE_PROTOCOL, so it is exactly the volume that was booted and
# never a guess at which ESP is ours.
portal = $PORTAL
port   = $PORT
nqn    = $NQN
nsid   = $NSID
CONF
elif [[ "$DISCOVER" == "yes" ]]; then
    cat > "$WORK/stormboot.conf" <<CONF
# stormbootx — this stick discovers its portal over DNS.
#
# Opt-in: it asks whichever resolver DHCP hands it for the SRV and TXT records
# at _nvme-disc._tcp.$ZONE. Publish them with scripts/publish-portal-dns.sh —
# an unpublished zone costs a timeout on every boot before the floor is used.
#
# Which image this machine boots is still the service tag, not DNS. Discovery
# only answers *where* the appliance is.
zone     = $ZONE
discover = yes
CONF
else
    cat > "$WORK/stormboot.conf" <<CONF
# stormbootx — the portal is an appliance address, the image is this machine's.
#
# Nothing here says which image to boot. That is a fleet decision and it lives
# next to the images: a boothost/<service tag> synonym on the engine, claimed
# at $PORTAL:$API_PORT in one request that answers with a copy-on-write clone
# and the address, NQN and NSID reaching it. Moving this machine to another
# version is a PUT on its name — this stick does not change.
#
# nqn and nsid below are only the fallback, for a claim that cannot be reached:
# an image nobody assigned beats no image.
#
# DNS discovery is off. It was the default while the portal was the thing a
# machine had to be told; the portal is now a fixed appliance address and the
# interesting question is answered by the service tag. Add \`discover = yes\`
# to bring it back.
portal   = $PORTAL
port     = $PORT
nqn      = $NQN
nsid     = $NSID
api_port = $API_PORT
claim    = yes
CONF
fi

# FAT16 with 512-byte clusters: FAT32 needs ~33 MB of filesystem before it has
# enough clusters to be legal, which is eight times the whole image. FAT16 at
# the default 2 KB cluster size is rejected below 8 MB for the same reason, so
# -s 1 is what makes a 4 MB ESP possible.
ESP="$WORK/esp.img"
truncate -s "${ESP_MIB}M" "$ESP"
mkfs.fat -F 16 -s 1 -n STORMBOOTX "$ESP" >/dev/null
mmd   -i "$ESP" ::/EFI ::/EFI/BOOT ::/stormboot
mcopy -i "$ESP" "$BIN" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$ESP" "$WORK/stormboot.conf" ::/stormboot/stormboot.conf

mkdir -p "$(dirname "$OUTPUT")"
rm -f "$OUTPUT"
truncate -s "$(( ESP_MIB + 2 ))M" "$OUTPUT"
sfdisk --quiet --label gpt "$OUTPUT" <<EOF
start=2048, size=$(( ESP_MIB * 2048 )), type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="STORMBOOTX"
EOF
dd if="$ESP" of="$OUTPUT" bs=1M seek=1 conv=notrunc status=none

say "binary  $(du -h "$BIN" | cut -f1)  $BIN"
say "image   $(du -h "$OUTPUT" | cut -f1)  $OUTPUT"
if [[ "$PROBE" == "yes" ]]; then
    say "boots   tcp4probe — reports whether this firmware carries a TCP/IP stack"
elif [[ "$PIN" == "yes" ]]; then
    say "target  nvme-tcp://$PORTAL:$PORT/$NQN?nsid=$NSID  (pinned, no claim knobs)"
elif [[ "$DISCOVER" == "yes" ]]; then
    say "portal  discovered from _nvme-disc._tcp.$ZONE"
    say "image   claimed as boothost/<service tag> at the portal:$API_PORT"
else
    say "portal  $PORTAL:$PORT  (named, no DNS)"
    say "image   claimed as boothost/<service tag> at $PORTAL:$API_PORT"
    say "        falling back to $NQN?nsid=$NSID if the claim cannot be reached"
fi
cat <<EOF

  Write it:
    dd if=$OUTPUT of=/dev/sdX bs=4M conv=fsync

  Retarget it without rebuilding — mount the ESP and edit
  \stormboot\stormboot.conf, or move the DNS record.
EOF
