#!/usr/bin/env bash
# Check that a stormbootx ISO is laid out the way Debian's netinst is (#55).
#
#   tests/iso-layout.sh ISO [ISO ...]
#
# AMI Aptio 4 (server1, X9) hung at POST A2 after reading the first 12 KB of
# a pure-El-Torito ISO's FAT16 esp.img; Debian's isohybrid ISO with a FAT12
# efi.img booted on the same virtual media. So each ISO must have:
#   - a UEFI El Torito boot image, /esp.img;
#   - an MBR partition of type 0xef and a GPT entry, both mapping /esp.img;
#   - that ESP at FAT12 with 2 KiB clusters (mkfs.fat's geometry at 4 MiB),
#     carrying \EFI\BOOT\BOOTX64.EFI.
#
# Needs xorriso, fsck.fat and mtools. Unprivileged; files under $TMPDIR.
set -euo pipefail

[[ $# -gt 0 ]] || { echo "usage: $0 ISO [ISO ...]" >&2; exit 2; }

say() { printf 'iso-layout: %s\n' "$*"; }
W=$(mktemp -d "${TMPDIR:-/tmp}/iso-layout.XXXXXX")
trap 'rm -rf "$W"' EXIT

fail=0
check() {  # check ISO 'what' command...
    local what=$1; shift
    if "$@"; then say "  ok: $what"; else say "  FAIL: $what"; fail=1; fi
}

for iso in "$@"; do
    say "$(basename "$iso")"
    xorriso -indev "$iso" -report_el_torito plain -report_system_area plain \
        > "$W/report" 2>/dev/null
    check "El Torito UEFI boot image" \
        grep -qE '^El Torito boot img : +1 +UEFI +y +none' "$W/report"
    check "El Torito image is /esp.img" \
        grep -qE '^El Torito img path : +1 +/esp.img$' "$W/report"
    check "MBR partition of type 0xef" \
        grep -qE '^MBR partition +: +[0-9]+ +0x[0-9a-f]{2} +0xef ' "$W/report"
    check "MBR partition maps /esp.img" \
        grep -qE '^MBR partition path +: +[0-9]+ +/esp.img$' "$W/report"
    check "GPT entry maps /esp.img" \
        grep -qE '^GPT partition path +: +[0-9]+ +/esp.img$' "$W/report"

    rm -f "$W/esp.img"
    xorriso -osirrox on -indev "$iso" -extract /esp.img "$W/esp.img" >/dev/null 2>&1
    fsck.fat -v -n "$W/esp.img" > "$W/fsck" 2>&1 || true
    check "ESP is FAT12" grep -q '12 bit entries' "$W/fsck"
    check "ESP has 2048-byte clusters" grep -qE '^ +2048 bytes per cluster' "$W/fsck"
    check "ESP carries \\EFI\\BOOT\\BOOTX64.EFI" \
        mdir -i "$W/esp.img" ::/EFI/BOOT/BOOTX64.EFI
done
[[ $fail -eq 0 ]] || { say "FAIL"; exit 1; }
say "PASS"
