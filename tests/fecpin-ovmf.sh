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
#   holds    written, one warm reset, the next start says it held
#   cold     a card that runs it only after a cold reset: warm, cold, held (#203)
#   stuck    a card that keeps the value and never runs it: warm, cold, then
#            no third reset and no network boot (#203)
#   forgets  a card that loses the value: warm, cold, stuck, never a third
#   off      `fec_pin = off` on that same machine forgets the record, writes nothing
#   refuses  a write that fails: no reset, the boot goes on
#   rs       a card already on RS: nothing written, no reset
#   unset    a medium with no fec_pin: a differing card is read and left alone
#   api      GET /api/v1/fec, a signed `fec all rs` and `fec-reset` (#203)
#
# The real card's read and write are mlxfec's, and have run only on metal.
#
# Needs qemu-system-x86_64, OVMF, mtools. Unprivileged; under $TMPDIR, deleted on
# exit, in its own target dir so the job's stormbootx.efi is left alone.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LIMIT=${LIMIT:-120}
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

# boot NAME VARS 'CONF LINES' 'AWAITED LINE' 'WANTED RESETS' WANT...
# The resets are the boot's `-resetting` lines in order, e.g. `warm cold`.
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
    # No -no-reboot: the warm and cold resets are what is being tested. A removable
    # scsi-hd, not USB: OVMF sometimes misses a USB stick (99f9e4b, and here).
    qemu-system-x86_64 -machine q35,accel="$accel" -m 512 \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$vars" \
        -drive if=none,id=stick,format=raw,file="$T/$name.img" \
        -device virtio-scsi-pci,id=scsi -device scsi-hd,drive=stick,bus=scsi.0,removable=on,bootindex=1 \
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
    grep -E 'fec pin|test card|^  (84:00|why|wrote|warm|cold|nothing|a warm|no more|the card runs|`fec_pin)' "$txt" | sed -n '1,24s/^/  | /p' || true
    n=$(grep -oE '(warm|cold)-resetting' "$txt" | sed 's/-resetting//' | tr '\n' ' ' | sed 's/ $//' || true)
    say "[$name] resets: '${n}' in $((waited + AFTER)) s, $(grep -cF 'a TEST card is compiled in' "$txt" || true) start(s)"
    [[ "$n" == "$resets" ]] || { say "[$name] wanted resets '$resets'"; fail=1; }
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
    "the last pin held (warm: 84:00.0 port 1: device-default -> rs)" 'warm' \
    'fec pin     : rs (\stormboot\local.conf): 1 port(s) read, 1 with another next-boot value, 1 running another' \
    '84:00.0 port 1: runs device-default, next boot device-default' \
    'why: fec_pin = rs in \stormboot\local.conf; only the card' \
    'wrote 84:00.0 port 1: device-default -> rs' \
    'warm-resetting (1 of at most 2)' \
    '1 port(s) read, 0 with another next-boot value, 0 running another' \
    'not:did not get it running'

boot cold "$T/cold.fd" $'fec_pin = rs\nfec_test_card = cold' \
    'the last pin held (cold: reset to load the next-boot value)' 'warm cold' \
    '84:00.0 port 1: runs device-default, next boot rs' \
    'cold-resetting (2 of at most 2)' \
    'not:did not get it running'

boot stuck "$T/stuck.fd" $'fec_pin = rs\nfec_test_card = stuck' \
    'a warm and a cold reset did not get it running rs: cold: reset to load the next-boot value' 'warm cold' \
    'no more resets, and no network boot on the wrong FEC' \
    'not:pin held'

boot forgets "$T/forgets.fd" $'fec_pin = rs\nfec_test_card = forgets' \
    'a warm and a cold reset did not get it running rs: cold: 84:00.0 port 1: device-default -> rs' 'warm cold' \
    'not:pin held'

