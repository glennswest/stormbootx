#!/usr/bin/env bash
# The EFI Shell fallback (#60): boot an EFI Shell under OVMF with a stormbootx
# ISO attached and no boot option for it, and require the ISO's startup.nsh
# to start stormbootx by itself.
#
#   tests/shell-ovmf.sh ISO old|ovmf|SHELL.efi 'EXPECTED LINE' ...
#
# old   the EDK shell (EdkShellBinPkg Shell_Full.efi, "EFI Shell version
#       2.31"), the shell AMI Aptio 4 carries; fetched from edk2-stable201811,
#       the last tag that has it, and checked against its pinned digest
# ovmf  the build box's own UEFI Shell 2.x (Shell.efi beside OVMF)
#
# The shell is the first boot option (bootindex 0). The CD is the second
# only because OVMF connects nothing the boot order leaves out; the shell
# never returns to the boot manager. The shell is BOOTX64.EFI on a disk of
# its own, which is also the decoy, a FAT with \EFI\BOOT\
# BOOTX64.EFI and no \stormboot\stormboot.conf. LAYOUT says where it sorts:
# `cd-first` (default) maps the CD as fs0, as on a blade with no local FAT;
# `cd-last` puts the decoy first. Each EXPECTED is a fixed string the serial
# console must carry; `startup.nsh: starting stormbootx from` is always
# required too.
#
# LOCAL_EFI=FILE (#145) is server1's layout: the shell's disk is removable
# (a stick), and a fixed virtio disk carries FILE as its ESP's
# \EFI\BOOT\BOOTX64.EFI, the installed disk. stormbootx falls through (no
# network) and must start that file itself rather than return to the shell.
#
# Needs qemu-system-x86_64, OVMF, mkfs.fat, mtools, sfdisk and curl (old).
# Unprivileged; everything is under $TMPDIR and deleted on exit.
set -euo pipefail

ISO=${1:?usage: $0 ISO old|ovmf|SHELL.efi 'EXPECTED' ...}
SHELL_ARG=${2:?usage: $0 ISO old|ovmf|SHELL.efi 'EXPECTED' ...}
shift 2
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LAYOUT=${LAYOUT:-cd-first}
LIMIT=${LIMIT:-60}
OLD_SHELL_URL=https://github.com/tianocore/edk2/raw/edk2-stable201811/EdkShellBinPkg/FullShell/X64/Shell_Full.efi
OLD_SHELL_SHA256=ea5e763a8a5f9733dbf7c33ffa16a19e078c6af635b51d8457bc377a22106a8c
export MTOOLS_SKIP_CHECK=1

say() { printf 'shell-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

for cmd in qemu-system-x86_64 mkfs.fat mmd mcopy sfdisk; do
    command -v "$cmd" >/dev/null || die "$cmd is not installed"
done
[[ -r "$OVMF_CODE" ]] || die "no OVMF at $OVMF_CODE"
[[ -s "$ISO" ]] || die "no ISO at $ISO"

W=$(mktemp -d "${TMPDIR:-/tmp}/shell-ovmf.XXXXXX")
trap 'rm -rf "$W"' EXIT

case "$SHELL_ARG" in
old)
    curl -fsSL -o "$W/shell.efi" "$OLD_SHELL_URL" || die "could not fetch $OLD_SHELL_URL"
    echo "$OLD_SHELL_SHA256  $W/shell.efi" | sha256sum -c --quiet - \
        || die "the EDK shell's digest is not $OLD_SHELL_SHA256"
    SHELL_EFI="$W/shell.efi" ;;
ovmf)
    SHELL_EFI=$(find "$(dirname "$OVMF_CODE")" /usr/share/edk2 -name 'Shell*.efi' 2>/dev/null | head -1)
    [[ -n "$SHELL_EFI" ]] || die "no Shell.efi beside OVMF" ;;
*)
    SHELL_EFI="$SHELL_ARG" ;;
esac
[[ -s "$SHELL_EFI" ]] || die "no shell at $SHELL_EFI"

