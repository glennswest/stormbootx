#!/usr/bin/env bash
# A stormdist release medium under OVMF (#198): stormbootx finds it, checks it,
# says how fast it reads, and starts the release's own bootloader from it.
#
#   tests/dist-ovmf.sh TCP4PROBE.EFI STORMDIST
#
# STORMDIST is the real writer (scripts/build-stormdist.sh): `stormdist make`
# writes a full release ISO from a recipe with a boot payload, signed with a
# test release key, as stormcentral will (stormdist#10). Its ESP's
# BOOTX64.EFI is tcp4probe, beside a marker file, so `boot fs : \EFI\BOOT
# lists … RELEASE.MRK` proves the payload ran with the medium's ESP as its
# DeviceHandle. stormbootx is built with that key (STORMBOOTX_DIST_TEST_KEY,
# its own target dir; no golden can) and boots from a stick with no network:
#
#   cd-local   the ISO as a CD, install_source = local: installed before the network
#   usb-forge  the ISO written to a USB stick, the default: the network boot
#              gives nothing (no NIC) and nothing local boots, so it installs
#   installed  the same with a fixed disk that boots: that disk, not the medium
#   tampered   a byte of the ESP changed after signing: refused, nothing started
#   otherkey   the ISO signed by another key: refused
#   off        install_source = off: the medium is not looked at
#
# The medium is bootable itself and carries a bootindex (OVMF connects only
# devices in the boot order), so the firmware would start its tcp4probe after
# stormbootx returns: the positive boots require stormbootx's `starting …
# from the stormdist medium` before tcp4probe's output, and the refusals stop
# while stormbootx is still on its last screen.
#
# Needs qemu-system-x86_64, OVMF, mtools, mkfs.fat, openssl, python3.
# Unprivileged; under $TMPDIR, deleted on exit.
set -euo pipefail

PROBE=${1:?usage: $0 TCP4PROBE.EFI STORMDIST}
STORMDIST=${2:?usage: $0 TCP4PROBE.EFI STORMDIST}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LIMIT=${LIMIT:-120}
export MTOOLS_SKIP_CHECK=1

say() { printf 'dist-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

[[ -s "$PROBE" ]] || die "no tcp4probe at $PROBE"
[[ -x "$STORMDIST" ]] || die "no stormdist at $STORMDIST"
PROBE=$(cd "$(dirname "$PROBE")" && pwd)/$(basename "$PROBE")
STORMDIST=$(cd "$(dirname "$STORMDIST")" && pwd)/$(basename "$STORMDIST")

T=$(mktemp -d "${TMPDIR:-/tmp}/dist.XXXXXX")
qemu=""
cleanup() {
    [[ -n "$qemu" ]] && kill "$qemu" 2>/dev/null || true
    rm -rf "$T"
}
trap cleanup EXIT

# The release key, and another.
key() {
    openssl genpkey -algorithm ed25519 -out "$T/$1.pem"
    openssl pkey -in "$T/$1.pem" -outform DER | tail -c 32 > "$T/$1.seed"
    openssl pkey -in "$T/$1.pem" -pubout -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n'
}
PUB=$(key release)
key other >/dev/null

say "building stormbootx with the test release key compiled in"
( cd "$ROOT" && STORMBOOTX_DIST_TEST_KEY=$PUB STORMBOOTX_BUILD=dist-test CARGO_TARGET_DIR="$T/target" \
    cargo build --locked --release --target x86_64-unknown-uefi --bin stormbootx )
"$ROOT/scripts/build-boot-agent.sh" --binary "$T/target/x86_64-unknown-uefi/release/stormbootx.efi" \
    --no-fallback --media dist --size 64 --output "$T/base.img" >/dev/null

# The release's ESP: tcp4probe as its bootloader, a marker, and 40 MiB to
# read (the read rate).
truncate -s 64M "$T/esp.img"
mkfs.fat -n RELEASEESP "$T/esp.img" >/dev/null
mmd -i "$T/esp.img" ::/EFI ::/EFI/BOOT
mcopy -i "$T/esp.img" "$PROBE" ::/EFI/BOOT/BOOTX64.EFI
printf 'the release ESP of dist-ovmf\n' > "$T/RELEASE.MRK"
mcopy -i "$T/esp.img" "$T/RELEASE.MRK" ::/EFI/BOOT/RELEASE.MRK
{ printf 'STORMDIST-TAMPER-MARK'; head -c $((40 << 20)) /dev/zero; } > "$T/filler.bin"
mcopy -i "$T/esp.img" "$T/filler.bin" ::/FILLER.BIN
head -c 4096 /dev/zero | tr '\0' '\063' > "$T/eltorito.img"

# One golden: 8 MiB, 4 MiB allocated.
{ head -c $((4 << 20)) /dev/zero | tr '\0' '\125'; head -c $((4 << 20)) /dev/zero; } > "$T/vol.raw"
cat > "$T/recipe.toml" <<EOF
version = "12.74.0-disttest"
kind = "full"
volumes = [
  { name = "golden-disttest", component = "disttest", commit = "dist-ovmf", image = "$T/vol.raw", size = 8388608, device_sha256 = "$(sha256sum "$T/vol.raw" | cut -d' ' -f1)", ranges = [ { offset = 0, length = 4194304 } ] },
]

[[flavour]]
name = "cilium"
members = ["golden-disttest"]

[boot]
esp = "$T/esp.img"
bios = "$T/eltorito.img"
EOF
"$STORMDIST" make "$T/recipe.toml" "$T/release.iso" --signing-key "$T/release.seed"
"$STORMDIST" make "$T/recipe.toml" "$T/other.iso" --signing-key "$T/other.seed"
say "stormdist made release.iso ($(stat -c %s "$T/release.iso") bytes)"
cp "$T/release.iso" "$T/tampered.iso"
python3 -I - "$T/tampered.iso" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, 'rb').read())
i = b.find(b'STORMDIST-TAMPER-MARK')
assert i >= 0, 'no marker in the ISO'
b[i + 30] ^= 0xff
open(p, 'wb').write(b)
PY

