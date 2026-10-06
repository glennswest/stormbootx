#!/usr/bin/env bash
# The self-update under OVMF (#83).
#
#   tests/update-ovmf.sh PAYLOAD.EFI
#
# Makes an Ed25519 test key, builds stormbootx with its public half compiled
# in (STORMBOOTX_UPDATE_TEST_KEY; no golden build sets it), and runs
# tests/net-ovmf.sh's stubs with UPDATE_KEY set, which boots the medium (a
# USB stick) ten times through tests/update-boots.sh: stormcentral's own
# signed manifest, a bad signature, an update and its trial, a current boot,
# canaries, a retired driver, a medium short of room, and a release that
# cannot attach failing its trial twice and being put back.
#
# Run it last in a build: the test-key build replaces stormbootx.efi in the
# target dir. Needs what tests/net-ovmf.sh needs, plus openssl (Ed25519) and
# sfdisk. Unprivileged; everything is under $TMPDIR and deleted on exit.
set -euo pipefail

PAYLOAD=${1:?usage: $0 PAYLOAD.EFI}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
say() { printf 'update-ovmf: %s\n' "$*"; }
command -v openssl >/dev/null || { say "FAIL: openssl is not installed"; exit 1; }

T=$(mktemp -d "${TMPDIR:-/tmp}/update-ovmf.XXXXXX")
trap 'rm -rf "$T"' EXIT
openssl genpkey -algorithm ed25519 -out "$T/key.pem"
PUB=$(openssl pkey -in "$T/key.pem" -pubout -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n')
[[ ${#PUB} -eq 64 ]] || { say "FAIL: no Ed25519 public key ($PUB)"; exit 1; }
say "test key $PUB"

say "building stormbootx with the test key compiled in"
( cd "$ROOT" && STORMBOOTX_UPDATE_TEST_KEY=$PUB STORMBOOTX_BUILD=update-test \
    cargo build --locked --release --target x86_64-unknown-uefi --bin stormbootx )
cp "${CARGO_TARGET_DIR:-$ROOT/target}/x86_64-unknown-uefi/release/stormbootx.efi" "$T/stormbootx-testkey.efi"

UPDATE_KEY="$T/key.pem" "$ROOT/tests/net-ovmf.sh" "$T/stormbootx-testkey.efi" "$PAYLOAD"
