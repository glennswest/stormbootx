# The self-update's boots (#83, #89). Sourced by tests/net-ovmf.sh when
# UPDATE_KEY is set, after its stubs are up: $EFI is a stormbootx built with
# that key's public half compiled in (beside stormcentral's release key), and
# the stub engine serves whatever is published in $W/serve as the boothelper
# `stormbootx-test`.
#
# The medium is a GPT disk (scripts/build-boot-agent.sh without --iso),
# writable, on a USB stick (qemu-xhci + usb-storage, removable; MEDIUM=virtio
# puts it on virtio-blk instead), booted from its removable-media path. One
# OVMF variable store serves every boot, so StormBootMinSerial persists, and
# QEMU restarts on a reset (no -no-reboot), so a boot that updates restarts
# into the new set in the same run. Each boot is checked on the console and
# then on the disk itself (mtools):
#
#   real      stormcentral's own promotion (tests/fixtures, serial 1, hex
#             signature): verifies against the release key, and the first
#             file fetch 404s here, so nothing is written. Its claim names
#             the agent with no update serial (#90)
#   badsig    serial 5 signed with another key: refused, nothing written
#   update    serial 5 signed: five files differ (two new, one a "driver"
#             that is not a PE image), written, restart; the new binary's
#             first start is a trial, and the attach passes it
#   current   serial 5 again: nothing to do; StormBootUpdate = serial:5, and
#             the claim's agent says update_serial 5 (#90; trial:5:1 in the
#             update boot, failed:8 under serial 6 after the revert)
#   notcanary serial 6 names another machine as its only canary: refused
#   canary    serial 6 names this machine (52:54:00:12:34:56): taken, and the
#             driver it no longer carries is retired to .prev
#   short     serial 7 carries a file larger than the ESP: refused for room,
#             nothing written
#   bad1      serial 8, whose stormboot.conf names a dead NVMe port: written,
#             restart, trial start 1 falls through
#   bad2      trial start 2 falls through
#   revert    the third start puts serial 6 back and chain-loads it; serial 6
#             refuses 8 (failed here), attaches, and hands down failed:8

command -v openssl >/dev/null || die "openssl is not installed"
command -v mtype >/dev/null || die "mtools is not installed"

host_cpu=max
[[ $accel == kvm ]] && host_cpu=host
URL="http://10.0.2.2:$PORT/api/v1/boothelpers/stormbootx-test"
IMG="$W/media.img"
OFF=1048576   # build-boot-agent.sh puts the ESP at 1 MiB
MAC=52:54:00:12:34:56
case ${MEDIUM:-usb} in
    usb) medium=(-device qemu-xhci,id=xhci -device usb-storage,bus=xhci.0,drive=m0,removable=on,bootindex=1) ;;
    virtio) medium=(-device virtio-blk-pci,drive=m0,bootindex=1) ;;
    *) die "MEDIUM=$MEDIUM is not usb or virtio" ;;
esac

"$ROOT/scripts/build-boot-agent.sh" --binary "$EFI" --engine 10.0.2.2 --api-port "$PORT" \
    --port "$NVME_PORT" --nsid 1 --ntp "10.0.2.2:$NTP_PORT" --update "$URL" \
    --tree "$W/tree0" --output "$IMG" >/dev/null
cp "$OVMF_VARS" "$W/update.vars"
openssl genpkey -algorithm ed25519 -out "$W/other.pem" 2>/dev/null

