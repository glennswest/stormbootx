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
#   scripts/build-nic-drivers.sh [OUTDIR]     (default /build/images/drivers)
#   scripts/build-boot-agent.sh --iso --drivers /build/images/drivers
#
# Runs ON the build box (dev.g8.lo); needs gcc, make, perl and git.
set -euo pipefail

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

IPXE_REPO="https://github.com/ipxe/ipxe.git"
IPXE_REF="629e28b56c8d61f5c9251114c4d5b390b61857d1"
# hermon is opt-in: on server1 (AMI Aptio 4) it hung the boot in its start or
# its bind, and stays off the media until that is understood (#26).
read -r -a DRIVERS <<< "${IPXE_DRIVERS:-intelx}"
OUTDIR="${1:-/build/images/drivers}"

for tool in gcc make perl git; do
    command -v "$tool" >/dev/null || die "$tool not installed on the build host"
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

say "iPXE $IPXE_REF"
git -C "$WORK" init -q ipxe
git -C "$WORK/ipxe" fetch -q --depth 1 "$IPXE_REPO" "$IPXE_REF"
git -C "$WORK/ipxe" checkout -q FETCH_HEAD

TARGETS=()
for d in "${DRIVERS[@]}"; do TARGETS+=("bin-x86_64-efi/$d.efidrv"); done
say "building ${TARGETS[*]}"
make -C "$WORK/ipxe/src" -j"$(nproc)" "${TARGETS[@]}" >"$WORK/make.log" 2>&1 || {
    tail -40 "$WORK/make.log" >&2
    die "iPXE build failed"
}

mkdir -p "$OUTDIR"
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