# The installed disk: a GPT ESP with tcp4probe (no marker).
truncate -s 8M "$T/local-esp.img"
mkfs.fat -n LOCALESP "$T/local-esp.img" >/dev/null
mmd -i "$T/local-esp.img" ::/EFI ::/EFI/BOOT
mcopy -i "$T/local-esp.img" "$PROBE" ::/EFI/BOOT/BOOTX64.EFI
truncate -s 10M "$T/local.img"
sfdisk --quiet --label gpt "$T/local.img" <<EOF
start=2048, size=16384, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B
EOF
dd if="$T/local-esp.img" of="$T/local.img" bs=1M seek=1 conv=notrunc status=none

accel=tcg
[[ -w /dev/kvm ]] && accel=kvm

STARTED='boot        : starting \EFI\BOOT\BOOTX64.EFI from the stormdist medium'
LISTS='boot fs     : \EFI\BOOT lists'

# boot NAME ISO cd|usb 'CONF' 'AWAITED' local|- WANT...
# A WANT `not:X` must be absent; `A<<B` needs A on an earlier line than B.
boot() {
    local name=$1 iso=$2 how=$3 conf=$4 await=$5 ldisk=$6
    shift 6
    say "[$name] $(basename "$iso") as a $how; $(printf '%s' "$conf" | tr '\n' ';')"
    cp "$T/base.img" "$T/$name.img"
    printf '%s\n' "$conf" > "$T/local.conf"
    mcopy -o -i "$T/$name.img@@1M" "$T/local.conf" ::/stormboot/local.conf
    cp "$OVMF_VARS" "$T/$name.fd"
    local dist=()
    case "$how" in
    cd)  dist=(-drive if=none,id=dist,format=raw,readonly=on,file="$iso"
               -device scsi-cd,drive=dist,bus=scsi.0,bootindex=2) ;;
    usb) cp "$iso" "$T/$name.stick"
         dist=(-drive if=none,id=dist,format=raw,file="$T/$name.stick"
               -device qemu-xhci,id=xhci -device usb-storage,bus=xhci.0,drive=dist,removable=on,bootindex=2) ;;
    esac
    local fixed=()
    [[ "$ldisk" == local ]] && fixed=(-drive if=none,id=ld,format=raw,file="$T/local.img"
                                      -device virtio-blk-pci,drive=ld,bootindex=3)
    local log="$T/$name.log" txt="$T/$name.txt"
    : > "$log"
    qemu-system-x86_64 -machine q35,accel="$accel" -m 1024 \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$T/$name.fd" \
        -drive if=none,id=stick,format=raw,file="$T/$name.img" \
        -device virtio-scsi-pci,id=scsi -device scsi-hd,drive=stick,bus=scsi.0,removable=on,bootindex=1 \
        "${dist[@]}" "${fixed[@]}" \
        -net none -display none -serial file:"$log" &
    qemu=$!
    local waited=0
    until tr -d '\r' < "$log" | grep -qF -- "$await"; do
        (( waited < LIMIT )) || break
        sleep 1
        waited=$((waited + 1))
    done
    sleep 3
    kill "$qemu" 2>/dev/null || true
    wait "$qemu" 2>/dev/null || true
    qemu=""
    tr -d '\r' < "$log" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$txt"

    local fail=0 want a b la lb
    grep -E '^(dist|boot|boot fs|timing|RESULT|local) ' "$txt" | sed 's/^/  | /' || true
    say "[$name] stopped after ${waited}s"
    grep -qF -- "$await" "$txt" || { say "[$name] missing: $await"; fail=1; }
    for want in "$@"; do
        if [[ "$want" == not:* ]]; then
            ! grep -qF -- "${want#not:}" "$txt" || { say "[$name] found: ${want#not:}"; fail=1; }
        elif [[ "$want" == *'<<'* ]]; then
            a=${want%%<<*} b=${want#*<<}
            la=$(grep -nF -- "$a" "$txt" | head -1 | cut -d: -f1)
            lb=$(grep -nF -- "$b" "$txt" | head -1 | cut -d: -f1)
            [[ -n "$la" && -n "$lb" && "$la" -lt "$lb" ]] || { say "[$name] not in order: $a, then $b"; fail=1; }
        else
            grep -qF -- "$want" "$txt" || { say "[$name] missing: $want"; fail=1; }
        fi
    done
    if [[ $fail -ne 0 ]]; then
        tail -80 "$txt"
        die "[$name]"
    fi
}