# release SERIAL KEY [OPTION...]: a release from tree0 in which four files
# differ from the medium's (README.txt is new to it, and differs per serial).
# Published to serve/. Options:
#   bad          its stormboot.conf names a port nothing listens on
#   canary=MAC   a canary line (repeatable)
#   driver       it carries stormboot/drivers/retire.efi (not a PE image:
#                its load fails, and the boot goes on)
#   big=BYTES    it carries stormboot/big.bin of that size
release() {
    local serial=$1 key=$2 r="$W/rel$1" opt; shift 2
    local bad= driver= big= canaries=()
    for opt in "$@"; do
        case $opt in
            bad) bad=1 ;;
            driver) driver=1 ;;
            canary=*) canaries+=("${opt#canary=}") ;;
            big=*) big=${opt#big=} ;;
            *) die "release: unknown option $opt" ;;
        esac
    done
    rm -rf "$r" "$W/serve"
    cp -r "$W/tree0" "$r"
    printf 'stormbootx-update-test serial %s\n' "$serial" >> "$r/EFI/BOOT/BOOTX64.EFI"
    printf 'rem serial %s\r\n' "$serial" >> "$r/startup.nsh"
    printf '\n# release serial %s\n' "$serial" >> "$r/stormboot/stormboot.conf"
    [[ -n $bad ]] && sed -i 's/^port .*/port     = 9/' "$r/stormboot/stormboot.conf"
    mkdir -p "$r/stormboot/drivers"
    printf 'not a driver; a file the first medium did not have (serial %s)\n' "$serial" \
        > "$r/stormboot/drivers/README.txt"
    [[ -n $driver ]] && printf 'not a PE image; a driver a later release drops\n' \
        > "$r/stormboot/drivers/retire.efi"
    [[ -n $big ]] && head -c "$big" /dev/zero > "$r/stormboot/big.bin"
    {
        echo "stormbootx-manifest 1"
        echo "version 0.13.0-test"
        echo "commit test$serial"
        echo "golden golden-stormbootx-test-$serial"
        echo "serial $serial"
        for opt in "${canaries[@]}"; do echo "canary $opt"; done
        ( cd "$r" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | while read -r f; do
            echo "file $(sha256sum < "$f" | cut -d' ' -f1) $(stat -c %s "$f") $f"
        done )
    } > "$W/rel$serial.manifest"
    mkdir -p "$W/serve"
    cp -r "$r" "$W/serve/files"
    cp "$W/rel$serial.manifest" "$W/serve/current"
    openssl pkeyutl -sign -inkey "$key" -rawin -in "$W/serve/current" -out "$W/serve/current.sig"
}

# on_disk PATH: a file from the medium, as the boot left it.
on_disk() { mtype -i "$IMG@@$OFF" "::$1" 2>/dev/null; }
same() {
    local path=$1 want=$2
    if cmp -s <(on_disk "$path") "$want"; then say "[disk] $path is $(basename "$(dirname "$want")")/$(basename "$want")"
    else die "[disk] $path is not $want"; fi
}
state_has() {
    on_disk /stormboot/state | grep -qxF -- "$1" && say "[disk] state: $1" || {
        on_disk /stormboot/state | sed 's/^/  state | /'
        die "[disk] state lacks: $1"
    }
}

# uboot NAME EXPECTED...: boot the medium; stop at the payload or a fall-through.
uboot() {
    local name=$1; shift
    local log="$W/u-$name.serial" txt="$W/u-$name.txt"
    : > "$W/stub.log"
    say "[$name] booting the disk (${MEDIUM:-usb}) under OVMF ($accel, up to ${LIMIT}s)"
    qemu-system-x86_64 -machine q35,accel="$accel" -cpu "$host_cpu" -m 1024 \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$W/update.vars" \
        -fw_cfg name=opt/org.tianocore/IPv4Support,string=no \
        -fw_cfg name=opt/org.tianocore/IPv6Support,string=no \
        -netdev user,id=n0 -device virtio-net-pci,netdev=n0,romfile=,mac=$MAC \
        -drive if=none,id=m0,format=raw,file="$IMG" "${medium[@]}" \
        -display none -serial file:"$log" &
    local qemu=$! t=0
    while kill -0 "$qemu" 2>/dev/null && (( t < LIMIT )); do
        grep -qE "is there a TCP/IP stack in this firmware|no network boot: " "$log" 2>/dev/null \
            && { sleep 2; break; }
        sleep 1; t=$((t + 1))
    done
    kill "$qemu" 2>/dev/null; wait "$qemu" 2>/dev/null || true
    tr -d '\r' < "$log" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$txt"
    say "[$name] console:"
    { grep -E "^(stormbootx |update|drivers|    \[ *[0-9]+ s\] .*retire|attaching|blockio     : published|RESULT|no network boot|handed down : StormBootUpdate|media|  portal)" "$txt" || true; } \
        | sed 's/^/  | /'
    say "[$name] stub log:"
    { grep boothelpers "$W/stub.log" || echo "(no boothelper requests)"; } | sed 's/^/  > /'
    local fail=0 want
    for want in "$@"; do
        if [[ "$want" == stub:* ]]; then
            grep -qF -- "${want#stub:}" "$W/stub.log" && say "[$name] stub saw: ${want#stub:}" \
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
        sed -n '1,200s/^/  : /p' "$txt"
        die "[$name] expected lines missing"
    fi
}

