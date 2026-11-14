#!/bin/bash
# Build the Rust NIC UEFI drivers stormbootx loads from the rustnic media
# (#26, #29, #34, #75).
#
# Some platforms carry no UEFI driver for their own NICs: the Supermicro X9
# blades (server1-8), an Intel 10G with a legacy `IBA XE` option ROM and a
# ConnectX-3 with legacy `FlexBoot`. With no EFI_SIMPLE_NETWORK stormbootx has
# nothing to run its TCP/IP on (#56). These drivers provide that SNP:
#
#   ixgbe -> stormnic-ixgbe.efi   Intel 82599 / X540 / X552 10G
#   mlx4  -> stormnic-mlx4.efi    Mellanox ConnectX-3
#   virtio -> stormnic-virtio.efi virtio-net (VMs: pve, OVMF), bound only
#                                 with `prefer_media_drivers = virtio` (#108)
#   mlx5  -> stormnic-mlx5.efi    Mellanox ConnectX-4/5/6, only when asked for:
#                                 the rustnic media ask (#202), with
#                                 `prefer_media_drivers = virtio,mlx5`
#   e1000e -> stormnic-e1000e.efi Intel 82574L, 82579, I217/I218/I219 (#70);
#                                 binds a NIC nothing else drives, and takes one
#                                 only with `prefer_media_drivers = e1000e`
#   igb   -> stormnic-igb.efi     Intel 82575/82576/82580, I350/I354, I210/I211
#                                 (#69); binds a NIC nothing else drives, and
#                                 there is no key to take one
#
# No iPXE (owner on #81, 2026-10-02: "I dont want the ipxe code. Move to
# ours."; #91). A machine whose firmware has its own NIC driver boots the
# stormbootx (fw) medium, which carries no drivers at all (#52).
#
#   scripts/build-nic-drivers.sh [OUTDIR]     (default tmp/drivers in the checkout)
#   scripts/build-boot-agent.sh --iso --drivers tmp/drivers
#
# STORMNIC_DRIVERS (default "ixgbe mlx4 virtio e1000e igb") picks which are built. What ships is
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
# 4b4813d (#192): jumbo frames (stormnic-ixgbe#33): the SNP reports
# MaxPacketSize 9000, Rings::start sets MAXFRS.MFS 9022 and HLREG0.JUMBOEN,
# and the RX/TX buffers are 9 KB (DMA region 145 pages, was 33), so a g16
# lease offering DHCP option 26 runs the 82599 at 9000 (#165). Also the uefi
# crate's log capped at WARN (INFO when verbose, stormnic-ixgbe#8).
# 728b328 (#101): a quiet console by default (stormnic-ixgbe#22): one line per
# bound NIC (MAC, link speed or `link down after N ms (LINKS, AUTOC, AUTOC2,
# ESDP)`, SNP installed) plus warnings, and the full trace only when the
# volatile `StormnicVerbose` variable is set (stormbootx#102, open);
# link-down diagnostics (stormnic-ixgbe#26); X540/X552/X550/X553 Start says
# "verified in simulation only" (stormnic-ixgbe#17, #23); the DMA check
# listens 3 s for a received frame only when verbose (stormnic-ixgbe#24), so
# a linked boot is 3 s faster. Nothing else changes for the
# 82599 blades.
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
STORMNIC_IXGBE_REF="4b4813dfea457a53b68fdc5a438125ae94c50795"
STORMNIC_MLX4_REPO="https://github.com/glennswest/stormnic-mlx4.git"
# f3d3bc2 (v0.2.8, #195): a ConnectX-3 whose PCI I/O GetLocation fails is
# driven with `?` as its location, the failure printed (stormnic-mlx4#7), and
# Start prints a failed PCI ID read. v0.2.7 (e961b52): the quiet console's one
# line per port really is one (the link coming up during the bring-up wait is
# trace only, stormnic-mlx4#16). v0.2.6: a byte-reproducible image
# (stormnic-mlx4#13). On top of
# 04e7d2c (v0.2.5, #104): a quiet console (stormnic-mlx4#16), one line per
# port (MAC, link up 10G XFI or no link, SNP installed) plus every warning and
# error, the last 16 trace lines replayed ahead of a failure, and the full
# trace only when the volatile `StormnicVerbose` variable is set (stormbootx
# #102, open). v0.2.4 (32083cd): after the link wait and at every link-down,
# each port's module EEPROM through MAD_IFC, its PTYS link-mode masks (one
# read-only ACCESS_REG query) and whether the card offers a forced speed
# (firmware 2.30.8000 does not); nothing new is written (stormnic-mlx4#17).
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
STORMNIC_MLX4_REF="f3d3bc23b8007739efcd6ab3f232cce652e24303"
STORMNIC_VIRTIO_REPO="https://github.com/glennswest/stormnic-virtio.git"
# 1a04808 (v0.1.0, #75): virtio-net (virtio 1.x PCI, 1af4:1041 and the
# transitional 1af4:1000), verified under OVMF and on pve (stormnic-virtio#1).
# It holds PciIo BY_DRIVER | EXCLUSIVE, so a NIC's iPXE option ROM can't
# force it off, and like the others declines a function already held. Every
# OVMF's VirtioNetDxe holds the NIC before the media drivers load, so it binds
# only where the media says `prefer_media_drivers = virtio` (#108).
STORMNIC_VIRTIO_REF="1a04808aea492e508c0c66d4134b06532405e0b8"
STORMNIC_MLX5_REPO="https://github.com/glennswest/stormnic-mlx5.git"
# 66de545 (#131, #73): the ConnectX-4/4 Lx/5/6 driver, reproducible and
# --locked, not yet seen on a card (stormnic-mlx5#1). Not in this script's
# default set; build-golden.sh's rustnic media name it (#202). FlexBoot holds
# the card, so it binds only where the media says `prefer_media_drivers = mlx5`.
STORMNIC_MLX5_REF="66de5456520e40bda00cbbfc0e03e1f7e5fa6b2f"
STORMNIC_E1000E_REPO="https://github.com/glennswest/stormnic-e1000e.git"
# d0bc5ec (v0.1.0, #70, stormnic-e1000e#1): SNP for the 82574L and the PCH
# 82579/I217/I218/I219, 67 device IDs listed from pci.ids 2026.11.06
# (src/ids.rs). Holds PciIo BY_DRIVER | EXCLUSIVE and declines a function
# another driver holds, one console line per NIC, quiesced at every exit and
# at ExitBootServices. Verified on QEMU's e1000e (8086:10d3) under OVMF, alone
# and taken from iPXE's ROM; the PCH parts in simulation only (their line says
# so), and Linux's I219 work-arounds (ULP exit, ring flush) are not done.
# Intel's UNDI (or iPXE's ROM) holds these NICs on most machines, so on the
# media it drives only a NIC nothing else does, unless the media says
# `prefer_media_drivers = e1000e`; no golden says it.
STORMNIC_E1000E_REF="d0bc5ecb6280c33cc0600f51b98c7c6c5ae954d8"
STORMNIC_IGB_REPO="https://github.com/glennswest/stormnic-igb.git"
# 80c9ea5 (v0.1.0, #69, stormnic-igb#1): SNP for 36 igb PF IDs (82575, 82576,
# 82580, I350, I354, I210, I211). Declines a function another driver holds
# BY_DRIVER, one console line per NIC. Verified on QEMU's igb (82576,
# 8086:10c9) under OVMF; never on igb silicon (none in the lab), so it drives
# only an igb nothing else does: there is no `prefer_media_drivers = igb`.
STORMNIC_IGB_REF="80c9ea5b341b2b4d59b4bc13f9c3160180c0a65a"
read -r -a STORMNIC_DRIVERS <<< "${STORMNIC_DRIVERS:-ixgbe mlx4 virtio e1000e igb}"
for d in "${STORMNIC_DRIVERS[@]}"; do
    [[ "$d" == ixgbe || "$d" == mlx4 || "$d" == virtio || "$d" == mlx5 || "$d" == e1000e || "$d" == igb ]] \
        || die "STORMNIC_DRIVERS: no Rust driver '$d' (ixgbe, mlx4, virtio, mlx5, e1000e, igb)"
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
# stormnic-mlx4 for their ConnectX-3 (#34), stormnic-virtio for VMs (#75).
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
    # The same bytes from every build (#130, as stormbootx's own since #53):
    # the checkout, target dir and CARGO_HOME remapped out of panic strings,
    # and lld-link's time stamp and PDB GUID off. RUSTFLAGS reaches only the
    # target with --target, never a build script; no driver sets rustflags.
    local chome="${CARGO_HOME:-$HOME/.cargo}"
    local flags="--remap-path-prefix=$chome=/cargo --remap-path-prefix=$WORK/$name=/$name"
    flags+=" --remap-path-prefix=$WORK/$name-target=/target -C link-arg=/Brepro -C link-arg=/DEBUG:NONE"
    ( cd "$WORK/$name" && CARGO_TARGET_DIR="$WORK/$name-target" RUSTFLAGS="$flags" \
        cargo build -q --release $lock --target x86_64-unknown-uefi ) \
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
    case $short in
        e1000e) same_ids "$name" "$WORK/$name/src/ids.rs" 'device: 0x[0-9a-fA-F]{4}' E1000E_IDS ;;
        ixgbe) same_ids "$name" "$WORK/$name/src/ids.rs" 'device: 0x[0-9a-fA-F]{4}' IXGBE_IDS ;;
        igb) same_ids "$name" "$WORK/$name/src/ids.rs" 'n\(0x[0-9a-fA-F]{4}' IGB_IDS ;;
        mlx4) same_ids "$name" "$WORK/$name/src/main.rs" '^ *\(0x[0-9a-fA-F]{4}, "|\[\(0x[0-9a-fA-F]{4}, "|\), \(0x[0-9a-fA-F]{4}, "' MLX4_IDS ;;
        mlx5) same_ids "$name" "$WORK/$name/src/main.rs" '^ *\(0x[0-9a-fA-F]{4}, "' MLX5_TAKE_OVER ;;
    esac
}
# stormbootx's take-over (src/drivers.rs, #202) takes the functions its lists
# name; each driver binds the ones its own list names. A pin that changes one
# must change the other (#70), so the build fails when they differ.
# same_ids NAME FILE PATTERN CONST: PATTERN finds each device id in FILE.
same_ids() {
    local name=$1 file=$2 pat=$3 const=$4 theirs ours
    theirs=$(grep -oE "$pat" "$file" | grep -oE '0x[0-9a-fA-F]{4}' | sed 's/0x//' | tr A-F a-f | LC_ALL=C sort -u)
    ours=$(sed -n "/^const $const/,/^\];/p" "$(dirname "$0")/../src/drivers.rs" \
        | grep -oE '0x(8086|15b3), 0x[0-9a-fA-F]{4}' | sed 's/.*0x//' | tr A-F a-f | LC_ALL=C sort -u)
    [[ -n "$theirs" && "$theirs" == "$ours" ]] || die "$name's device ids and src/drivers.rs $const differ:
$(diff <(printf '%s\n' "$theirs") <(printf '%s\n' "$ours") | grep '^[<>]')"
    say "  $name: $(wc -l <<< "$theirs") device IDs, the same as src/drivers.rs $const"
}
# An OUTDIR from before #91 may still hold iPXE; nothing here may carry it on.
rm -f "$OUTDIR/STORMNIC-SOURCE.txt" "$OUTDIR/IPXE-SOURCE.txt" "$OUTDIR"/ipxe-*.efi "$OUTDIR"/*.efi.off
build_stormnic stormnic-ixgbe "$STORMNIC_IXGBE_REPO" "$STORMNIC_IXGBE_REF" ixgbe
build_stormnic stormnic-mlx4 "$STORMNIC_MLX4_REPO" "$STORMNIC_MLX4_REF" mlx4
build_stormnic stormnic-virtio "$STORMNIC_VIRTIO_REPO" "$STORMNIC_VIRTIO_REF" virtio
build_stormnic stormnic-mlx5 "$STORMNIC_MLX5_REPO" "$STORMNIC_MLX5_REF" mlx5
build_stormnic stormnic-e1000e "$STORMNIC_E1000E_REPO" "$STORMNIC_E1000E_REF" e1000e
build_stormnic stormnic-igb "$STORMNIC_IGB_REPO" "$STORMNIC_IGB_REF" igb