boot cd-local "$T/release.iso" cd 'install_source = local' "$LISTS" - \
    'dist        : a TEST key is compiled in' \
    'dist        : stormdist 12.74.0-disttest (full) on ' \
    'BOOT.sig and RELEASE.sig verify, RELEASE.toml matches BOOT.toml' \
    'install_source = local (in \stormboot\local.conf): before the network' \
    'dist        : read boot/esp.img: 64 MiB in ' \
    'boot/esp.img matches BOOT.toml (esp_sha256)' \
    "$STARTED<<$LISTS" \
    'RELEASE.MRK' \
    'not:no network boot'

boot usb-forge "$T/release.iso" usb '' "$LISTS" - \
    ' (usb, 512-byte blocks): BOOT.sig and RELEASE.sig verify' \
    'MiB/s (usb: a USB' \
    'the network boot gave no image' \
    'and nothing local boots: the stormdist medium' \
    "$STARTED<<$LISTS" \
    'RELEASE.MRK' \
    'dist read'

boot installed "$T/release.iso" usb '' 'starting \EFI\BOOT\BOOTX64.EFI from the local disk' local \
    'not installed: the local disk boots' \
    'not:read boot/esp.img' \
    "not:$STARTED" \
    'not:RELEASE.MRK'

boot tampered "$T/tampered.iso" cd 'install_source = local' 'RESULT:' - \
    'BOOT.sig and RELEASE.sig verify' \
    'boot/esp.img is not the ESP BOOT.toml names (esp_sha256): refused' \
    'the network boot goes on' \
    "not:$STARTED" \
    "not:$LISTS"

boot otherkey "$T/other.iso" usb '' 'RESULT:' - \
    'a stormdist medium, refused: BOOT.sig does not verify against the 2 compiled-in release key(s)' \
    'not:read boot/esp.img' \
    "not:$STARTED" \
    "not:$LISTS"

boot off "$T/release.iso" cd 'install_source = off' 'RESULT:' - \
    'not:dist        : stormdist' \
    'not:read boot/esp.img' \
    "not:$LISTS"

say PASS
