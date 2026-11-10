#!/usr/bin/env bash
# Build stormdist (glennswest/stormdist) for the stormdist medium test (#198,
# tests/dist-ovmf.sh).
#
#   scripts/build-stormdist.sh OUTDIR
#
# `stormdist make` is what writes a release's full ISO (stormdist#10: El
# Torito UEFI/BIOS and a hybrid GPT over boot/esp.img, BOOT.toml and BOOT.sig
# signed with the release key), so the test boots stormbootx against a medium
# the real writer made, not one shaped like it. Built from the pinned commit,
# `--locked`. Writes OUTDIR/stormdist.
#
# STORMDIST_REF moves the pin; bump it deliberately, as a Cargo.lock bump.
set -euo pipefail

OUT=${1:?usage: $0 OUTDIR}
REF=${STORMDIST_REF:-224126e098c54d482b2b1bd1244262dbddb8645a}   # stormdist v0.5.0
REPO=${STORMDIST_REPO:-https://github.com/glennswest/stormdist}

say() { printf 'build-stormdist: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
# Outside the checkout: under it, cargo would take stormdist for a member of
# stormbootx's workspace ("believes it's in a workspace when it's not").
SRC=$(mktemp -d "${TMPDIR:-/tmp}/stormdist-src.XXXXXX")
git init -q "$SRC"
git -C "$SRC" fetch -q --depth 1 "$REPO" "$REF" || die "could not fetch $REF from $REPO"
git -C "$SRC" checkout -q FETCH_HEAD
say "stormdist $(git -C "$SRC" rev-parse --short=12 HEAD)"

( cd "$SRC" && cargo build --release --locked --bin stormdist ) || die "cargo build of stormdist failed"
TARGET=$(cd "$SRC" && cargo metadata --format-version 1 --no-deps -q \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
install -m 0755 "$TARGET/release/stormdist" "$OUT/stormdist"
rm -rf "$SRC"
say "$OUT/stormdist"
