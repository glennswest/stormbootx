#!/usr/bin/env bash
# A NIC that fails to start is given back to the firmware (#231, 47063dda87c6).
#
#   tests/nicfail-ovmf.sh PAYLOAD.EFI
#
# Builds stormbootx with STORMBOOTX_NET_TEST_FAIL=0 (no golden build sets
# it): nic 0 fails after its exclusive SNP open, Start and Initialize, as a
# driver that will not start would. tests/net-ovmf.sh's `nicfail` boot
# (NICFAIL=1) then needs, with the firmware's own IPv4 stack on: the NIC
# given back (`nic 0: given back to the firmware`), stormbootx with no
# network, and the next boot option, the firmware's PXE on that same NIC,
# leasing from slirp and starting the payload over TFTP. Before #231 the
# open was never closed and the PXE option found its NIC gone.
#
# Run it before tests/update-ovmf.sh (which must stay last): this build
# replaces stormbootx.efi in the target dir. Unprivileged; everything is
# under $TMPDIR and deleted on exit.
set -euo pipefail

PAYLOAD=${1:?usage: $0 PAYLOAD.EFI}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
say() { printf 'nicfail-ovmf: %s\n' "$*"; }
T=$(mktemp -d "${TMPDIR:-/tmp}/nicfail-ovmf.XXXXXX")
trap 'rm -rf "$T"' EXIT

say "building stormbootx with nic 0 failing after its open (STORMBOOTX_NET_TEST_FAIL=0)"
( cd "$ROOT" && STORMBOOTX_NET_TEST_FAIL=0 STORMBOOTX_BUILD=nicfail-test \
    cargo build --locked --release --target x86_64-unknown-uefi --bin stormbootx --bin stormbootx-launch )
R="${CARGO_TARGET_DIR:-$ROOT/target}/x86_64-unknown-uefi/release"
cp "$R/stormbootx.efi" "$T/stormbootx-nicfail.efi"
# The launcher beside it, where build-boot-agent.sh looks for it (#229).
cp "$R/stormbootx-launch.efi" "$T/"

NICFAIL=1 "$ROOT/tests/net-ovmf.sh" "$T/stormbootx-nicfail.efi" "$PAYLOAD"
