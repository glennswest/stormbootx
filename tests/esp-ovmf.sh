#!/usr/bin/env bash
# The #37 bridge under OVMF: boot espprobe off a 512-byte stick, and have it
# read and start a bootloader off a second disk with 4096-byte blocks and a
# 4096-byte-sector FAT16 ESP, the shape stormblock lays on a 4K release disk.
#
#   tests/esp-ovmf.sh ESPPROBE.efi PAYLOAD.efi
#
# PAYLOAD is tcp4probe.efi in the sc-build command. espprobe starts it from
# the bytes esp.rs read (espboot.rs, the code stormbootx falls back to),
# under stormbootx's own read-only filesystem on the ESP (espfs.rs, #42), in
# place of OVMF's FAT. The payload then does what shim or systemd-boot would
# through its DeviceHandle: lists \EFI\BOOT, reads and seeks BOOTX64.EFI
# (its digest must match), reads a loader entry, and is refused a write.
# espprobe takes the filesystem back, prints `espprobe: PASS` and powers off.
#
# Needs qemu-system-x86_64, OVMF, mkfs.fat, mtools, sfdisk and python3. KVM is
# used when /dev/kvm is writable, TCG otherwise. Unprivileged; everything is
# under $TMPDIR and deleted on exit.
set -euo pipefail

PROBE=${1:?usage: $0 ESPPROBE.efi PAYLOAD.efi}
PAYLOAD=${2:?usage: $0 ESPPROBE.efi PAYLOAD.efi}
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
export MTOOLS_SKIP_CHECK=1