# The shell's disk, and the decoy: BOOTX64.EFI with no stormboot.conf.
truncate -s 8M "$W/shell-esp.img"
mkfs.fat -n EFISHELL "$W/shell-esp.img" >/dev/null
mmd -i "$W/shell-esp.img" ::/EFI ::/EFI/BOOT
mcopy -i "$W/shell-esp.img" "$SHELL_EFI" ::/EFI/BOOT/BOOTX64.EFI
truncate -s 10M "$W/shell.img"
sfdisk --quiet --label gpt "$W/shell.img" <<EOF
start=2048, size=16384, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B
EOF
dd if="$W/shell-esp.img" of="$W/shell.img" bs=1M seek=1 conv=notrunc status=none

# The installed disk (#145): a GPT ESP with LOCAL_EFI as its bootloader.
ldisk=()
if [[ -n "${LOCAL_EFI:-}" ]]; then
    [[ -s "$LOCAL_EFI" ]] || die "no LOCAL_EFI at $LOCAL_EFI"
    truncate -s 8M "$W/local-esp.img"
    mkfs.fat -n LOCALESP "$W/local-esp.img" >/dev/null
    mmd -i "$W/local-esp.img" ::/EFI ::/EFI/BOOT
    mcopy -i "$W/local-esp.img" "$LOCAL_EFI" ::/EFI/BOOT/BOOTX64.EFI
    truncate -s 10M "$W/local.img"
    sfdisk --quiet --label gpt "$W/local.img" <<EOF
start=2048, size=16384, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B
EOF
    dd if="$W/local-esp.img" of="$W/local.img" bs=1M seek=1 conv=notrunc status=none
    ldisk=(-drive if=none,id=ld,format=raw,file="$W/local.img"
           -device virtio-blk-pci,drive=ld,addr=0x7,bootindex=2)
fi

# Mappings follow device-path order, so the PCI slot decides which is fs0.
case "$LAYOUT" in
cd-first) CD_ADDR=0x4; SH_ADDR=0x6 ;;
cd-last)  CD_ADDR=0x6; SH_ADDR=0x4 ;;
*) die "LAYOUT: cd-first or cd-last" ;;
esac

if [[ -n "${LOCAL_EFI:-}" ]]; then
    # Removable as a stick is, without USB: a USB stick was not always
    # enumerated before BDS looked for it (Boot0002 Not Found).
    shdev=(-device virtio-scsi-pci,id=shscsi,addr="$SH_ADDR"
           -device scsi-hd,drive=sh,bus=shscsi.0,removable=on,bootindex=0)
else
    shdev=(-device virtio-blk-pci,drive=sh,addr="$SH_ADDR",bootindex=0)
fi

cp "$OVMF_VARS" "$W/vars.fd"
accel=tcg
[[ -w /dev/kvm ]] && accel=kvm
say "booting $(basename "$SHELL_EFI") ($SHELL_ARG) with $(basename "$ISO"), $LAYOUT, under OVMF ($accel, up to ${LIMIT}s)"
timeout "$LIMIT" qemu-system-x86_64 -machine q35,accel="$accel" -m 512 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file="$W/vars.fd" \
    -device virtio-scsi-pci,id=scsi,addr="$CD_ADDR" \
    -drive if=none,id=cd,media=cdrom,format=raw,readonly=on,file="$ISO" \
    -device scsi-cd,drive=cd,bus=scsi.0,bootindex=1 \
    -drive if=none,id=sh,format=raw,file="$W/shell.img" \
    "${shdev[@]}" "${ldisk[@]}" \
    -net none \
    -display none -serial file:"$W/serial.log" -no-reboot || true

tr -d '\r' < "$W/serial.log" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' > "$W/console.txt"
fail=0
for want in "startup.nsh: starting stormbootx from" "$@"; do
    if grep -qF -- "$want" "$W/console.txt"; then
        say "found: $want"
    else
        say "missing: $want"; fail=1
    fi
done
if [[ $fail -ne 0 ]]; then
    say "console (last 80 lines):"
    grep -v '^\s*$' "$W/console.txt" | tail -80
    die "the shell did not start stormbootx"
fi
say "console:"
grep -E 'startup.nsh|Shell|stormbootx|media' "$W/console.txt" | sed -n '1,20s/^/  | /p'
say "PASS"
