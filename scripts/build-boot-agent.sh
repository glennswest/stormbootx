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
# image to boot. That is a fleet decision living next to the images: the agent
# claims boothost/default by the machine's MAC (universal boot, #15), and the
# engine gives each machine its own clone, as mac-<hex> until it is named, or
# the image of the name the MAC is an alias of. Engines older than
# stormblock#200 are claimed by service tag. Moving a machine to another
# version is a PUT on its name; the stick never changes, and one stick boots
# every machine.
#
# --probe builds a diagnostic stick that boots tcp4probe instead of the agent.
#
# Runs ON the build box (dev.g8.lo). Output defaults to tmp/images in the
# checkout, which is on the build's own drive and deleted with it: nothing is
# kept on a build box. What ships is a golden (deploy/build-golden.sh, #21).
# Never /tmp, which on dev is a tmpfs sized at half of RAM.
set -euo pipefail

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

PORTAL="192.168.31.202"                     # forge.g16.lo, eth1 (MTU 9000)
PORT="4420"
NQN="nqn.2026-09.lo.g16:stormcos"
NSID="2"
API_PORT="9090"                              # engine API on the portal host
ESP_MIB="4"
OUTDIR="$(cd "$(dirname "$0")/.." && pwd)/tmp/images"
OUTPUT=""
BIN=""
PIN="no"
PROBE="no"
ISO="no"
FEC=""
DRIVERS=""
DNS=""
NTP=""
MEDIA=""
RNG=""
UPDATE=""
TREE=""

usage() {
    sed -n '2,20p' "$0" | sed 's/^# \?//'
    cat <<'USAGE'

Options:
  --pin            write only the portal, with no claim knobs
  --api-port N     engine API port on the portal host (default 9090)
  --probe          boot tcp4probe instead of the agent (a diagnostic stick)
  --fec MODE       recovery stick: write this FEC to the ConnectX, then reset
                   (default|rs|fc|off|autoneg). Absent from a normal stick.
  --drivers DIR    lay DIR's files in \stormboot\drivers; stormbootx loads each
                   *.efi as a NIC driver (#26; scripts/build-nic-drivers.sh)
  --media LABEL    name this media on the console (`media : LABEL`), e.g.
                   normal, or rustnic ixgbe@563ea8d mlx4@cf37f8b (#45, #34)
  --dns ADDR       DNS server for the PTR of the machine's own address, when
                   its DHCP reply names none or cannot be read (#26)
  --ntp SERVER     NTP server (host[:port], or off) when the lease names none
                   in option 42; default pool.ntp.org (#77)
  --portal ADDR    NVMe/TCP portal, with --pin (default 192.168.31.202)
  --engine ADDR    the portal and engine host, claim still on (tests/net-ovmf.sh)
  --rng FIRST      the first entropy source tried: firmware (default), cpu or
                   jitter (#56); the ones above it are skipped
  --update URL     where the medium updates itself from (#83):
                   http://host[:port]/path to a stormcentral boothelper, or off
  --tree DIR       also lay the ESP's files out in DIR (the golden's media/
                   tree, which stormcentral serves to a self-update)
  --port N         portal port (default 4420)
  --nqn NQN        subsystem NQN (default nqn.2026-09.lo.g16:stormcos)
  --nsid N         namespace (default 2)
  --size MIB       ESP size (default 4; mkfs.fat picks FAT12/16 by size)
  --binary PATH    prebuilt .efi (default: build it)
  --output PATH    image path (default tmp/images/stormbootx.img in the checkout)
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --pin)    PIN="yes"; shift ;;
        --probe)  PROBE="yes"; shift ;;
        --fec)    FEC="$2"; shift 2 ;;
        --iso)    ISO="yes"; shift ;;
        --drivers) DRIVERS="$2"; shift 2 ;;
        --dns)    DNS="$2"; shift 2 ;;
        --ntp)    NTP="$2"; shift 2 ;;
        --media)  MEDIA="$2"; shift 2 ;;
        --api-port) API_PORT="$2"; shift 2 ;;
        --portal) PORTAL="$2"; PIN="yes"; shift 2 ;;
        --engine) PORTAL="$2"; shift 2 ;;
        --rng)    RNG="$2"; shift 2 ;;
        --update) UPDATE="$2"; shift 2 ;;
        --tree)   TREE="$2"; shift 2 ;;
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

