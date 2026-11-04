#!/usr/bin/env bash
# The FEC pin at boot (#186) under OVMF, with a pretend card.
#
#   tests/fecpin-ovmf.sh
#
# QEMU has no ConnectX and can't fake a 15b3 id, so this builds stormbootx with
# STORMBOOTX_FEC_TEST (no golden can: build-golden.sh refuses), whose card keeps
# its NV value in a variable. What it proves is the part a bad day would be made
# of, real warm resets and a real NV variable across them:
#
#   holds    written once, one warm reset, the next start says it held
#   forgets  a card that loses the value: one warm reset and never another
#   off      `fec_pin = off` on that same machine forgets the record, writes nothing
#   refuses  a write that fails: no reset, the boot goes on
#   rs       a card already on RS: nothing written, no reset
#   unset    a medium with no fec_pin: a differing card is read and left alone
#
# The real card's read and write are mlxfec's, and have run only on metal.
#
# Needs qemu-system-x86_64, OVMF, mtools. Unprivileged; under $TMPDIR, deleted on
# exit, in its own target dir so the job's stormbootx.efi is left alone.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LIMIT=${LIMIT:-60}
# Time after the awaited line for a loop to show itself: a start takes seconds.
AFTER=${AFTER:-12}
export MTOOLS_SKIP_CHECK=1

say() { printf 'fecpin-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

T=$(mktemp -d "${TMPDIR:-/tmp}/fecpin.XXXXXX")
qemu=""
cleanup() {
    [[ -n "$qemu" ]] && kill "$qemu" 2>/dev/null || true
    rm -rf "$T"
}
trap cleanup EXIT

say "building stormbootx with the test card compiled in"
( cd "$ROOT" && STORMBOOTX_FEC_TEST=1 STORMBOOTX_BUILD=fecpin-test CARGO_TARGET_DIR="$T/target" \
    cargo build --locked --release --target x86_64-unknown-uefi --bin stormbootx )
"$ROOT/scripts/build-boot-agent.sh" --binary "$T/target/x86_64-unknown-uefi/release/stormbootx.efi" \
    --no-fallback --media fecpin --size 64 --output "$T/base.img" >/dev/null

accel=tcg
[[ -w /dev/kvm ]] && accel=kvm

# boot NAME VARS 'CONF LINES' 'AWAITED LINE' WANTED_RESETS WANT...
# A WANT starting `not:` must not be on the console.
boot() {
    local name=$1 vars=$2 conf=$3 await=$4 resets=$5
    shift 5
    say "[$name] $(printf '%s' "$conf" | tr '\n' ';')"
    cp "$T/base.img" "$T/$name.img"
    printf '%s\n' "$conf" > "$T/local.conf"
    mcopy -o -i "$T/$name.img@@1M" "$T/local.conf" ::/stormboot/local.conf
    [[ -e "$vars" ]] || cp "$OVMF_VARS" "$vars"
    local log="$T/$name.log" txt="$T/$name.txt"
    : > "$log"
    # No -no-reboot: the warm reset is what is being tested.
    qemu-system-x86_64 -machine q35,accel="$accel" -m 512 \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$vars" \
        -drive if=none,id=stick,format=raw,file="$T/$name.img" \
        -device qemu-xhci,id=xhci -device usb-storage,bus=xhci.0,drive=stick,removable=on,bootindex=1 \
        -net none -display none -serial file:"$log" &
    qemu=$!
    local waited=0
    until tr -d '\r' < "$log" | grep -qF -- "$await"; do
        (( waited < LIMIT )) || break
        sleep 1
        waited=$((waited + 1))
    done
    sleep "$AFTER"
    kill "$qemu" 2>/dev/null || true
    wait "$qemu" 2>/dev/null || true
    qemu=""
    tr -d '\r' < "$log" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$txt"

    local fail=0 want n
    grep -E 'fec pin|test card|^  (84:00|why|wrote|warm|nothing|not writing|the last|`fec_pin)' "$txt" | sed -n '1,14s/^/  | /p' || true
    n=$(grep -cF 'warm-resetting once' "$txt" || true)
    say "[$name] $n warm reset(s) in $((waited + AFTER)) s, $(grep -cF 'a TEST card is compiled in' "$txt" || true) start(s)"
    [[ "$n" == "$resets" ]] || { say "[$name] wanted $resets warm reset(s)"; fail=1; }
    grep -qF -- "$await" "$txt" || { say "[$name] missing: $await"; fail=1; }
    for want in "$@"; do
        if [[ "$want" == not:* ]]; then
            ! grep -qF -- "${want#not:}" "$txt" || { say "[$name] found: ${want#not:}"; fail=1; }
        else
            grep -qF -- "$want" "$txt" || { say "[$name] missing: $want"; fail=1; }
        fi
    done
    if [[ $fail -ne 0 ]]; then
        tail -60 "$txt"
        die "[$name]"
    fi
}

boot holds "$T/holds.fd" $'fec_pin = rs\nfec_test_card = holds' \
    "the last boot's pin held: 84:00.0 port 1: device-default -> rs" 1 \
    'fec pin     : rs (\stormboot\local.conf): 1 port(s) read, 1 with another next-boot value' \
    '84:00.0 port 1: next boot device-default: writing rs' \
    'why: fec_pin = rs in \stormboot\local.conf, and only the card' \
    'wrote 84:00.0 port 1: device-default -> rs' \
    '1 port(s) read, 0 with another next-boot value' \
    "not:did not hold"

boot forgets "$T/forgets.fd" $'fec_pin = rs\nfec_test_card = forgets' \
    "the last boot's pin did not hold: 84:00.0 port 1: device-default -> rs" 1 \
    'not writing again and not resetting' \
    "not:pin held"

boot off "$T/forgets.fd" $'fec_pin = off\nfec_test_card = forgets' \
    "forgot the last pin's record: 84:00.0 port 1: device-default -> rs" 0 \
    'not:writing rs'

boot refuses "$T/refuses.fd" $'fec_pin = rs\nfec_test_card = refuses' \
    'nothing written (1 port(s) failed)' 0 \
    'not:did not hold'

boot rs "$T/rs.fd" $'fec_pin = rs\nfec_test_card = rs' \
    '1 port(s) read, 0 with another next-boot value' 0 \
    'not:writing rs'

boot unset "$T/unset.fd" 'fec_test_card = holds' \
    'test card port 1: current device-default, next boot device-default' 0 \
    'not:fec pin' 'not:writing rs'

say PASS
