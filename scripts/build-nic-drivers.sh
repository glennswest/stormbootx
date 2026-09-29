#!/bin/bash
# Build the NIC UEFI drivers stormbootx loads from its boot media (#26).
#
# Some platforms carry the firmware's whole TCP/IP stack and no UEFI driver for
# their own NICs — the Supermicro X9 blades (server1–8): an Intel 10G with a
# legacy `IBA XE` option ROM and a ConnectX-3 with legacy `FlexBoot`. With no
# EFI_SIMPLE_NETWORK there is nothing for TCP4 to bind to. iPXE, built as an
# EFI driver, provides that SNP; the platform's own MNP/IP4/TCP4 sit on top.
#
#   intelx  -> ipxe-intelx.efi   Intel 82599 / X540 / X552 10G
#   hermon  -> ipxe-hermon.efi   Mellanox ConnectX-3 (15b3:1003); opt-in,
#                                IPXE_DRIVERS="intelx hermon" (hangs server1)
#
# The owner-approved INTERIM (2026-09-27): EFI drivers only, no PXE. The
# long-term driver is a no_std Rust crate from the Intel datasheets (#27).
#
# iPXE is GPL-2 and ships as separate binaries beside stormbootx, with a note
# naming the exact source commit. Pinned: a driver on boot media changes only
# when someone changes IPXE_REF, in its own commit.
#
#   scripts/build-nic-drivers.sh [OUTDIR]     (default tmp/drivers in the checkout)
#   scripts/build-boot-agent.sh --iso --drivers tmp/drivers
#
# What ships is the nic-drivers golden: deploy/build-golden.sh nic-drivers OUT.
#
# Runs ON the build box (dev.g8.lo); needs gcc, make, perl and git.
set -euo pipefail

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# Our own copy (owner, 2026-09-28: "pull the drivers out, and create our own
# git repo"): glennswest/ipxe holds the pinned commit, so a build never reaches
# upstream. The Rust drivers (stormnic-ixgbe, stormnic-mlx4) replace it (#27).
IPXE_REPO="https://github.com/glennswest/ipxe.git"
IPXE_REF="629e28b56c8d61f5c9251114c4d5b390b61857d1"
# hermon is opt-in: on server1 (AMI Aptio 4) it hung the boot in its start or
# its bind, and stays off the media until that is understood (#26).
read -r -a DRIVERS <<< "${IPXE_DRIVERS:-intelx}"
# The Rust drivers (stormbootx#29, #27), each from a pinned commit of its own
# repo, bumped deliberately in its own commit like IPXE_REF. A driver not on
# the media is carried as <name>.efi.off, which stormbootx does not load (it
# loads only \stormboot\drivers\*.efi): a driver that binds the NIC but offers
# no network yet must not take the blades' network away.
# STORMNIC_ON_MEDIA is the list of Rust drivers put on the media as .efi:
# `ixgbe` in place of iPXE's intelx (stormnic-ixgbe#7), `mlx4` for the
# ConnectX-3 (#34). The stormbootx-rustnic media golden is built with
# STORMNIC_ON_MEDIA="ixgbe mlx4" (#45, #34).
STORMNIC_IXGBE_REPO="https://github.com/glennswest/stormnic-ixgbe.git"
# 9476135: PHY and link code matched to its docs/spec/phy.md (stormnic-ixgbe#13:
# SFP+ ID over I2C, NVM init, AUTOC 10G SFI, laser on SDP3; 25 device IDs),
# on top of 884cf18's bring-up in Start (#2) and committed Cargo.lock (#11),
# so it builds --locked (#43, #44, #47).
STORMNIC_IXGBE_REF="9476135ae9621c8b14ebb5da9fac62ffda49ca6e"
STORMNIC_MLX4_REPO="https://github.com/glennswest/stormnic-mlx4.git"
# cef8dc5: firmware bring-up to INIT_HCA and back (stormnic-mlx4#2) and the
# Ethernet data path check (#3), both run inside Start, which then releases
# the NIC and returns UNSUPPORTED (no SNP yet). Up to ~20 s per Ethernet port
# with link. Committed Cargo.lock (#9), so it builds --locked (#34).
STORMNIC_MLX4_REF="cef8dc5ce53560f4b8bc34f75b9b80b1732ec49d"
read -r -a STORMNIC_ON_MEDIA <<< "${STORMNIC_ON_MEDIA:-}"
for d in "${STORMNIC_ON_MEDIA[@]}"; do
    [[ "$d" == ixgbe || "$d" == mlx4 ]] || die "STORMNIC_ON_MEDIA: no Rust driver '$d' (ixgbe, mlx4)"
done
on_media() { local d; for d in "${STORMNIC_ON_MEDIA[@]}"; do [[ "$d" == "$1" ]] && return 0; done; return 1; }
# STORMNIC_CARRY=no skips building the carried `.off` copies when nothing will
# use them: the stormbootx media golden copies only *.efi onto the media.
STORMNIC_CARRY="${STORMNIC_CARRY:-yes}"
if on_media ixgbe; then
    # One driver per NIC: iPXE's intelx would claim the controller first.
    DRIVERS=("${DRIVERS[@]/intelx}")
    read -r -a DRIVERS <<< "${DRIVERS[*]}"
fi
OUTDIR="${1:-$(cd "$(dirname "$0")/.." && pwd)/tmp/drivers}"

