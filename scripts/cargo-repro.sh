#!/usr/bin/env bash
# cargo, with the build's paths taken out of the binary (#53).
#
#   scripts/cargo-repro.sh build --release --target x86_64-unknown-uefi …
#
# Every build gets a fresh drive, so the checkout, the target dir and
# CARGO_HOME (where the crates' sources are unpacked) are at a different path
# each time, and rustc writes source paths into panic locations, which
# `strip` does not touch. Two goldens from one commit then carried two
# stormbootx.efi digests. Each of those prefixes is remapped to a fixed name,
# so the same commit gives the same bytes wherever it is built, which the
# self-update and any "same agent?" check across goldens rely on.
# tests/repro.sh is the proof.
#
# Arguments go to cargo unchanged. RUSTFLAGS from the caller is kept.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CHOME="${CARGO_HOME:-$HOME/.cargo}"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"

# When several match, rustc takes the last, so the narrowest goes last: the
# default target dir is inside the checkout.
flags=(
    "--remap-path-prefix=$ROOT=/stormbootx"
    "--remap-path-prefix=$CHOME=/cargo"
    "--remap-path-prefix=$TARGET=/target"
)
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }${flags[*]}"
exec cargo "$@"
