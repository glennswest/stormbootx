#!/usr/bin/env bash
# Is stormbootx.efi the same bytes wherever it is built? (#53)
#
#   tests/repro.sh [BIN]     (default stormbootx)
#   GOLDEN_EFI=t/g/bin/stormbootx.efi tests/repro.sh
#                            … and a golden built in the checkout must match
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
#   REPRO_MEDIA="stormbootx stormbootx-rustnic-disk" tests/repro.sh
#                            … and each golden built twice, from two clones
#
# The media goldens too (#130): each named golden is built by
# deploy/build-golden.sh in two clones of the checkout at different paths,
# with their own target dirs and CARGO_HOMEs, and every file of the two trees
# (the .iso/.img, BUILD, SHA256SUMS, media/) must be the same bytes. On a
# difference it says which files, and for an image the first bytes that
# differ.
#
# Unprivileged; everything is under $TMPDIR and deleted on exit.
set -euo pipefail

BIN=${1:-stormbootx}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
say() { printf 'repro: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
# The stamp build-golden.sh gives a golden, so GOLDEN_EFI can be compared.
STAMP="$(git -C "$ROOT" rev-parse HEAD | cut -c1-7)"
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
        first=$(cmp -l "$A" "$B" | awk 'NR==1{next} {print $1; exit}')
        [[ -n "$first" ]] && od -A x -t x1z -j $(( first - 129 )) -N 256 "$f"
    done
    die "$BIN.efi depends on where it was built"
fi
grep -aqF "$W" "$A" && die "$BIN.efi carries the build directory's path"
if [[ -n "${GOLDEN_EFI:-}" ]]; then
    dg=$(sha256sum < "$GOLDEN_EFI" | cut -d' ' -f1)
    say "$GOLDEN_EFI  $dg"
    [[ "$dg" == "$da" ]] || die "$GOLDEN_EFI is not the bytes built here"
fi
say "PASS: $BIN.efi is the same bytes from two build directories and two CARGO_HOMEs${GOLDEN_EFI:+, and $GOLDEN_EFI too}"

[[ -n "${REPRO_MEDIA:-}" ]] || exit 0
HEAD="$(git -C "$ROOT" rev-parse HEAD)"
SIDES=("$W/m1" "$W/m2-a-much-longer-media-build-directory")
for side in "${SIDES[@]}"; do
    git clone -q "$ROOT" "$side/src"
    git -C "$side/src" checkout -q --detach "$HEAD"
done
# The second build runs as another machine's would: another umask, another
# time zone and, where user namespaces allow, another uid (root in one). One
# VM built two equal ISOs while stormcentral's builder built a third (#130).
OTHER=(env -u SOURCE_DATE_EPOCH TZ=Asia/Tokyo)
unshare -r true 2>/dev/null && OTHER=(unshare -r "${OTHER[@]}")
say "the second build runs under: umask 077, ${OTHER[*]}"
for g in $REPRO_MEDIA; do
    for side in "${SIDES[@]}"; do
        local_env=(env -u SOURCE_DATE_EPOCH)
        [[ $side == "${SIDES[1]}" ]] && local_env=("${OTHER[@]}")
        ( cd "$side/src" && { [[ $side == "${SIDES[1]}" ]] && umask 077; true; } \
            && "${local_env[@]}" CARGO_HOME="$side/cargo-home" \
            CARGO_TARGET_DIR="$side/target" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
            deploy/build-golden.sh "$g" "$side/out-$g" ) >"$side/$g.log" 2>&1 \
            || { tail -40 "$side/$g.log"; die "golden $g did not build in $side"; }
        ( cd "$side/out-$g" && find . -type f | LC_ALL=C sort | xargs sha256sum ) > "$side/$g.sums"
    done
    a="${SIDES[0]}" b="${SIDES[1]}"
    if ! diff -q "$a/$g.sums" "$b/$g.sums" >/dev/null; then
        say "golden $g: files that differ:"
        diff "$a/$g.sums" "$b/$g.sums" | sed 's/^/repro:   /' || true
        for f in $(diff <(cut -c67- "$a/$g.sums") <(cut -c67- "$b/$g.sums") >/dev/null; \
                   paste -d' ' <(cut -c1-64 "$a/$g.sums") <(cut -c1-64 "$b/$g.sums") <(cut -c67- "$a/$g.sums") \
                   | awk '$1 != $2 {print $3}'); do
            say "  $f: $(cmp -l "$a/out-$g/$f" "$b/out-$g/$f" 2>/dev/null | wc -l) byte(s) differ; the first:"
            cmp -l "$a/out-$g/$f" "$b/out-$g/$f" 2>/dev/null | head -12 | sed 's/^/repro:     /' || true
        done
        die "golden $g depends on where (or when) it was built"
    fi
    say "golden $g: $(wc -l < "$a/$g.sums") files, the same bytes from both builds"
    sed 's/^/repro:   /' "$a/$g.sums"
done
say "PASS: the media goldens ($REPRO_MEDIA) are the same bytes from two builds"
