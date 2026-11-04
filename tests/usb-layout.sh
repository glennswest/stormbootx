#!/usr/bin/env bash
# Check a golden's USB stick image is the layout a Dell 14G boots (#185).
#
#   tests/usb-layout.sh IMG [TREE]
#
# The R6415 (BIOS 1.12.2) refused the hybrid ISO on a stick and booted a hand-
# built GPT + FAT32 one. So IMG must have:
#   - a protective MBR (one 0xee entry) and a GPT with one ESP, at LBA 2048;
#   - a real FAT32 there: FATSz16 0, the FAT32 string, at least 65,525 clusters
#     (a FAT32 label on FAT16's cluster count is what Aptio misread,
#     stormcos#188), hidden sectors 2048, and a clean fsck;
#   - \EFI\BOOT\BOOTX64.EFI (EFI_NAME=BOOTAA64.EFI on arm64), startup.nsh and
#     \stormboot\stormboot.conf;
#   - with TREE (the golden's media/), every file of TREE byte for byte, and no
#     other: the stick is the same medium as the ISO.
#
# Needs sfdisk, fsck.fat and mtools. Unprivileged; files under $TMPDIR.
set -euo pipefail

IMG=${1:?usage: $0 IMG [TREE]}
TREE=${2:-}
EFI_NAME=${EFI_NAME:-BOOTX64.EFI}
say() { printf 'usb-layout: %s\n' "$*"; }
W=$(mktemp -d "${TMPDIR:-/tmp}/usb-layout.XXXXXX")
trap 'rm -rf "$W"' EXIT

fail=0
check() {  # check 'what' command...
    local what=$1; shift
    if "$@"; then say "  ok: $what"; else say "  FAIL: $what"; fail=1; fi
}
byte() { od -An -tu1 -j"$2" -N1 "$1" | tr -d ' '; }   # FILE OFFSET
le16() { od -An -tu2 --endian=little -j"$2" -N2 "$1" | tr -d ' '; }
le32() { od -An -tu4 --endian=little -j"$2" -N4 "$1" | tr -d ' '; }
is() { [[ "$1" == "$2" ]]; }
at_least() { (( ${1:-0} >= $2 )); }

say "$(basename "$IMG")"
check "protective MBR (0xee, boot signature)" is "$(byte "$IMG" 450):$(le16 "$IMG" 510)" "238:43605"
sfdisk -d "$IMG" > "$W/table" 2>/dev/null || true
check "a GPT" grep -q '^label: gpt' "$W/table"
check "one partition" is "$(grep -c '^[^ ]* *: start=' "$W/table")" 1
check "it is the ESP, at LBA 2048" \
    grep -qiE ': start= +2048,.*type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B' "$W/table"

SECTORS=$(sed -n 's/.*: start= *[0-9]*, size= *\([0-9]*\),.*/\1/p' "$W/table" | head -1)
if [[ -z "$SECTORS" ]]; then
    say "FAIL: no partition to read"; exit 1
fi
dd if="$IMG" of="$W/esp" bs=512 skip=2048 count="$SECTORS" status=none

check "FATSz16 is 0" is "$(le16 "$W/esp" 22)" 0
check "the FAT32 string" is "$(dd if="$W/esp" bs=1 skip=82 count=8 status=none)" "FAT32   "
check "hidden sectors 2048, where it starts" is "$(le32 "$W/esp" 28)" 2048
fsck.fat -v -n "$W/esp" > "$W/fsck" 2>&1 && clean=yes || clean=no
check "fsck.fat finds nothing" is "$clean" yes
CLUSTERS=$(sed -n 's/^ *\([0-9]*\) data clusters.*/\1/p' "$W/fsck")
check "a real FAT32: $CLUSTERS clusters, at least 65525" at_least "$CLUSTERS" 65525

check "\\EFI\\BOOT\\$EFI_NAME" mdir -i "$W/esp" "::/EFI/BOOT/$EFI_NAME"
check "\\startup.nsh" mdir -i "$W/esp" ::/startup.nsh
check "\\stormboot\\stormboot.conf" mdir -i "$W/esp" ::/stormboot/stormboot.conf

if [[ -n "$TREE" ]]; then
    mkdir "$W/out"
    mcopy -s -n -i "$W/esp" ::/EFI ::/stormboot ::/startup.nsh "$W/out/" 2>/dev/null || true
    check "the same files as $(basename "$TREE")/" diff -r "$TREE" "$W/out"
fi
[[ $fail -eq 0 ]] || { say "FAIL"; exit 1; }
say "PASS"