boot off "$T/forgets.fd" $'fec_pin = off\nfec_test_card = forgets' \
    "forgot the last pin's record: cold: 84:00.0 port 1: device-default -> rs" '' \
    'not:wrote'

boot refuses "$T/refuses.fd" $'fec_pin = rs\nfec_test_card = refuses' \
    'nothing written (1 port(s) failed)' '' \
    'not:did not get it running'

boot rs "$T/rs.fd" $'fec_pin = rs\nfec_test_card = rs' \
    '1 port(s) read, 0 with another next-boot value, 0 running another' '' \
    'not:wrote'

boot unset "$T/unset.fd" 'fec_test_card = holds' \
    'test card port 1: current device-default, next boot device-default' '' \
    'not:fec pin' 'not:wrote'

# #203: auto takes the port's speed (the test card says 25g) to the standard's
# rs; a port's own key beats fec_pin, by BDF and, in any case, by MAC.
boot auto "$T/auto.fd" $'fec_pin = auto\nfec_test_card = holds' \
    "the last pin held (warm: 84:00.0 port 1: device-default -> rs)" 'warm' \
    'fec pin     : auto (\stormboot\local.conf): 1 port(s) read, 1 with another next-boot value' \
    '0000:84:00.0 (25g) wants rs (fec_pin = auto, 25g by the standard: rs)'

boot portoff "$T/portoff.fd" $'fec_pin = rs\nfec_pin.0000:84:00.0 = off\nfec_test_card = holds' \
    'fec pin     : per port (\stormboot\local.conf): 1 port(s) read, 0 with another next-boot value, 0 running another' '' \
    '0000:84:00.0 left as is: its own key says off' 'not:wrote'

boot bymac "$T/bymac.fd" $'fec_pin = off\nfec_pin.52:54:00:FE:C0:01 = rs\nfec_test_card = holds' \
    "the last pin held (warm: 84:00.0 port 1: device-default -> rs)" 'warm' \
    '0000:84:00.0 (25g) wants rs (its own key, rs)'

