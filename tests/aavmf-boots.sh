# stormbootx for arm64 under AAVMF (#84), sourced by tests/net-ovmf.sh with
# ARCH=arm64 once its stubs are up (the engine, the NVMe/TCP target whose ESP
# carries the payload as BOOTAA64.EFI, the SNTP server).
#
#   ARCH=arm64 [A64_GOLDEN_ISO=ISO] [A64_GOLDEN_DISK=IMG] \
#       tests/net-ovmf.sh STORMBOOTX-AARCH64.EFI TCP4PROBE-AARCH64.EFI
#
# QEMU's `virt` machine, `-cpu max`, TCG (the build VM is x86_64), EDK2's
# ArmVirtQemu firmware (edk2-aarch64, "AAVMF") and a virtio-net NIC with no
# option ROM, driven by the firmware's VirtioNetDxe. Boots:
#   a64-golden  the stormbootx-arm64 golden's ISO as built (A64_GOLDEN_ISO),
#               on virtio-scsi: its BOOTAA64.EFI starts, says `media : fw
#               arm64`, and stops there (its engine is not reachable here);
#   a64-stick   the stormbootx-arm64-disk golden's stick (A64_GOLDEN_DISK),
#               on qemu-xhci + usb-storage, the same;
#   a64-claim   STORMBOOTX-AARCH64.EFI on a test ISO naming the stubs: smoltcp
#               over the firmware's SNP, a lease from slirp, the engine's
#               version, the default claim (`stubhost`), the RTC set from the
#               SNTP stub, the attach, the firmware's FAT reading the 96 MiB payload
#               through stormbootx's BlockIO, and the payload (tcp4probe built
#               for aarch64) started as \EFI\BOOT\BOOTAA64.EFI and reading the
#               hand-down back.
#
# Needs qemu-system-aarch64 and edk2-aarch64. Everything is under $W.

AAVMF_DIR=${AAVMF_DIR:-/usr/share/edk2/aarch64}
# The silent build: the default one is DEBUG and writes its debug lines to
# the same PL011 as the console, in the middle of ours.
AAVMF_CODE=${AAVMF_CODE:-$AAVMF_DIR/QEMU_EFI-silent-pflash.raw}
AAVMF_VARS=${AAVMF_VARS:-$AAVMF_DIR/vars-template-pflash.raw}
AAVMF_BIOS=${AAVMF_BIOS:-$AAVMF_DIR/QEMU_EFI.fd}
A64_LIMIT=${A64_LIMIT:-420}
A64_START_LIMIT=${A64_START_LIMIT:-240}

command -v qemu-system-aarch64 >/dev/null || die "qemu-system-aarch64 is not installed (#84, stormcentral#604)"
say "AAVMF: $(ls "$AAVMF_DIR" 2>/dev/null | tr '\n' ' ')"
say "$(qemu-system-aarch64 --version | head -1)"
if [[ -r $AAVMF_CODE && -r $AAVMF_VARS ]]; then
    say "AAVMF as pflash: $AAVMF_CODE, a copy of $AAVMF_VARS"
elif [[ -r $AAVMF_BIOS ]]; then
    say "AAVMF as -bios: $AAVMF_BIOS (no pflash variable template)"
else
    die "no AAVMF in $AAVMF_DIR (edk2-aarch64)"
fi

