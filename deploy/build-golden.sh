#!/bin/bash
# Build one of stormbootx's goldens into OUT, and nothing else.
#
#   deploy/build-golden.sh stormbootx         OUT [--drivers DIR]
#   deploy/build-golden.sh stormbootx-rustnic OUT
#   deploy/build-golden.sh nic-drivers        OUT
#
# Everything is a golden (owner, 2026-09-28; #21, stormcentral#126): the boot
# media is a golden, and so are the NIC drivers it carries. This script is the
# repo's half of that. It compiles and writes files into OUT, which the caller
# provides (stormcentral mounts the golden's volume there). It does no platform
# work (stormcentral#128): no forge, no device, no stormblock tool. It leaves
# nothing outside OUT and its own temp dirs, because nothing is kept on a
# build box.
#
# stormbootx golden:
#   bin/stormbootx.efi      the agent (\EFI\BOOT\BOOTX64.EFI on the media)
#   bin/tcp4probe.efi       the per-model diagnostic
#   boot/stormbootx.iso     El Torito UEFI ISO, for BMC virtual media
#   boot/stormbootx.img     GPT disk image, for a USB stick
#   boot/tcp4probe.iso      a diagnostic ISO that boots tcp4probe
#   media/                  the medium's files as a tree (#83): EFI/BOOT/BOOTX64.EFI,
#                           stormboot/stormboot.conf, stormboot/drivers/*, startup.nsh
#   media.files             one `<sha256> <size> <path>` line per media/ file: the
#                           `file` lines of the self-update manifest stormcentral
#                           signs when it promotes this golden (stormcentral#279)
#   SHA256SUMS, BUILD       digests of every file above; commit and inputs
#
#   The firmware-drivers medium (#52, owner on #81): no \stormboot\drivers,
#   so every NIC is the firmware's own UEFI driver's (the Dell R230). The
#   console says `media : fw` and `drivers : none on the media`. A --drivers
#   (stormcentral's old nic-drivers input) is ignored. Machines whose
#   firmware has no driver for their NICs (the X9 blades) boot
#   stormbootx-rustnic.
#
# stormbootx-rustnic golden (#45): the same agent with the Rust NIC drivers,
# for machines whose firmware has no UEFI driver for their NICs (the X9 blades):
#   bin/stormbootx.efi
#   boot/stormbootx-rustnic.iso   carries stormnic-ixgbe.efi (STORMNIC_IXGBE_REF)
#                                 and stormnic-mlx4.efi (STORMNIC_MLX4_REF, #34),
#                                 and no iPXE NIC driver
#   media/, media.files           as above (#83)
#   SHA256SUMS, BUILD
#
#   Built here from the pins in scripts/build-nic-drivers.sh, never from a
#   nic-drivers golden, so the drivers are the commit's. The console says
#   `media : rustnic ixgbe@<sha> mlx4@<sha>`.
#
# nic-drivers golden (an EFI boothelper; no medium takes it as an input since
# #52):
#   bin/stormnic-ixgbe.efi, bin/stormnic-mlx4.efi
#                           the Rust drivers the rustnic medium carries (#29, #34)
#   STORMNIC-SOURCE.txt     each Rust driver's commit and digest
#   SHA256SUMS, BUILD
#   No iPXE, in this golden or any other (owner on #81; #91).
set -euo pipefail

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

GOLDEN="${1:-}"; OUT="${2:-}"
[[ -n "$GOLDEN" && -n "$OUT" ]] || die "usage: $0 stormbootx|stormbootx-rustnic|nic-drivers OUT [--drivers DIR]"
shift 2
DRIVERS=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --drivers) DRIVERS="$2"; shift 2 ;;
        *) die "unknown argument: $1" ;;
    esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
[[ -z "$(ls -A "$OUT")" ]] || die "$OUT is not empty; a golden is written into an empty tree"

COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
git -C "$ROOT" diff --quiet || die "the checkout has uncommitted changes; a golden is built from a commit"
export STORMBOOTX_BUILD="${COMMIT:0:7}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The digests and the record, written last, over whatever the golden holds.
seal() {
    ( cd "$OUT" && find . -type f ! -name SHA256SUMS ! -name BUILD | sed 's|^\./||' | sort \
        | xargs -r sha256sum ) > "$WORK/SHA256SUMS"
    mv "$WORK/SHA256SUMS" "$OUT/SHA256SUMS"
    {
        echo "golden   = $GOLDEN"
        echo "repo     = glennswest/stormbootx"
        echo "commit   = $COMMIT"
        echo "version  = $(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
        if [[ -n "${1:-}" ]]; then echo "$1"; fi
    } > "$OUT/BUILD"
    say "golden $GOLDEN: $(find "$OUT" -type f | wc -l) files, $(du -sh "$OUT" | cut -f1)"
    cat "$OUT/SHA256SUMS"
}

# Where a medium of this golden updates itself from (#83): stormcentral's
# open boothelper route for the golden, which serves the current promoted,
# signed release (stormcentral#279).
update_url() { echo "http://stormcentral.g8.lo/api/v1/boothelpers/$1"; }

