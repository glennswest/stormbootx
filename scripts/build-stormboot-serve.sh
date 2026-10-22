#!/usr/bin/env bash
# Build stormblock's `stormboot-serve` (stormblock#474) for the laptop test
# (#164, tests/laptop-ovmf.sh).
#
#   scripts/build-stormboot-serve.sh OUTDIR
#
# It is the target the laptop app (storminstall#20) runs on a Mac or Windows
# machine: one image file over NVMe/TCP, a copy-on-write overlay per claim,
# and the boothost health/intent/claim/installed API. Built here for Linux
# from the pinned commit, `--locked`, so the test boots stormbootx against
# the code the app links, not a stub of it. Writes OUTDIR/stormboot-serve.
#
# STORMBOOT_SERVE_REF moves the pin (a stormblock commit with
# crates/nvmeof-target); bump it deliberately, as a Cargo.lock bump.
set -euo pipefail

OUT=${1:?usage: $0 OUTDIR}
REF=${STORMBOOT_SERVE_REF:-10d3b469434e6ba66e925b874bdf122914abae29}   # stormblock v22.2.0
REPO=${STORMBOOT_SERVE_REPO:-https://github.com/glennswest/stormblock}

say() { printf 'build-stormboot-serve: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
SRC="$OUT/stormblock-src"
rm -rf "$SRC"
git init -q "$SRC"
git -C "$SRC" fetch -q --depth 1 "$REPO" "$REF" || die "could not fetch $REF from $REPO"
git -C "$SRC" checkout -q FETCH_HEAD
say "stormblock $(git -C "$SRC" rev-parse --short=12 HEAD) ($(git -C "$SRC" describe --tags --always 2>/dev/null || true))"

( cd "$SRC" && cargo build --release --locked -p stormblock-nvmeof --bin stormboot-serve ) \
    || die "cargo build of stormboot-serve failed"
TARGET=$(cd "$SRC" && cargo metadata --format-version 1 --no-deps -q \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
install -m 0755 "$TARGET/release/stormboot-serve" "$OUT/stormboot-serve"
rm -rf "$SRC"
say "$OUT/stormboot-serve: $("$OUT/stormboot-serve" --version)"