say() { printf 'esp-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

for cmd in qemu-system-x86_64 mkfs.fat mmd mcopy sfdisk python3; do
    command -v "$cmd" >/dev/null || die "$cmd is not installed"
done
[[ -r "$OVMF_CODE" ]] || die "no OVMF at $OVMF_CODE"

W=$(mktemp -d "${TMPDIR:-/tmp}/esp-ovmf.XXXXXX")
trap 'rm -rf "$W"' EXIT

# The stick: 512-byte blocks, espprobe as its bootloader.
truncate -s 8M "$W/stick-esp.img"
mkfs.fat -F 16 -s 1 -n ESPPROBE "$W/stick-esp.img" >/dev/null
mmd -i "$W/stick-esp.img" ::/EFI ::/EFI/BOOT
mcopy -i "$W/stick-esp.img" "$PROBE" ::/EFI/BOOT/BOOTX64.EFI
truncate -s 10M "$W/stick.img"
sfdisk --quiet --label gpt "$W/stick.img" <<EOF
start=2048, size=16384, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B
EOF
dd if="$W/stick-esp.img" of="$W/stick.img" bs=1M seek=1 conv=notrunc status=none

# The disk under test: 4096-byte blocks, a 64 MiB FAT16 at 4096-byte sectors.
mkfs.fat -C -S 4096 -F 16 -s 1 -n RELEASE "$W/esp4k.img" 65536 >/dev/null
mmd -i "$W/esp4k.img" ::/EFI ::/EFI/BOOT ::/loader ::/loader/entries
mcopy -i "$W/esp4k.img" "$PAYLOAD" ::/EFI/BOOT/BOOTX64.EFI
# A systemd-boot loader entry: a file a bootloader opens through its
# DeviceHandle's filesystem, which is stormbootx's here (#42).
printf 'title stormbootx #42 loader entry\nlinux /vmlinuz\n' > "$W/entry.conf"
mcopy -i "$W/esp4k.img" "$W/entry.conf" ::/loader/entries/stormbootx-test.conf
PAYLOAD_SHA=$(sha256sum < "$PAYLOAD" | cut -d' ' -f1)
PAYLOAD_LEN=$(stat -c %s "$PAYLOAD")
python3 - "$W/esp4k.img" "$W/disk4k.img" <<'PY'
import struct, sys, uuid, zlib
esp, out = sys.argv[1], sys.argv[2]
bs = 4096
data = open(esp, 'rb').read()
first = (1 << 20) // bs
last = first + -(-len(data) // bs) - 1
total = last + 1 + 1 + 4 + 256          # room for the backup table
entries = bytearray(128 * 128)
entries[0:128] = (uuid.UUID('C12A7328-F81F-11D2-BA4B-00A0C93EC93B').bytes_le
                  + uuid.uuid4().bytes_le + struct.pack('<QQQ', first, last, 0)
                  + 'EFI system'.encode('utf-16-le').ljust(72, b'\0'))
ecrc = zlib.crc32(entries)
disk_guid = uuid.uuid4().bytes_le
def header(me, alt, table):
    h = struct.pack('<8sIIIIQQQQ16sQIII', b'EFI PART', 0x10000, 92, 0, 0,
                    me, alt, 6, total - 6, disk_guid, table, 128, 128, ecrc)
    return h[:16] + struct.pack('<I', zlib.crc32(h)) + h[20:]
img = bytearray(total * bs)
# Protective MBR.
img[446:462] = struct.pack('<BBBBBBBBII', 0, 0, 2, 0, 0xEE, 0xFF, 0xFF, 0xFF, 1,
                           min(total - 1, 0xFFFFFFFF))
img[510:512] = b'\x55\xaa'
img[bs:bs + 92] = header(1, total - 1, 2)
img[2 * bs:2 * bs + len(entries)] = entries
img[first * bs:first * bs + len(data)] = data
img[(total - 5) * bs:(total - 5) * bs + len(entries)] = entries
img[(total - 1) * bs:(total - 1) * bs + 92] = header(total - 1, 1, total - 5)
open(out, 'wb').write(img)
PY

ACCEL=tcg; [[ -w /dev/kvm ]] && ACCEL=kvm
if [[ -r "$OVMF_VARS" ]]; then
    cp "$OVMF_VARS" "$W/vars.fd"
    FW=(-drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" -drive if=pflash,format=raw,file="$W/vars.fd")
else
    FW=(-bios "$OVMF_CODE")
fi
say "booting espprobe under OVMF ($ACCEL)"
timeout 600 qemu-system-x86_64 -machine q35,accel=$ACCEL -m 512 -display none -no-reboot \
    "${FW[@]}" -net none \
    -drive if=none,id=stick,format=raw,file="$W/stick.img" \
    -device virtio-blk-pci,drive=stick,bootindex=0 \
    -drive if=none,id=d4k,format=raw,file="$W/disk4k.img" \
    -device virtio-blk-pci,drive=d4k,logical_block_size=4096,physical_block_size=4096 \
    -serial file:"$W/serial.log" || say "qemu exited $?"

# The console, without OVMF's escape sequences.
sed -e 's/\x1b\[[0-9;?]*[A-Za-z]//g' -e 's/\r//g' "$W/serial.log" | grep -v '^\s*$' > "$W/console.txt" || true
grep -E 'espprobe|disk  |stormbootx:|local     :|firmware  :|started   :|esp fs|boot fs|TCP/IP stack' "$W/console.txt" || true

ok=yes
grep -q 'x 4096 bytes' "$W/console.txt" || { say "no 4096-byte disk was seen"; ok=no; }
grep -q 'FAT16 at 4096-byte sectors' "$W/console.txt" || { say "esp.rs did not read the 4K FAT"; ok=no; }
grep -q 'local     : ESP partition' "$W/console.txt" || { say "espboot::find (what auto asks, #3) found no bootloader"; ok=no; }
grep -q 'loaded from the buffer' "$W/console.txt" || { say "LoadImage from the buffer failed"; ok=no; }
grep -q 'is there a TCP/IP stack in this firmware' "$W/console.txt" || { say "the payload never printed"; ok=no; }
grep -q 'espprobe: PASS' "$W/console.txt" || { say "espprobe did not pass"; ok=no; }
# #42: stormbootx's filesystem replaced the firmware's FAT on the ESP, became
# the started image's DeviceHandle, and served it what a bootloader asks.
for want in \
    "esp fs    : read-only filesystem on the ESP's partition handle, the firmware's FAT disconnected from it" \
    "esp fs    : it is the loaded image's DeviceHandle" \
    ' bytes, read-only, label "RELEASE", 4096-byte blocks' \
    'boot fs     : \EFI\BOOT lists . | .. | BOOTX64.EFI' \
    "boot fs     : BOOTX64.EFI $PAYLOAD_LEN bytes, sha256 $PAYLOAD_SHA" \
    'boot fs     : seek ok' \
    'boot fs     : loader entry: title stormbootx #42 loader entry' \
    'boot fs     : create refused (WRITE_PROTECTED)' \
    'esp fs    : withdrawn'; do
    grep -qF -- "$want" "$W/console.txt" || { say "missing: $want"; ok=no; }
done
if [[ "$ok" != yes ]]; then
    say "console:"; tail -60 "$W/console.txt"
    die "the 4K ESP was not booted through stormbootx's reader"
fi
say "PASS: OVMF started a bootloader that esp.rs read off a 4096-byte FAT on a 4096-byte disk"