# a64 NAME MEDIUM EXPECTED...   (MEDIUM: an .iso on virtio-scsi, an .img as a
# USB stick). A64_STOP is the console line that ends the boot (the payload's
# banner by default), A64_SETTLE how long the firmware runs on after it.
A64_STOP=
A64_SETTLE=3
a64() {
    local name=$1 medium=$2; shift 2
    [[ -z "${NET_ONLY:-}" || "$NET_ONLY" == "$name" ]] || return 0
    local log="$W/$name.serial" txt="$W/$name.txt" fw media
    if [[ -r $AAVMF_CODE && -r $AAVMF_VARS ]]; then
        cp "$AAVMF_VARS" "$W/$name.vars"
        fw=(-drive if=pflash,format=raw,readonly=on,file="$AAVMF_CODE"
            -drive if=pflash,format=raw,file="$W/$name.vars")
    else
        fw=(-bios "$AAVMF_BIOS")
    fi
    case $medium in
        *.img) media=(-drive if=none,id=stick,format=raw,file="$medium"
                      -device qemu-xhci,id=xhci -device usb-storage,bus=xhci.0,drive=stick,removable=on,bootindex=1) ;;
        *) media=(-device virtio-scsi-pci,id=cdscsi
                  -drive if=none,id=cd0,media=cdrom,format=raw,readonly=on,file="$medium"
                  -device scsi-cd,drive=cd0,bus=cdscsi.0,bootindex=1) ;;
    esac
    : > "$log"; : > "$W/stub.log"; : > "$W/ntp.log"
    say "[$name] booting $(basename "$medium") under AAVMF (virt, tcg, cpu max, up to ${A64_LIMIT}s)"
    qemu-system-aarch64 -machine virt -accel tcg -cpu max -m 1024 \
        "${fw[@]}" \
        -fw_cfg name=opt/org.tianocore/IPv4Support,string=no \
        -fw_cfg name=opt/org.tianocore/IPv6Support,string=no \
        -netdev user,id=n0 -device virtio-net-pci,netdev=n0,romfile= \
        "${media[@]}" \
        -display none -serial file:"$log" -no-reboot &
    local qemu=$! t=0 how="stopped at its stop line"
    local stop=${A64_STOP:-is there a TCP/IP stack in this firmware}
    while kill -0 "$qemu" 2>/dev/null && (( t < A64_LIMIT )); do
        grep -qE "$stop|no network boot: " "$log" 2>/dev/null && { sleep "$A64_SETTLE"; break; }
        if (( t == A64_START_LIMIT )) && ! grep -q "NVMe/TCP boot extension" "$log" 2>/dev/null; then
            how="never started stormbootx in ${A64_START_LIMIT}s"
            break
        fi
        sleep 1; t=$((t + 1))
    done
    kill -0 "$qemu" 2>/dev/null || how="qemu exited by itself"
    (( t < A64_LIMIT )) || how="ran the whole ${A64_LIMIT}s"
    kill "$qemu" 2>/dev/null || true; wait "$qemu" 2>/dev/null || true
    say "[$name] $how after ${t}s"
    tr -d '\r' < "$log" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' > "$txt"
    say "[$name] console:"
    grep -v '^\s*$' "$txt" | sed -n '1,120s/^/  | /p'
    say "[$name] stub log:"
    sed 's/^/  > /' "$W/stub.log" "$W/ntp.log" | cut -c1-240
    local fail=0 want
    for want in "$@"; do
        if [[ "$want" == stub:* ]]; then
            grep -qF -- "${want#stub:}" "$W/stub.log" "$W/ntp.log" && say "[$name] stub saw: ${want#stub:}" \
                || { say "[$name] stub missing: ${want#stub:}"; fail=1; }
        elif [[ "$want" == not:* ]]; then
            grep -qF -- "${want#not:}" "$txt" && { say "[$name] unexpected: ${want#not:}"; fail=1; } \
                || say "[$name] absent, as it should be: ${want#not:}"
        elif grep -qF -- "$want" "$txt"; then
            say "[$name] found: $want"
        else
            say "[$name] missing: $want"; fail=1
        fi
    done
    if [[ $fail -ne 0 ]]; then
        say "[$name] its boot, attach and bootloader lines:"
        grep -nE 'blockio|BOOTX64|BOOTAA64|boot fs|boot  ' "$txt" | head -40 | sed 's/^/  | /' || true
        say "[$name] console, last 60 lines:"
        grep -v '^\s*$' "$txt" | tail -60 | sed 's/^/  | /'
        die "[$name] expected lines missing"
    fi
}

# The goldens as built: BOOTAA64.EFI starts and knows its medium.
if [[ -n "${A64_GOLDEN_ISO:-}" ]]; then
    A64_STOP="media       : " \
    a64 a64-golden "$A64_GOLDEN_ISO" "stormbootx $VERSION (" "NVMe/TCP boot extension" "media       : fw arm64"
fi
if [[ -n "${A64_GOLDEN_DISK:-}" ]]; then
    A64_STOP="media       : " \
    a64 a64-stick "$A64_GOLDEN_DISK" "stormbootx $VERSION (" "NVMe/TCP boot extension" "media       : fw arm64"
fi

# The whole path against the stubs.
rm -f "$W/claim.new" "$W/claim.slow" "$W/ntp.bad" "$W/intent"
: > "$W/claim.ok"
"$ROOT/scripts/build-boot-agent.sh" --iso --arch arm64 --binary "$EFI" --engine 10.0.2.2 --api-port "$PORT" \
    --port "$NVME_PORT" --nsid 1 --ntp "10.0.2.2:$NTP_PORT" --media "agent arm64" --output "$W/a64.iso" >/dev/null
a64 a64-claim "$W/a64.iso" \
    "media       : agent arm64" \
    "tcp4        : smoltcp over SNP (nic 0 " \
    "nic 0: leased 10.0.2.15/24 gw 10.0.2.2" \
    "engine      : stormblock 19.4.0  (universal boot)" \
    "booting stubhost's image" \
    ", set to 2031-05-04 03:02:0" \
    "namespace : " \
    "x 4096 bytes" \
    "blockio     : published on handle" \
    "blockio     : 96 MiB read in all" \
    "boot        : starting \\EFI\\BOOT\\BOOTAA64.EFI from the attached image" \
    "is there a TCP/IP stack in this firmware" \
    "handed down : StormBootTag = stubhost  (attributes 0x6)" \
    "handed down : StormBootClock = synced:10.0.2.2  (attributes 0x6)" \
    "boot fs     : \\EFI\\BOOT lists . | .. | BOOTAA64.EFI" \
    "boot fs     : BOOTAA64.EFI is 100663296 bytes; not hashed here" \
    "rtc         : 2031-05-04 03:0" \
    "not:no network boot" \
    "not:BOOTX64.EFI" \
    "stub:GET /api/v1/health" \
    "stub:POST /api/v1/synonyms/boothost/default/claim" \
    "stub:{\"agent\":{\"name\":\"stormbootx\",\"version\":\"$VERSION\"" \
    "stub:NTP request from "