# media.files: the self-update manifest's file lines, from the media/ tree.
media_files() {
    ( cd "$OUT/media" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | while read -r f; do
        printf '%s %s %s\n' "$(sha256sum < "$f" | cut -d' ' -f1)" "$(stat -c %s "$f")" "$f"
    done ) > "$OUT/media.files"
}

case "$GOLDEN" in
nic-drivers)
    "$ROOT/scripts/build-nic-drivers.sh" "$WORK/drivers"
    for d in stormnic-ixgbe stormnic-mlx4; do
        [[ -f "$WORK/drivers/$d.efi" ]] || die "no $d.efi was built"
    done
    mkdir -p "$OUT/bin"
    cp "$WORK/drivers/"*.efi "$OUT/bin/"
    cp "$WORK/drivers/STORMNIC-SOURCE.txt" "$OUT/"
    ! compgen -G "$OUT/bin/ipxe-*" >/dev/null || die "an iPXE driver reached the nic-drivers golden"
    seal "stormnic = $(cat "$WORK/drivers/STORMNIC-SOURCE.txt")"
    ;;
stormbootx)
    say "building stormbootx and tcp4probe for x86_64-unknown-uefi"
    ( cd "$ROOT" && cargo build --locked --release --target x86_64-unknown-uefi )
    REL="${CARGO_TARGET_DIR:-$ROOT/target}/x86_64-unknown-uefi/release"
    mkdir -p "$OUT/bin" "$OUT/boot"
    cp "$REL/stormbootx.efi" "$REL/tcp4probe.efi" "$OUT/bin/"

    # No NIC drivers on this medium (#52): the firmware's own bind every NIC.
    [[ -z "$DRIVERS" ]] || say "ignoring --drivers $DRIVERS: the fw medium carries no NIC drivers (#52)"

    # The portal and the PTR fallback are forge's network, like the compiled
    # defaults. Which image a machine boots is its boothost on the engine,
    # never the media, so one golden boots every machine.
    agent=(--binary "$OUT/bin/stormbootx.efi" --dns 192.168.31.252 --media fw
           --update "$(update_url stormbootx)")
    "$ROOT/scripts/build-boot-agent.sh" --iso "${agent[@]}" --output "$OUT/boot/stormbootx.iso"
    "$ROOT/scripts/build-boot-agent.sh" "${agent[@]}" --tree "$OUT/media" --output "$OUT/boot/stormbootx.img"
    media_files
    "$ROOT/scripts/build-boot-agent.sh" --iso --probe --binary "$OUT/bin/tcp4probe.efi" \
        --output "$OUT/boot/tcp4probe.iso"
    [[ ! -e "$OUT/media/stormboot/drivers" ]] || die "NIC drivers reached the fw medium"
    seal "drivers  = none (the firmware's own, #52)"
    ;;
stormbootx-rustnic)
    [[ -z "$DRIVERS" ]] || die "$GOLDEN builds its own drivers from the commit's pins; no --drivers"
    say "building stormbootx for x86_64-unknown-uefi"
    ( cd "$ROOT" && cargo build --locked --release --target x86_64-unknown-uefi --bin stormbootx )
    REL="${CARGO_TARGET_DIR:-$ROOT/target}/x86_64-unknown-uefi/release"
    mkdir -p "$OUT/bin" "$OUT/boot"
    cp "$REL/stormbootx.efi" "$OUT/bin/"

    # stormnic-ixgbe for the Intel 10G, stormnic-mlx4 for the ConnectX-3
    # (#34), and no iPXE at all (#91).
    "$ROOT/scripts/build-nic-drivers.sh" "$WORK/drivers"
    for d in stormnic-ixgbe stormnic-mlx4; do
        [[ -f "$WORK/drivers/$d.efi" ]] || die "no $d.efi was built"
    done
    ! compgen -G "$WORK/drivers/ipxe-*.efi" >/dev/null || die "an iPXE driver reached the rustnic media"
    mkdir -p "$WORK/media-drivers"
    cp "$WORK/drivers/"*.efi "$WORK/drivers/STORMNIC-SOURCE.txt" "$WORK/media-drivers/"

    pin() { sed -n "s/^$1=\"\(.......\).*\"/\1/p" "$ROOT/scripts/build-nic-drivers.sh"; }
    "$ROOT/scripts/build-boot-agent.sh" --iso --binary "$OUT/bin/stormbootx.efi" \
        --drivers "$WORK/media-drivers" --dns 192.168.31.252 \
        --media "rustnic ixgbe@$(pin STORMNIC_IXGBE_REF) mlx4@$(pin STORMNIC_MLX4_REF)" \
        --update "$(update_url stormbootx-rustnic)" --tree "$OUT/media" \
        --output "$OUT/boot/stormbootx-rustnic.iso"
    media_files
    seal "drivers  = $(cd "$WORK/media-drivers" && ls *.efi | tr '\n' ' ')(built here)
stormnic = $(cat "$WORK/drivers/STORMNIC-SOURCE.txt")"
    ;;
*)
    die "no golden $GOLDEN (stormbootx, stormbootx-rustnic or nic-drivers)"
    ;;
esac
