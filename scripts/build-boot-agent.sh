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
# By default the stick names no target at all: it discovers the portal from the
# network's own resolver (SRV/TXT _nvme-disc._tcp), so one image works on every
# network and a portal that moves is a zone edit rather than a visit to every
# machine. --pin writes the portal into \stormboot\stormboot.conf for a box
# that must attach somewhere specific; --probe builds a diagnostic stick that
# boots tcp4probe instead of the agent.
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
  --pin            write the portal into the config, disabling discovery
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
else
    cat > "$WORK/stormboot.conf" <<CONF
# stormbootx — this stick names no target and discovers one.
#
# It asks whichever resolver DHCP hands it for the SRV and TXT records at
# _nvme-disc._tcp.$ZONE, so the same image boots on every network and a portal
# that moves is a zone edit rather than a visit to every machine. Publish the
# records with scripts/publish-portal-dns.sh.
#
# To pin this machine instead, add a portal line — that turns discovery off:
#   portal = $PORTAL
#
# port, nqn and nsid may be set here on their own: they override discovery
# field by field without pinning the address.
zone     = $ZONE
discover = yes
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
    say "target  nvme-tcp://$PORTAL:$PORT/$NQN?nsid=$NSID  (pinned)"
else
    say "target  discovered from _nvme-disc._tcp.$ZONE"
fi
cat <<EOF

  Write it:
    dd if=$OUTPUT of=/dev/sdX bs=4M conv=fsync

  Retarget it without rebuilding — mount the ESP and edit
  \stormboot\stormboot.conf, or move the DNS record.
EOF
