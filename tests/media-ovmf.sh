#!/usr/bin/env bash
# Boot a stormbootx ISO under OVMF and require a line on its console (#45).
#
#   tests/media-ovmf.sh ISO 'EXPECTED LINE' ['ANOTHER' ...]
#   tests/media-ovmf.sh IMG 'EXPECTED LINE' ['ANOTHER' ...]
#
# An `.img` (a disk golden, #41) is booted as a USB stick (qemu-xhci +
# usb-storage, removable), the way the X9 blades boot theirs.
#
# Each argument after the ISO is a fixed string that must appear on the serial
# console within the time limit. Fedora's OVMF has no TCP4, so the boot never
# reaches the network: this checks what the media says about itself before
# that (the banner, `media : …`, the drivers it started), nothing after.
#
# Needs qemu-system-x86_64 and OVMF. KVM when /dev/kvm is writable, TCG
# otherwise. Unprivileged; everything is under $TMPDIR and deleted on exit.
set -euo pipefail

ISO=${1:?usage: $0 ISO 'EXPECTED' ...}
shift
[[ $# -gt 0 ]] || { echo "usage: $0 ISO 'EXPECTED' ..." >&2; exit 2; }
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LIMIT=${LIMIT:-60}

say() { printf 'media-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

command -v qemu-system-x86_64 >/dev/null || die "qemu-system-x86_64 is not installed"
[[ -r "$OVMF_CODE" ]] || die "no OVMF at $OVMF_CODE"
[[ -s "$ISO" ]] || die "no medium at $ISO"
case $ISO in
    *.img) medium=(-drive if=none,id=stick,format=raw,file="$ISO"
                   -device qemu-xhci,id=xhci -device usb-storage,bus=xhci.0,drive=stick,removable=on,bootindex=1) ;;
    *) medium=(-cdrom "$ISO" -boot d) ;;
esac

W=$(mktemp -d "${TMPDIR:-/tmp}/media-ovmf.XXXXXX")
trap 'rm -rf "$W"' EXIT
cp "$OVMF_VARS" "$W/vars.fd"

accel=tcg
[[ -w /dev/kvm ]] && accel=kvm
say "booting $(basename "$ISO") under OVMF ($accel, up to ${LIMIT}s)"
timeout "$LIMIT" qemu-system-x86_64 -machine q35,accel="$accel" -m 512 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file="$W/vars.fd" \
    "${medium[@]}" -net none \
    -display none -serial file:"$W/serial.log" -no-reboot || true

# The console carries CRs and escape sequences; compare on plain lines.
tr -d '\r' < "$W/serial.log" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$W/console.txt"
say "console (first 20 lines):"
# sed reads to the end: `| head` closes the pipe early, and under pipefail the
# writer's EPIPE failed the test once the console ran past 20 lines (#49).
grep -v '^\s*$' "$W/console.txt" | sed -n '1,20s/^/  | /p'
fail=0
for want in "$@"; do
    if grep -qF -- "$want" "$W/console.txt"; then
        say "found: $want"
    else
        say "missing: $want"; fail=1
    fi
done
if [[ $fail -ne 0 ]]; then
    say "console (last 60 lines):"
    tail -60 "$W/console.txt"
    die "expected lines missing"
fi
say "PASS"