# #203: the API. A NIC (slirp), no engine (port 9 refuses), `fec_pin = off` so
# the boot writes nothing itself, and a hold with a test command key: the FEC
# read by GET /api/v1/fec, a signed `fec all rs` writes the next-boot value, a
# signed `fec-reset` warm-resets, and the card then runs rs. Nothing else resets.
command -v openssl >/dev/null || die "[api] openssl is not installed"
command -v curl >/dev/null || die "[api] curl is not installed"
openssl genpkey -algorithm ed25519 -out "$T/cmd.pem" 2>/dev/null
CMD_PUB=$(openssl pkey -in "$T/cmd.pem" -pubout -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n')
[[ ${#CMD_PUB} -eq 64 ]] || die "[api] no Ed25519 public key"
APIP=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
ME=mac-525400123456

signed() {
    local id=$1 cmd=$2 args=${3:-} issued=2026-11-13T22:00:00Z
    printf 'stormbootx-command v1\n%s\n%s\n%s\n%s\n%s\n' "$ME" "$id" "$cmd" "$args" "$issued" > "$T/cmd.txt"
    openssl pkeyutl -sign -inkey "$T/cmd.pem" -rawin -in "$T/cmd.txt" -out "$T/cmd.sig"
    printf '{"host":"%s","id":%s,"command":"%s","args":"%s","issued":"%s","by":"fecpin-test","sig":"%s"}' \
        "$ME" "$id" "$cmd" "$args" "$issued" "$(od -An -tx1 "$T/cmd.sig" | tr -d ' \n')"
}
api_fail() {
    say "[api] $*; serial console, last 60 lines:"
    tr -d '\r' < "$T/api.log" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' | grep -v '^\s*$' | tail -60 | sed 's/^/  | /'
    die "[api]"
}
console_count() { tr -d '\r' < "$T/api.log" | grep -cF -- "$1" || true; }
# hold_wait N: until the console has said it is holding N times.
hold_wait() {
    local t=0
    until (( $(console_count 'hold        : holding until a command; the API at http://10.0.2.15:9180') >= $1 )); do
        (( t < LIMIT )) || api_fail "hold $1 never came"
        sleep 1; t=$((t + 1))
    done
}
console_wait() {
    local t=0
    until tr -d '\r' < "$T/api.log" | grep -qF -- "$1"; do
        (( t < 60 )) || api_fail "the console never said: $1"
        sleep 1; t=$((t + 1))
    done
    say "[api] console: $1"
}
post() {
    local code
    code=$(curl -s -m 30 -o "$T/resp" -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
        --data-binary "$1" "http://127.0.0.1:$APIP/api/v1/command" || true)
    [[ $code == 202 ]] || api_fail "POST $(cut -c1-60 <<<"$1"): HTTP $code $(cat "$T/resp" 2>/dev/null)"
    say "[api] HTTP $code: $(cut -c1-120 "$T/resp")"
}
fec_has() {
    local j
    j=$(curl -s -m 10 "http://127.0.0.1:$APIP/api/v1/fec")
    for want in "$@"; do
        [[ $j == *"$want"* ]] || api_fail "GET /api/v1/fec lacks $want: $j"
    done
    say "[api] GET /api/v1/fec: ${j:0:220}"
}

"$ROOT/scripts/build-boot-agent.sh" --binary "$T/target/x86_64-unknown-uefi/release/stormbootx.efi" \
    --engine 10.0.2.2 --api-port 9 --ntp 10.0.2.2:9 --hold forever --command-key "$CMD_PUB" \
    --no-fallback --media fecapi --size 64 --output "$T/api.img" >/dev/null
printf 'fec_pin = off\nfec_test_card = holds\n' > "$T/local.conf"
mcopy -o -i "$T/api.img@@1M" "$T/local.conf" ::/stormboot/local.conf
cp "$OVMF_VARS" "$T/api.fd"
: > "$T/api.log"
say "[api] booting with a NIC, the API at 127.0.0.1:$APIP"
qemu-system-x86_64 -machine q35,accel="$accel" -m 1024 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file="$T/api.fd" \
    -fw_cfg name=opt/org.tianocore/IPv4Support,string=no \
    -fw_cfg name=opt/org.tianocore/IPv6Support,string=no \
    -netdev user,id=n0,hostfwd=tcp:127.0.0.1:$APIP-:9180 -device virtio-net-pci,netdev=n0,romfile= \
    -drive if=none,id=stick,format=raw,file="$T/api.img" \
    -device virtio-scsi-pci,id=scsi -device scsi-hd,drive=stick,bus=scsi.0,removable=on,bootindex=1 \
    -display none -serial file:"$T/api.log" &
qemu=$!
hold_wait 1
fec_has '"fec_pin":"off"' '"bdf":"0000:84:00.0"' '"current":"device-default","next_boot":"device-default"' '"test_card":true'
post "$(signed 1 fec 'all rs')"
console_wait 'command     : fec (id 1): wrote '
# The pretend `holds` card reads what it stores as running at once; that a
# reset is what loads it is the boots above. Here: written, then a real reset.
fec_has '"next_boot":"rs"'
post "$(signed 2 fec-reset)"
console_wait 'command     : fec-reset (id 2): warm-resetting so the cards load their next-boot FEC'
hold_wait 2
(( $(console_count 'a TEST card is compiled in') >= 4 )) || api_fail "no second start after fec-reset"
fec_has '"current":"rs","next_boot":"rs"'
n=$(grep -oE '(warm|cold)-resetting \(' <(tr -d '\r' < "$T/api.log") | tr '\n' ' ' || true)
[[ -z "$n" ]] || api_fail "the pin reset by itself: $n"
kill "$qemu" 2>/dev/null || true
wait "$qemu" 2>/dev/null || true
qemu=""

say PASS
