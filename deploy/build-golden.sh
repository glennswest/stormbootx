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
#   SHA256SUMS, BUILD       digests of every file above; commit and inputs
#
#   The media carries \stormboot\drivers: the nic-drivers golden's bin/ when
#   --drivers names it (stormcentral mounts inputs read-only), else the same
#   drivers built here from the same pinned iPXE commit. The console says
#   `media : normal`.
#
# stormbootx-rustnic golden (#45): the same agent with the Rust NIC drivers
# instead of iPXE's, so they are what a machine booting it tests:
#   bin/stormbootx.efi
#   boot/stormbootx-rustnic.iso   carries stormnic-ixgbe.efi (STORMNIC_IXGBE_REF)
#                                 and no iPXE NIC driver
#   SHA256SUMS, BUILD
#
#   Built here from the pins in scripts/build-nic-drivers.sh, never from a
#   nic-drivers golden, so the drivers are the commit's. The console says
#   `media : rustnic ixgbe@<sha>`. stormnic-mlx4 joins it once it binds.
#
# nic-drivers golden:
#   bin/ipxe-intelx.efi     iPXE intelx as an EFI driver (the approved interim, #26)
#   IPXE-SOURCE.txt         the GPL-2 source note
#   SHA256SUMS, BUILD
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

case "$GOLDEN" in
nic-drivers)
    "$ROOT/scripts/build-nic-drivers.sh" "$WORK/drivers"
    mkdir -p "$OUT/bin"
    cp "$WORK/drivers/"*.efi "$OUT/bin/"
    # The Rust drivers ride along as *.efi.off, not loaded unless placed on
    # the media (#29).
    cp "$WORK/drivers/"*.efi.off "$OUT/bin/" 2>/dev/null || true
    cp "$WORK/drivers/IPXE-SOURCE.txt" "$WORK/drivers/STORMNIC-SOURCE.txt" "$OUT/"
    seal "ipxe     = $(sed -n 's/^IPXE_REF="\(.*\)"/\1/p' "$ROOT/scripts/build-nic-drivers.sh")
stormnic = $(cat "$WORK/drivers/STORMNIC-SOURCE.txt")"
    ;;
stormbootx)
    say "building stormbootx and tcp4probe for x86_64-unknown-uefi"
    ( cd "$ROOT" && cargo build --locked --release --target x86_64-unknown-uefi )
    REL="${CARGO_TARGET_DIR:-$ROOT/target}/x86_64-unknown-uefi/release"
    mkdir -p "$OUT/bin" "$OUT/boot"
    cp "$REL/stormbootx.efi" "$REL/tcp4probe.efi" "$OUT/bin/"

    if [[ -n "$DRIVERS" ]]; then
        compgen -G "$DRIVERS/*.efi" >/dev/null || die "no *.efi in $DRIVERS"
        from="the nic-drivers golden at $DRIVERS"
    else
        # Only *.efi reaches the media, so the carried stormnic-ixgbe.efi.off
        # is not built here unless STORMNIC_ON_MEDIA asks for it.
        STORMNIC_CARRY=no "$ROOT/scripts/build-nic-drivers.sh" "$WORK/drivers"
        DRIVERS="$WORK/drivers"
        from="built here, iPXE $(sed -n 's/^IPXE_REF="\(.*\)"/\1/p' "$ROOT/scripts/build-nic-drivers.sh")"
    fi
    # Only the drivers and their source note go on the media.
    mkdir -p "$WORK/media-drivers"
    cp "$DRIVERS"/*.efi "$WORK/media-drivers/"
    for note in "$DRIVERS/IPXE-SOURCE.txt" "$DRIVERS/../IPXE-SOURCE.txt"; do
        if [[ -f "$note" ]]; then cp "$note" "$WORK/media-drivers/"; break; fi
    done

    # The portal and the PTR fallback are forge's network, like the compiled
    # defaults. Which image a machine boots is its boothost on the engine,
    # never the media, so one golden boots every machine.
    agent=(--binary "$OUT/bin/stormbootx.efi" --drivers "$WORK/media-drivers" --dns 192.168.31.252 --media normal)
    "$ROOT/scripts/build-boot-agent.sh" --iso "${agent[@]}" --output "$OUT/boot/stormbootx.iso"
    "$ROOT/scripts/build-boot-agent.sh" "${agent[@]}" --output "$OUT/boot/stormbootx.img"
    "$ROOT/scripts/build-boot-agent.sh" --iso --probe --binary "$OUT/bin/tcp4probe.efi" \
        --output "$OUT/boot/tcp4probe.iso"
    seal "drivers  = $(cd "$WORK/media-drivers" && ls *.efi | tr '\n' ' ')($from)"
    ;;
stormbootx-rustnic)
    [[ -z "$DRIVERS" ]] || die "$GOLDEN builds its own drivers from the commit's pins; no --drivers"
    say "building stormbootx for x86_64-unknown-uefi"
    ( cd "$ROOT" && cargo build --locked --release --target x86_64-unknown-uefi --bin stormbootx )
    REL="${CARGO_TARGET_DIR:-$ROOT/target}/x86_64-unknown-uefi/release"
    mkdir -p "$OUT/bin" "$OUT/boot"
    cp "$REL/stormbootx.efi" "$OUT/bin/"

    # stormnic-ixgbe on the media in place of iPXE's intelx, and no iPXE at all.
    STORMNIC_ON_MEDIA=ixgbe "$ROOT/scripts/build-nic-drivers.sh" "$WORK/drivers"
    [[ -f "$WORK/drivers/stormnic-ixgbe.efi" ]] || die "no stormnic-ixgbe.efi was built"
    ! compgen -G "$WORK/drivers/ipxe-*.efi" >/dev/null || die "an iPXE driver reached the rustnic media"
    mkdir -p "$WORK/media-drivers"
    cp "$WORK/drivers/"*.efi "$WORK/drivers/STORMNIC-SOURCE.txt" "$WORK/media-drivers/"

    ref="$(sed -n 's/^STORMNIC_IXGBE_REF="\(.*\)"/\1/p' "$ROOT/scripts/build-nic-drivers.sh")"
    "$ROOT/scripts/build-boot-agent.sh" --iso --binary "$OUT/bin/stormbootx.efi" \
        --drivers "$WORK/media-drivers" --dns 192.168.31.252 --media "rustnic ixgbe@${ref:0:7}" \
        --output "$OUT/boot/stormbootx-rustnic.iso"
    seal "drivers  = $(cd "$WORK/media-drivers" && ls *.efi | tr '\n' ' ')(built here)
stormnic = $(cat "$WORK/drivers/STORMNIC-SOURCE.txt")"
    ;;
*)
    die "no golden $GOLDEN (stormbootx, stormbootx-rustnic or nic-drivers)"
    ;;
esac