cp "$EFI" "$W/A.efi"
attached=("blockio     : published on handle" "is there a TCP/IP stack in this firmware")
FIX="$ROOT/tests/fixtures"
no_state() { on_disk /stormboot/state >/dev/null && die "[disk] a refused release wrote a state file" || true; }

# stormcentral's own promotion, its signature as stormcentral serves it (hex).
# The binary must believe it (release key, not the test key) and go on to the
# files, which this stub does not serve: nothing is written.
rm -rf "$W/serve"; mkdir -p "$W/serve"
cp "$FIX/stormcentral-rustnic-serial1.manifest" "$W/serve/current"
cp "$FIX/stormcentral-rustnic-serial1.sig" "$W/serve/current.sig"
uboot real "${attached[@]}" \
    "update      : $URL/files/EFI/BOOT/BOOTX64.EFI answered HTTP 404" \
    "stub:GET /api/v1/boothelpers/stormbootx-test/current.sig" \
    "stub:\"commit\":\"update-test\"}" \
    "not:does not verify" \
    "not:signed manifest refused"
same /EFI/BOOT/BOOTX64.EFI "$W/A.efi"
no_state

release 5 "$W/other.pem" driver
# Two keys: stormcentral's release key (#86) and the test key.
uboot badsig "${attached[@]}" \
    "update      : a TEST key is compiled in" \
    "update      : the manifest at $URL does not verify against the 2 compiled-in key(s); nothing taken" \
    "stub:GET /api/v1/boothelpers/stormbootx-test/current.sig" \
    "not:-> v0.13.0-test"
same /EFI/BOOT/BOOTX64.EFI "$W/A.efi"
no_state

release 5 "$UPDATE_KEY" driver
uboot update "${attached[@]}" \
    "-> v0.13.0-test (golden-stormbootx-test-5, serial 5), 5 file(s)" \
    "; restarting into it" \
    "update      : serial 5 on trial, start 1 of 2; it must reach an attach" \
    "retire.efi not started: LoadImage" \
    "update      : serial 5 passed its trial (attached); it stays" \
    "handed down : StormBootUpdate = good:5  (attributes 0x6)" \
    "stub:GET /api/v1/boothelpers/stormbootx-test/files/EFI/BOOT/BOOTX64.EFI" \
    "stub:\"commit\":\"update-test\"" \
    "stub:\"update_serial\":5,\"update\":\"trial:5:1\"}"
same /EFI/BOOT/BOOTX64.EFI "$W/rel5/EFI/BOOT/BOOTX64.EFI"
same /EFI/BOOT/BOOTX64.EFI.prev "$W/A.efi"
same /stormboot/stormboot.conf "$W/rel5/stormboot/stormboot.conf"
same /stormboot/drivers/README.txt "$W/rel5/stormboot/drivers/README.txt"
same /stormboot/drivers/retire.efi "$W/rel5/stormboot/drivers/retire.efi"
same /startup.nsh "$W/rel5/startup.nsh"
state_has "serial = 5"
state_has "min = 5"

uboot current "${attached[@]}" \
    "update      : current (serial 5, v0.13.0-test)" \
    "handed down : StormBootUpdate = serial:5  (attributes 0x6)" \
    "stub:\"update_serial\":5,\"update\":\"serial:5\"}" \
    "not:restarting into it" \
    "not:on trial"

