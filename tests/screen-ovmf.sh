#!/usr/bin/env bash
# The boot panel (#144) under OVMF: boot an ISO with no network, so the boot
# fails at "NIC up" and, with no local disk, sits on the panel for 30 s.
#
#   tests/screen-ovmf.sh ISO
#
# Checked on the screen (QEMU monitor screendumps, tests/screen.py):
#   - the panel is drawn: a light-grey header bar across the top text row;
#   - the failed step is red;
#   - F2 swaps in the details screen (the dump changes), Esc brings the
#     panel back (the dump below the header, where the clock is, is the same).
# Checked on the serial console: the log is all there (`no network boot:`,
# `RESULT: nothing to fall through to`) and no panel text is mixed into it.
#
# SCREEN_PNG=1 prints each dump as a base64 PNG line (`screen-ovmf: png NAME
# …`) so a screenshot can leave the build VM in its log.
#
# Needs qemu-system-x86_64, OVMF and python3. Unprivileged; everything is
# under $TMPDIR and deleted on exit.
set -euo pipefail

ISO=${1:?usage: $0 ISO}
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LIMIT=${LIMIT:-120}
HERE=$(cd "$(dirname "$0")" && pwd)

say() { printf 'screen-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

command -v qemu-system-x86_64 >/dev/null || die "qemu-system-x86_64 is not installed"
command -v python3 >/dev/null || die "python3 is not installed"
[[ -r "$OVMF_CODE" ]] || die "no OVMF at $OVMF_CODE"
[[ -s "$ISO" ]] || die "no ISO at $ISO"

W=$(mktemp -d "${TMPDIR:-/tmp}/screen-ovmf.XXXXXX")
trap 'kill "${qemu:-0}" 2>/dev/null || true; rm -rf "$W"' EXIT
cp "$OVMF_VARS" "$W/vars.fd"
accel=tcg
[[ -w /dev/kvm ]] && accel=kvm
S=(python3 "$HERE/screen.py")

say "booting $(basename "$ISO") with no network under OVMF ($accel, up to ${LIMIT}s)"
qemu-system-x86_64 -machine q35,accel="$accel" -m 512 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file="$W/vars.fd" \
    -cdrom "$ISO" -boot d -net none \
    -monitor unix:"$W/mon",server,nowait \
    -display none -serial file:"$W/serial.log" -no-reboot &
qemu=$!
t=0
tail_die() {
    say "serial console (last 30 lines):"
    tr -d '\r' < "$W/serial.log" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' | grep -v '^\s*$' | tail -30 | sed 's/^/  | /'
    die "$1"
}
until grep -q "RESULT: nothing to fall through to" "$W/serial.log" 2>/dev/null; do
    kill -0 "$qemu" 2>/dev/null || tail_die "qemu exited before the fall-through"
    (( t < LIMIT )) || tail_die "no fall-through after ${LIMIT}s"
    sleep 1; t=$((t + 1))
done
sleep 2

"${S[@]}" dump "$W/mon" "$W/panel.ppm"
"${S[@]}" key "$W/mon" f2
sleep 2
"${S[@]}" dump "$W/mon" "$W/details.ppm"
"${S[@]}" key "$W/mon" esc
sleep 2
"${S[@]}" dump "$W/mon" "$W/back.ppm"
kill "$qemu" 2>/dev/null; wait "$qemu" 2>/dev/null || true

tr -d '\r' < "$W/serial.log" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' > "$W/console.txt"
fail=0
check() { local what=$1; shift; if "$@"; then say "ok: $what"; else say "not so: $what"; fail=1; fi; }
check "the panel is drawn" "${S[@]}" panel "$W/panel.ppm"
check "a failed step is red" "${S[@]}" red "$W/panel.ppm"
check "F2 shows the details" "${S[@]}" differ "$W/panel.ppm" "$W/details.ppm"
check "Esc brings the panel back" "${S[@]}" same "$W/panel.ppm" "$W/back.ppm"
for want in "no network boot: " "RESULT: nothing to fall through to"; do
    check "the serial log has: $want" grep -qF -- "$want" "$W/console.txt"
done
for unwanted in "[ OK ]" "[FAIL]" "F2: every log line"; do
    check "the serial log has no panel text: $unwanted" bash -c '! grep -qF -- "$1" "$2"' _ "$unwanted" "$W/console.txt"
done
if [[ -n "${SCREEN_PNG:-}" ]]; then
    for n in panel details back; do
        say "png $n $("${S[@]}" png "$W/$n.ppm")"
    done
fi
if [[ $fail -ne 0 ]]; then
    say "serial console (last 40 lines):"
    grep -v '^\s*$' "$W/console.txt" | tail -40 | sed 's/^/  | /'
    die "the boot panel is not right"
fi
say "PASS"