if [[ "$ISO" == "yes" ]]; then
    OUTPUT="${OUTPUT:-$OUTDIR/stormbootx.iso}"
else
    OUTPUT="${OUTPUT:-$OUTDIR/stormbootx.img}"
fi
case "$OUTPUT" in
    /tmp/*) die "refusing to write a disk image into /tmp (tmpfs = RAM); use $OUTDIR" ;;
esac

# The ISO wraps the same ESP as an El Torito UEFI boot image (xorriso); the raw
# .img lays it into a GPT partition (sfdisk). Only one of the two is needed.
NEED="sfdisk"
[[ "$ISO" == "yes" ]] && NEED="xorriso"
for tool in mkfs.fat mmd mcopy "$NEED"; do
    command -v "$tool" >/dev/null || die "$tool not installed on the build host"
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

WANT="stormbootx"
[[ "$PROBE" == "yes" ]] && WANT="tcp4probe"

if [[ -z "$BIN" ]]; then
# Stamp the build so the console can name itself. Three different stale sticks
# in one machine produced three plausible-looking boot logs during bring-up, and
# nothing on screen said which binary was talking.
STORMBOOTX_BUILD="$(git -C "$(dirname "$0")/.." rev-parse --short HEAD 2>/dev/null || echo unknown)$(git -C "$(dirname "$0")/.." diff --quiet 2>/dev/null || echo -dirty)"
export STORMBOOTX_BUILD

    say "building $WANT for x86_64-unknown-uefi"
    ( cd "$ROOT" && cargo build --locked --release --target x86_64-unknown-uefi --bin "$WANT" )
    BIN="${CARGO_TARGET_DIR:-$ROOT/target}/x86_64-unknown-uefi/release/$WANT.efi"
fi
[[ -f "$BIN" ]] || die "no $WANT.efi at $BIN"
if [[ -n "$DRIVERS" ]]; then
    compgen -G "$DRIVERS/*.efi" >/dev/null || die "no *.efi in $DRIVERS (run scripts/build-nic-drivers.sh)"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [[ "$PIN" == "yes" ]]; then
    cat > "$WORK/stormboot.conf" <<CONF
# stormbootx — this stick names its portal and nothing else.
#
# No claim knobs: this machine attaches exactly here, on exactly this
# namespace, wherever it is plugged in. That is what --pin is for; the ordinary
# stick claims its image by service tag instead.
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
# stormbootx — the portal is an appliance address, the image is this machine's.
#
# Nothing here says which image to boot. That is a fleet decision and it lives
# next to the images: this machine's boothost synonym on the engine, claimed
# at $PORTAL:$API_PORT as boothost/default by its MAC (or by service tag on an
# engine older than stormblock#200), in one request that answers with a
# copy-on-write clone and the address, NQN and NSID reaching it. Moving this machine to another
# version is a PUT on its name — this stick does not change.
#
# nqn and nsid below are only the fallback, for a claim that cannot be reached:
# an image nobody assigned beats no image.
#
portal   = $PORTAL
port     = $PORT
nqn      = $NQN
nsid     = $NSID
api_port = $API_PORT
claim    = yes
CONF
fi

# A recovery stick, and nothing else. Stated by the operator on this one piece
# of media; the boot path never decides this for itself (see main.rs step 2b).
if [[ -n "$FEC" ]]; then
    cat >> "$WORK/stormboot.conf" <<CONF

# RECOVERY STICK: write this FEC to every ConnectX port, then warm-reset once.
# Remove this line (or use a normal stick) as soon as the card is back.
fec      = $FEC
CONF
fi

# Where the machine asks for its own name when the lease does not say (#26).
if [[ -n "$DNS" ]]; then
    cat >> "$WORK/stormboot.conf" <<CONF

# The DNS server asked for the PTR of this machine's own address, when its
# DHCP reply names none or firmware cannot read the reply back.
dns      = $DNS
CONF
fi

# The NTP server the clock is set from (#77), when the lease has no option 42.
if [[ -n "$NTP" ]]; then
    cat >> "$WORK/stormboot.conf" <<CONF

# The NTP server the hardware clock is set from (host[:port], or off), when
# the DHCP lease names none in option 42. Default pool.ntp.org.
ntp      = $NTP
CONF
fi

# Which variant this is (#45): two media of one commit differ only in their
# NIC drivers, and the console says which one booted.
if [[ -n "$MEDIA" ]]; then
    cat >> "$WORK/stormboot.conf" <<CONF

# Which media this is, printed at boot. Set by the golden build.
media    = $MEDIA
CONF
fi

# The first entropy source tried (#56). Absent on ordinary media: the best one
# present is used. The OVMF test states `cpu` to take the firmware's EFI_RNG
# out of the picture.
if [[ -n "$RNG" ]]; then
    [[ "$RNG" == firmware || "$RNG" == cpu || "$RNG" == jitter ]] || die "--rng: firmware, cpu or jitter"
    cat >> "$WORK/stormboot.conf" <<CONF

# The first entropy source tried; the ones above it are skipped.
rng      = $RNG
CONF
fi

# Where the medium updates itself from (#83). The golden build names its own
# boothelper on stormcentral; a medium built by hand has none and never checks.
if [[ -n "$UPDATE" ]]; then
    cat >> "$WORK/stormboot.conf" <<CONF

# Self-update (#83): the current release of this medium's golden, a signed
# manifest stormbootx verifies before writing anything. "off" pins this
# medium. Machine-local settings go in \stormboot\local.conf, which is read
# first and never updated.
update   = $UPDATE
CONF
fi

# mkfs.fat's own geometry: at 4 MiB that is FAT12 with 2 KiB clusters, the
# same as Debian's efi.img. This was FAT16 at 512-byte clusters (-F 16 -s 1)
# until #55: AMI Aptio 4 (server1, X9) read the first 12 KB of that ESP off
# virtual media and hung at POST A2, where Debian's netinst booted.
ESP="$WORK/esp.img"
truncate -s "${ESP_MIB}M" "$ESP"
mkfs.fat -n STORMBOOTX "$ESP" >/dev/null
mmd   -i "$ESP" ::/EFI ::/EFI/BOOT ::/stormboot
mcopy -i "$ESP" "$BIN" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$ESP" "$WORK/stormboot.conf" ::/stormboot/stormboot.conf
# An EFI Shell fallback starts the agent by itself (#60): a machine with no
# boot option for this media drops to the firmware's shell, which runs
# \startup.nsh. CRLF, as the old EDK shell's scripts are.
sed 's/$/\r/' "$ROOT/media/startup.nsh" > "$WORK/startup.nsh"
mcopy -i "$ESP" "$WORK/startup.nsh" ::/startup.nsh
# NIC drivers for firmware that has none of its own (#26). Loaded from the
# volume that booted, so on an ISO it is the ESP boot image that must carry
# them; the loose ISO9660 copy below is for reading, not for booting.
if [[ -n "$DRIVERS" ]]; then
    mmd -i "$ESP" ::/stormboot/drivers
    mcopy -i "$ESP" "$DRIVERS"/* ::/stormboot/drivers/
fi

# The same files as a tree (#83): what a self-update compares and fetches.
if [[ -n "$TREE" ]]; then
    rm -rf "$TREE"
    mkdir -p "$TREE/EFI/BOOT" "$TREE/stormboot"
    cp "$BIN" "$TREE/EFI/BOOT/BOOTX64.EFI"
    cp "$WORK/stormboot.conf" "$TREE/stormboot/stormboot.conf"
    cp "$WORK/startup.nsh" "$TREE/startup.nsh"
    if [[ -n "$DRIVERS" ]]; then
        mkdir -p "$TREE/stormboot/drivers"
        cp "$DRIVERS"/* "$TREE/stormboot/drivers/"
    fi
fi

mkdir -p "$(dirname "$OUTPUT")"
rm -f "$OUTPUT"

if [[ "$ISO" == "yes" ]]; then
    # An El Torito UEFI ISO for iDRAC/BMC virtual media, where a raw GPT disk is
    # awkward to mount. The ESP is the boot image (no emulation); the .efi and
    # conf are also laid loose in the ISO9660 tree, matching the layout the
    # first hand-built ISO used and firmware is happy to read either way.
    ISOROOT="$WORK/iso"
    mkdir -p "$ISOROOT/EFI/BOOT" "$ISOROOT/stormboot"
    cp "$ESP" "$ISOROOT/esp.img"
    cp "$BIN" "$ISOROOT/EFI/BOOT/BOOTX64.EFI"
    cp "$WORK/stormboot.conf" "$ISOROOT/stormboot/stormboot.conf"
    cp "$WORK/startup.nsh" "$ISOROOT/startup.nsh"
    if [[ -n "$DRIVERS" ]]; then
        mkdir -p "$ISOROOT/stormboot/drivers"
        cp "$DRIVERS"/* "$ISOROOT/stormboot/drivers/"
    fi
    #
    # Isohybrid, the way Debian's netinst is (#55): an MBR whose partition 2
    # (type 0xef) and a GPT that both map the ESP boot image, beside the El
    # Torito catalog. Pure El Torito hung Aptio 4. The MBR template is zeros:
    # the build box has no syslinux, and this media has no BIOS boot code.
    head -c 432 /dev/zero > "$WORK/mbr.bin"
    xorriso -as mkisofs -V STORMBOOTX -isohybrid-mbr "$WORK/mbr.bin" \
        -e esp.img -no-emul-boot -isohybrid-gpt-basdat \
        -o "$OUTPUT" "$ISOROOT" >/dev/null 2>&1
else
    truncate -s "$(( ESP_MIB + 2 ))M" "$OUTPUT"
    sfdisk --quiet --label gpt "$OUTPUT" <<EOF
start=2048, size=$(( ESP_MIB * 2048 )), type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="STORMBOOTX"
EOF
    dd if="$ESP" of="$OUTPUT" bs=1M seek=1 conv=notrunc status=none
fi

say "binary  $(du -h "$BIN" | cut -f1)  $BIN"
[[ -z "$MEDIA" ]] || say "media   $MEDIA"
say "image   $(du -h "$OUTPUT" | cut -f1)  $OUTPUT"
if [[ -n "$DRIVERS" ]]; then
    say "drivers $(cd "$DRIVERS" && ls *.efi | tr '\n' ' ')(\\stormboot\\drivers)"
fi
if [[ "$PROBE" == "yes" ]]; then
    say "boots   tcp4probe — reports whether this firmware carries a TCP/IP stack"
elif [[ "$PIN" == "yes" ]]; then
    say "target  nvme-tcp://$PORTAL:$PORT/$NQN?nsid=$NSID  (pinned, no claim knobs)"
else
    say "portal  $PORTAL:$PORT  (named, no DNS)"
    say "image   claimed as boothost/default by MAC at $PORTAL:$API_PORT (by service tag on older engines)"
    say "        falling back to $NQN?nsid=$NSID if the claim cannot be reached"
fi
cat <<EOF

  Write it:
    dd if=$OUTPUT of=/dev/sdX bs=4M conv=fsync

  Retarget it without rebuilding — mount the ESP and edit
  \stormboot\stormboot.conf. Which image a machine boots is its
  boothost synonym on the engine, not anything on the media.
EOF