# Canaries: serial 6 is for one other machine only, then for this one. It no
# longer carries retire.efi, so taking it retires that driver.
release 6 "$UPDATE_KEY" canary=52:54:00:00:00:01
uboot notcanary "${attached[@]}" \
    "update      : serial 6 is for its 1 canaries only; not this machine" \
    "handed down : StormBootUpdate = serial:5  (attributes 0x6)" \
    "not:restarting into it"
same /EFI/BOOT/BOOTX64.EFI "$W/rel5/EFI/BOOT/BOOTX64.EFI"
state_has "serial = 5"

release 6 "$UPDATE_KEY" canary=52:54:00:00:00:01 canary=$MAC
uboot canary "${attached[@]}" \
    "-> v0.13.0-test (golden-stormbootx-test-6, serial 6), 4 file(s)" \
    ", 1 driver(s) retired; restarting into it" \
    "update      : serial 6 on trial, start 1 of 2; it must reach an attach" \
    "update      : serial 6 passed its trial (attached); it stays" \
    "handed down : StormBootUpdate = good:6  (attributes 0x6)"
same /EFI/BOOT/BOOTX64.EFI "$W/rel6/EFI/BOOT/BOOTX64.EFI"
same /EFI/BOOT/BOOTX64.EFI.prev "$W/rel5/EFI/BOOT/BOOTX64.EFI"
same /stormboot/drivers/retire.efi.prev "$W/rel5/stormboot/drivers/retire.efi"
on_disk /stormboot/drivers/retire.efi >/dev/null && die "[disk] retire.efi is still loadable"
say "[disk] /stormboot/drivers/retire.efi is gone (retired to .prev)"
state_has "serial = 6"
state_has "min = 6"

# A release larger than the medium: refused for room before anything is
# written. Its big.bin is the whole ESP, so no free space can hold it.
release 7 "$UPDATE_KEY" big=$((4 << 20))
uboot short "${attached[@]}" \
    "update      : serial 7 needs " \
    "KB free; nothing written" \
    "not:restarting into it"
same /EFI/BOOT/BOOTX64.EFI "$W/rel6/EFI/BOOT/BOOTX64.EFI"
on_disk /stormboot/big.bin >/dev/null && die "[disk] big.bin was written"
on_disk /stormboot/big.bin.new >/dev/null && die "[disk] big.bin.new was left behind"
state_has "serial = 6"

release 8 "$UPDATE_KEY" bad
uboot bad1 \
    "-> v0.13.0-test (golden-stormbootx-test-8, serial 8), 4 file(s)" \
    "update      : serial 8 on trial, start 1 of 2; it must reach an attach" \
    "no network boot: " \
    "not:passed its trial"
same /EFI/BOOT/BOOTX64.EFI "$W/rel8/EFI/BOOT/BOOTX64.EFI"
same /EFI/BOOT/BOOTX64.EFI.prev "$W/rel6/EFI/BOOT/BOOTX64.EFI"
state_has "trial = 8 1"
state_has "prev = 6"

uboot bad2 \
    "update      : serial 8 on trial, start 2 of 2; it must reach an attach" \
    "update      : serial 8 is on trial; nothing new until it ends" \
    "no network boot: " \
    "not:passed its trial"
state_has "trial = 8 2"

uboot revert "${attached[@]}" \
    "update      : serial 8 started 2 times and never attached; putting serial 6 back" \
    "update      : 4 file(s) put back; starting the restored \\EFI\\BOOT\\BOOTX64.EFI" \
    "update      : serial 8 failed its trial on this medium; staying on serial 6 until a newer one" \
    "handed down : StormBootUpdate = failed:8  (attributes 0x6)" \
    "stub:\"update_serial\":6,\"update\":\"failed:8\"}" \
    "not:passed its trial"
same /EFI/BOOT/BOOTX64.EFI "$W/rel6/EFI/BOOT/BOOTX64.EFI"
same /stormboot/stormboot.conf "$W/rel6/stormboot/stormboot.conf"
same /startup.nsh "$W/rel6/startup.nsh"
same /stormboot/drivers/README.txt "$W/rel6/stormboot/drivers/README.txt"
state_has "serial = 6"
state_has "failed = 8"