for tool in gcc make perl git; do
    command -v "$tool" >/dev/null || die "$tool not installed on the build host"
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# No iPXE driver wanted (STORMNIC_ON_MEDIA=ixgbe with the default list): the
# rustnic media carries no iPXE at all, so nothing is fetched or built (#45).
IPXE_BUILD=yes
[[ ${#DRIVERS[@]} -gt 0 ]] || IPXE_BUILD=no
mkdir -p "$OUTDIR"

if [[ "$IPXE_BUILD" == no ]]; then
    say "iPXE: not built (no iPXE driver wanted)"
    rm -f "$OUTDIR/IPXE-SOURCE.txt"
else
    say "iPXE $IPXE_REF"
    git -C "$WORK" init -q ipxe
    git -C "$WORK/ipxe" fetch -q --depth 1 "$IPXE_REPO" "$IPXE_REF"
    git -C "$WORK/ipxe" checkout -q FETCH_HEAD

    TARGETS=()
    for d in "${DRIVERS[@]}"; do TARGETS+=("bin-x86_64-efi/$d.efidrv"); done
    say "building ${TARGETS[*]}"
    # make writes only to its log, and a minute of silence over sc-build's ssh
    # once ended in "Connection to dev.g8.lo closed by remote host" (#28). A line
    # every 20 s keeps the session visibly alive.
    ( while sleep 20; do say "  still building (${SECONDS}s)"; done ) &
    beat=$!
    rc=0
    make -C "$WORK/ipxe/src" -j"$(nproc)" "${TARGETS[@]}" >"$WORK/make.log" 2>&1 || rc=$?
    kill "$beat" 2>/dev/null; wait "$beat" 2>/dev/null || true
    if [[ $rc -ne 0 ]]; then
        tail -40 "$WORK/make.log" >&2
        die "iPXE build failed"
    fi

    for d in "${DRIVERS[@]}"; do
        cp "$WORK/ipxe/src/bin-x86_64-efi/$d.efidrv" "$OUTDIR/ipxe-$d.efi"
        say "driver  $(du -h "$OUTDIR/ipxe-$d.efi" | cut -f1)  $OUTDIR/ipxe-$d.efi"
    done
    cat > "$OUTDIR/IPXE-SOURCE.txt" <<EOF
The ipxe-*.efi files in this directory are iPXE (https://ipxe.org), built as
UEFI drivers (bin-x86_64-efi/<driver>.efidrv) from

  $IPXE_REPO
  commit $IPXE_REF

and distributed under the GNU General Public License, version 2 (see COPYING
and COPYING.GPLv2 in that source tree). They are separate programs from
stormbootx, which loads them at boot to drive NICs whose firmware has no UEFI
driver.
EOF
fi
# The Rust drivers: stormnic-ixgbe for the blades' Intel 10G (#29),
# stormnic-mlx4 for their ConnectX-3 (#34).
# PE Subsystem of an image; 11 is an EFI boot-service driver, 10 an application.
pe_subsystem() {
    local f=$1 pe
    pe=$(od -An -tu4 -j 60 -N4 "$f" | tr -d ' ')
    od -An -tu2 -j $((pe + 4 + 20 + 68)) -N2 "$f" | tr -d ' '
}
build_stormnic() {
    local name=$1 repo=$2 ref=$3 short=$4 lock sub efi
    if ! on_media "$short" && [[ "$STORMNIC_CARRY" == no ]]; then
        say "$name: not built (STORMNIC_CARRY=no, and it is not on the media)"
        rm -f "$OUTDIR/$name.efi.off"
        return
    fi
    say "$name $ref"
    git -C "$WORK" init -q "$name"
    git -C "$WORK/$name" fetch -q --depth 1 "$repo" "$ref"
    git -C "$WORK/$name" checkout -q FETCH_HEAD
    lock=--locked; [[ -f "$WORK/$name/Cargo.lock" ]] || { lock=""; say "  $name has no Cargo.lock: built unlocked"; }
    # shellcheck disable=SC2086
    ( cd "$WORK/$name" && CARGO_TARGET_DIR="$WORK/$name-target" cargo build -q --release $lock --target x86_64-unknown-uefi ) \
        || die "$name did not build"
    efi="$WORK/$name-target/x86_64-unknown-uefi/release/$name.efi"
    [[ -f "$efi" ]] || die "no $name.efi at $efi"
    # stormbootx loads it with LoadImage/StartImage as a driver; an application
    # image would run and exit, leaving no binding behind.
    sub=$(pe_subsystem "$efi")
    [[ "$sub" == 11 ]] || die "$name.efi has PE subsystem $sub, not 11 (EFI boot-service driver)"
    if on_media "$short"; then
        cp "$efi" "$OUTDIR/$name.efi"
        say "driver  $(du -h "$efi" | cut -f1)  $OUTDIR/$name.efi  (on the media, subsystem $sub)"
    else
        # `.off`: stormbootx loads only names ending in .efi, and the media
        # scripts copy the folder flat, so a plain file is carried, never loaded.
        cp "$efi" "$OUTDIR/$name.efi.off"
        say "driver  $(du -h "$efi" | cut -f1)  $OUTDIR/$name.efi.off  (carried, not loaded; subsystem $sub)"
    fi
    printf '%s %s %s%s\n' "$name" "$ref" "$(sha256sum "$efi" | cut -d' ' -f1)" "${lock:+ locked}" >> "$OUTDIR/STORMNIC-SOURCE.txt"
}
rm -f "$OUTDIR/STORMNIC-SOURCE.txt"
build_stormnic stormnic-ixgbe "$STORMNIC_IXGBE_REPO" "$STORMNIC_IXGBE_REF" ixgbe
build_stormnic stormnic-mlx4 "$STORMNIC_MLX4_REPO" "$STORMNIC_MLX4_REF" mlx4
