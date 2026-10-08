#!/usr/bin/env bash
# Is stormbootx.efi the same bytes wherever it is built? (#53)
#
#   tests/repro.sh [BIN]     (default stormbootx)
#
# Two goldens built from one commit carried two different stormbootx.efi
# digests, because every build gets a fresh drive: the checkout, the target
# dir and CARGO_HOME (the crates' sources) sit at a different path each time.
# This builds BIN twice, from two copies of the checkout at different paths,
# each with its own target dir and CARGO_HOME, the commit stamp the same, and
# requires the two .efi files to be identical. On a difference it prints the
# strings only one of them carries, which name what leaked in.
#
# Builds through scripts/cargo-repro.sh, the one place the path remapping
# lives. REPRO_RAW=1 builds with plain cargo instead, to show the problem.
#
# Unprivileged; everything is under $TMPDIR and deleted on exit.
set -euo pipefail

BIN=${1:-stormbootx}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
say() { printf 'repro: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
STAMP="$(git -C "$ROOT" rev-parse --short HEAD)"
CARGO="$(command -v cargo)"

build() {
    local dir=$1
    mkdir -p "$dir"
    git -C "$ROOT" archive HEAD | tar -x -C "$dir/src"
    local cargo=("$dir/src/scripts/cargo-repro.sh")
    [[ -n "${REPRO_RAW:-}" ]] && cargo=("$CARGO")
    ( cd "$dir/src" && env STORMBOOTX_BUILD="$STAMP" CARGO_HOME="$dir/cargo-home" \
        CARGO_TARGET_DIR="$dir/target" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
        "${cargo[@]}" build --locked --release --target x86_64-unknown-uefi --bin "$BIN" ) >"$dir/build.log" 2>&1 \
        || { cat "$dir/build.log"; die "the build in $dir failed"; }
    cp "$dir/target/x86_64-unknown-uefi/release/$BIN.efi" "$dir.efi"
}

# Different lengths too, so nothing lines up by accident.
mkdir -p "$W/a/src" "$W/a-much-longer-second-build-directory/src"
build "$W/a"
build "$W/a-much-longer-second-build-directory"
A="$W/a.efi" B="$W/a-much-longer-second-build-directory.efi"
da=$(sha256sum < "$A" | cut -d' ' -f1) db=$(sha256sum < "$B" | cut -d' ' -f1)
say "$BIN.efi $(stat -c %s "$A") bytes  $da  (built in $W/a)"
say "$BIN.efi $(stat -c %s "$B") bytes  $db  (built in $W/a-much-longer-…)"
if [[ "$da" != "$db" ]]; then
    say "strings only in one build:"
    diff <(grep -aoE '[[:print:]]{6,}' "$A" | sort -u) <(grep -aoE '[[:print:]]{6,}' "$B" | sort -u) \
        | grep '^[<>]' | head -40 || true
    say "$(cmp -l "$A" "$B" | wc -l) byte(s) differ (offset, octal bytes):"
    cmp -l "$A" "$B" | head -40 | sed 's/^/repro: cmp /' || true
    [[ -n "${REPRO_DUMP:-}" ]] && for f in "$A" "$B"; do
        say "$(basename "$f"): the PE header and the last 512 bytes"
        od -A x -t x1z -j 120 -N 160 "$f"
        local first; first=$(cmp -l "$A" "$B" | awk 'NR==1{next} {print $1; exit}')
        [[ -n "$first" ]] && od -A x -t x1z -j $(( first - 129 )) -N 256 "$f"
    done
    die "$BIN.efi depends on where it was built"
fi
grep -aqF "$W" "$A" && die "$BIN.efi carries the build directory's path"
say "PASS: $BIN.efi is the same bytes from two build directories and two CARGO_HOMEs"
