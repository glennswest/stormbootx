#!/bin/bash
# Build the Rust NIC UEFI drivers stormbootx loads from the rustnic media
# (#26, #29, #34).
#
# Some platforms carry no UEFI driver for their own NICs: the Supermicro X9
# blades (server1-8), an Intel 10G with a legacy `IBA XE` option ROM and a
# ConnectX-3 with legacy `FlexBoot`. With no EFI_SIMPLE_NETWORK stormbootx has
# nothing to run its TCP/IP on (#56). These drivers provide that SNP:
#
#   ixgbe -> stormnic-ixgbe.efi   Intel 82599 / X540 / X552 10G
#   mlx4  -> stormnic-mlx4.efi    Mellanox ConnectX-3
#
# No iPXE (owner on #81, 2026-10-02: "I dont want the ipxe code. Move to
# ours."; #91). A machine whose firmware has its own NIC driver boots the
# stormbootx (fw) medium, which carries no drivers at all (#52).
#
#   scripts/build-nic-drivers.sh [OUTDIR]     (default tmp/drivers in the checkout)
#   scripts/build-boot-agent.sh --iso --drivers tmp/drivers
#
# STORMNIC_DRIVERS (default "ixgbe mlx4") picks which are built. What ships is
# the stormbootx-rustnic medium and the nic-drivers golden
# (deploy/build-golden.sh).
#
# Runs ON the build box (dev.g8.lo); needs git and cargo with the
# x86_64-unknown-uefi target.
set -euo pipefail

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# Each driver from a pinned commit of its own repo, bumped deliberately in its
# own commit: a driver on boot media changes only when someone changes its pin.
STORMNIC_IXGBE_REPO="https://github.com/glennswest/stormnic-ixgbe.git"
# 563ea8d: the 82599 MAC reset's EEMNGCTL.CFG_DONE0 wait is logged, not fatal
# (stormnic-ixgbe#21). On server3 (X9SRD-F, 8086:1557) CFG_DONE0 never set
# (EEMNGCTL 0x80000196) and Start failed; EEC.AUTO_RD and EE_PRES already
# confirm the NVM load, so Start continues (#65).
# 8ea722a: Start on AMI Aptio 4 (stormnic-ixgbe#19). PciIo attribute
# Get/Supported failures are no longer fatal; Enable asks only for supported
# bits, then each bit alone; if Memory Space or Bus Master Enable is still
# clear, both are set by a config write to the command register. server3
# (X9SRD-F) refused the attributes with UNSUPPORTED (#63).
# On top of 0dd4267's SNP on a child handle (#4), 2afd319's rings (#3),
# 9476135's PHY and link code (#13, 25 device IDs) and 884cf18's bring-up
# (#2); builds --locked (#47, #48, #51, #63, #65).
STORMNIC_IXGBE_REF="563ea8d2fc9991b63de51b702ffd6afc4f476e95"
STORMNIC_MLX4_REPO="https://github.com/glennswest/stormnic-mlx4.git"
# 0e50017 (v0.2.3, #66): v0.2.2's link diagnostics (speed, autoneg and
# module from QUERY_PORT at the 5 s link wait and on every link change,
# stormnic-mlx4#15) and a log line for the first own frame the adapter
# loops back (spec §7 item 14, stormnic-mlx4#21). On top of
# cf37f8b (v0.2.1): the UAR's PCI I/O BarIndex is found through
# GetBarAttributes (stormnic-mlx4#15). AMI Aptio 4 numbers BARs, not BAR
# registers, so the hard-coded BarIndex 2 was refused and every doorbell write
# failed UNSUPPORTED (server3); VPI ports are driven as Ethernet. On top of
# 4c2d318's (v0.2.0) SNP on a child handle per Ethernet port with a MAC device
# path (#4): Start keeps the ConnectX-3 (~1.5 s per NIC plus up to 5 s for
# link), no #3 broadcast self-test (cef8dc5 is the pin that checks #1-#3), and
# ExitBootServices stops the device's DMA. Builds --locked (#34, #50, #64).
STORMNIC_MLX4_REF="0e50017e9d1a47efb34ae73c8151ba567b2c0f22"
read -r -a STORMNIC_DRIVERS <<< "${STORMNIC_DRIVERS:-ixgbe mlx4}"
for d in "${STORMNIC_DRIVERS[@]}"; do
    [[ "$d" == ixgbe || "$d" == mlx4 ]] || die "STORMNIC_DRIVERS: no Rust driver '$d' (ixgbe, mlx4)"
done
wanted() { local d; for d in "${STORMNIC_DRIVERS[@]}"; do [[ "$d" == "$1" ]] && return 0; done; return 1; }
OUTDIR="${1:-$(cd "$(dirname "$0")/.." && pwd)/tmp/drivers}"

for tool in git cargo; do
    command -v "$tool" >/dev/null || die "$tool not installed on the build host"
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUTDIR"

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
    if ! wanted "$short"; then
        say "$name: not built (not in STORMNIC_DRIVERS)"
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
    cp "$efi" "$OUTDIR/$name.efi"
    say "driver  $(du -h "$efi" | cut -f1)  $OUTDIR/$name.efi  (subsystem $sub)"
    printf '%s %s %s%s\n' "$name" "$ref" "$(sha256sum "$efi" | cut -d' ' -f1)" "${lock:+ locked}" >> "$OUTDIR/STORMNIC-SOURCE.txt"
}
# An OUTDIR from before #91 may still hold iPXE; nothing here may carry it on.
rm -f "$OUTDIR/STORMNIC-SOURCE.txt" "$OUTDIR/IPXE-SOURCE.txt" "$OUTDIR"/ipxe-*.efi "$OUTDIR"/*.efi.off
build_stormnic stormnic-ixgbe "$STORMNIC_IXGBE_REPO" "$STORMNIC_IXGBE_REF" ixgbe
build_stormnic stormnic-mlx4 "$STORMNIC_MLX4_REPO" "$STORMNIC_MLX4_REF" mlx4
