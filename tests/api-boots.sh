# stormbootx's own API and the hold (#167), sourced by tests/net-ovmf.sh for
# its stubs (the engine on $PORT, the SNTP server) and its helpers.
#
# Three boots, the API forwarded to the host by slirp (127.0.0.1:$APIP to the
# guest's 9180). Commands are signed with a key made here, named on the
# medium as `command_key =`, as the laptop app does:
#
#   api        the claim 404s and the medium names no fallback, so the boot
#              fails; `hold = 30`, but the only local disk (APIDISK1, a GPT
#              with no ESP) boots nothing, so it holds until a command.
#              GET status, inventory and log; a body that is not a command,
#              another key's signature and another machine's command are
#              refused; `format APIDISK1` wipes the disk's first and last MiB
#              (and nothing between); the same id again is refused; a serial
#              nothing has and `boot-next FFFF` are refused; `maint` claims
#              with `"image":"maint"`, the engine has no boothost/maint, and
#              it holds again; the inventory is collected again, the disk
#              now blank; `poweroff` ends the VM.
#   apirecord  the same machine (its variable store kept), the engine's host
#              record carrying a signed `hold`: taken before the intent and
#              reported done to the engine; the API refuses boot 1's last id;
#              `boot-next 0000` sets BootNext and resets.
#   apimaint   the engine's record carrying a signed `maint` and the engine
#              serving boothost/maint (stormblock#497): taken before the
#              intent, the claim carries `"image":"maint"`, never the
#              media's fallback, and the maintenance clone is attached and
#              its BOOTX64.EFI started.

[[ -z "${NET_ONLY:-}" || "$NET_ONLY" == api || "$NET_ONLY" == apirecord || "$NET_ONLY" == apimaint ]] || return 0
command -v openssl >/dev/null || die "[api] openssl is not installed"
command -v curl >/dev/null || die "[api] curl is not installed"
command -v sfdisk >/dev/null || die "[api] sfdisk is not installed"

openssl genpkey -algorithm ed25519 -out "$W/cmd.pem" 2>/dev/null
openssl genpkey -algorithm ed25519 -out "$W/other-cmd.pem" 2>/dev/null
CMD_PUB=$(openssl pkey -in "$W/cmd.pem" -pubout -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n')
[[ ${#CMD_PUB} -eq 64 ]] || die "[api] no Ed25519 public key"
APIP=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
ME=mac-525400123456

# signed KEY HOST ID COMMAND [ARGS]: the command's JSON body.
signed() {
    local key=$1 host=$2 id=$3 cmd=$4 args=${5:-} issued=2026-10-22T03:00:00Z
    printf 'stormbootx-command v1\n%s\n%s\n%s\n%s\n%s\n' "$host" "$id" "$cmd" "$args" "$issued" > "$W/cmd.txt"
    openssl pkeyutl -sign -inkey "$key" -rawin -in "$W/cmd.txt" -out "$W/cmd.sig"
    printf '{"host":"%s","id":%s,"command":"%s","args":"%s","issued":"%s","by":"api-test","sig":"%s"}' \
        "$host" "$id" "$cmd" "$args" "$issued" "$(od -An -tx1 "$W/cmd.sig" | tr -d ' \n')"
}

# api_get PATH: the body; api_post BODY: the status, the body in $W/resp.
api_get() { curl -s -m 10 "http://127.0.0.1:$APIP$1"; }
api_post() {
    curl -s -m 30 -o "$W/resp" -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
        --data-binary "$1" "http://127.0.0.1:$APIP/api/v1/command" || true
}

# expect_post NAME CODE TEXT BODY: the API answers CODE with TEXT in the body.
expect_post() {
    local name=$1 code=$2 text=$3 got
    got=$(api_post "$4")
    if [[ $got == "$code" ]] && grep -qF -- "$text" "$W/resp"; then
        say "[$name] HTTP $got: $(cut -c1-160 "$W/resp")"
    else
        say "[$name] expected HTTP $code with \"$text\", got $got: $(cat "$W/resp" 2>/dev/null)"
        api_fail "$name"
    fi
}

# console_wait NAME TEXT [SECS]: until the serial console says TEXT.
console_wait() {
    local name=$1 text=$2 t=0
    while (( t < ${3:-30} )); do
        tr -d '\r' < "$W/$name.serial" | grep -qF -- "$text" && { say "[$name] console: $text"; return 0; }
        sleep 1; t=$((t + 1))
    done
    say "[$name] the console never said: $text"
    api_fail "$name"
}

api_fail() {
    say "[$1] serial console, last 60 lines:"
    tr -d '\r' < "$W/$1.serial" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' | grep -v '^\s*$' | tail -60 | sed 's/^/  | /'
    say "[$1] stub log:"; sed 's/^/  > /' "$W/stub.log"
    [[ -n "${API_QEMU:-}" ]] && kill "$API_QEMU" 2>/dev/null
    die "[$1] the API test failed"
}

# api_boot NAME: start the VM in the background (API_QEMU) and wait for the
# hold.
api_boot() {
    local name=$1 iso="$W/$1.iso"
    "$ROOT/scripts/build-boot-agent.sh" --iso --binary "$EFI" --engine 10.0.2.2 --api-port "$PORT" \
        --ntp "10.0.2.2:$NTP_PORT" --no-fallback --hold 30 --command-key "$CMD_PUB" --output "$iso" >/dev/null
    : > "$W/stub.log"
    : > "$W/$name.serial"
    say "[$name] booting under OVMF ($accel), the API at 127.0.0.1:$APIP"
    qemu-system-x86_64 -machine q35,accel="$accel" -cpu "$host_cpu" -m 1024 \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$W/api.vars" \
        -fw_cfg name=opt/org.tianocore/IPv4Support,string=no \
        -fw_cfg name=opt/org.tianocore/IPv6Support,string=no \
        -netdev user,id=n0,hostfwd=tcp:127.0.0.1:$APIP-:9180 -device virtio-net-pci,netdev=n0,romfile= \
        -device virtio-scsi-pci,id=cdscsi \
        -drive if=none,id=cd0,media=cdrom,format=raw,readonly=on,file="$iso" \
        -device scsi-cd,drive=cd0,bus=cdscsi.0,bootindex=1 \
        -drive if=none,id=ad,format=raw,file="$W/apidisk.img" \
        -device scsi-hd,drive=ad,bus=cdscsi.0,serial=APIDISK1 \
        -display none -serial file:"$W/$name.serial" -no-reboot &
    API_QEMU=$!
}

# The local disk: a GPT with one Linux partition, a marker in the middle.
truncate -s 64M "$W/apidisk.img"
printf 'label: gpt\n,16M,L\n' | sfdisk -q "$W/apidisk.img"
printf 'KEEP-THIS-MIDDLE' | dd of="$W/apidisk.img" bs=1M seek=32 conv=notrunc status=none
cp "$OVMF_VARS" "$W/api.vars"
rm -f "$W/command.json" "$W/maint.ok"

# ---------------------------------------------------------------- boot 1
if [[ -z "${NET_ONLY:-}" || "$NET_ONLY" == api ]]; then
api_boot api
console_wait api "api         : port 9180 once a NIC holds a lease; commands signed by 1 release key(s), command_key in " 90
console_wait api "hold        : nothing local to fall through to (" 120
console_wait api "hold        : holding until a command; the API at http://10.0.2.15:9180" 10

status=$(api_get /api/v1/status)
say "[api] status: $status"
for want in '"phase":"holding"' "\"$ME\"" '"addresses":["10.0.2.15"]' '"agent":"stormbootx"'; do
    [[ $status == *"$want"* ]] || { say "[api] status lacks $want"; api_fail api; }
done
inv=$(api_get /api/v1/inventory)
[[ $inv == *'"serial":"APIDISK1"'* ]] && say "[api] inventory lists APIDISK1 ($(printf %s "$inv" | wc -c) bytes)" \
    || { say "[api] inventory lacks APIDISK1: ${inv:0:300}"; api_fail api; }
log=$(api_get /api/v1/log)
[[ $log == *"no network boot: no claim gave this machine an image"* && $log == *"NVMe/TCP boot extension"* ]] \
    && say "[api] log: $(printf %s "$log" | wc -l) lines, from the banner to the hold" \
    || { say "[api] the log lacks the banner or the failure"; api_fail api; }
[[ $(curl -s -m 10 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$APIP/nothing") == 404 ]] \
    && say "[api] an unknown path is a 404" || api_fail api

expect_post api 400 '"error":"no \"host\""' '{"command":"reboot"}'
expect_post api 403 "the signature is not by any of" "$(signed "$W/other-cmd.pem" $ME 50 reboot)"
expect_post api 403 "it is for server9" "$(signed "$W/cmd.pem" server9 51 reboot)"
expect_post api 202 '"accepted":100,"command":"format","when":"now"' "$(signed "$W/cmd.pem" $ME 100 format APIDISK1)"
console_wait api "command     : format (id 100): wiped scsi QEMU QEMU HARDDISK serial APIDISK1 (64 MiB): 1 MiB at each end zeroed" 60
expect_post api 403 "id 100 is not above 100" "$(signed "$W/cmd.pem" $ME 100 format APIDISK1)"
expect_post api 202 '"accepted":101' "$(signed "$W/cmd.pem" $ME 101 format NOSUCH)"
console_wait api "command     : format (id 101): refused: no drive has serial NOSUCH (serials here: "
expect_post api 202 '"accepted":102' "$(signed "$W/cmd.pem" $ME 102 boot-next FFFF)"
console_wait api "command     : boot-next (id 102): refused: there is no BootFFFF"
expect_post api 202 '"accepted":103' "$(signed "$W/cmd.pem" $ME 103 maint)"
console_wait api "command     : maint (id 103): claiming the maintenance image (boothost/maint)"
console_wait api "intent      : maint  (a command said so; the engine's is auto): the maintenance image" 60
console_wait api "no boothost/maint synonym on this engine" 60
console_wait api "no network boot: a command said maint, and no claim gave this machine the maintenance image" 60
console_wait api "hold        : holding until a command; the API at http://10.0.2.15:9180" 30
grep -q '^POST /api/v1/synonyms/boothost/default/claim .*"image":"maint"' "$W/stub.log" \
    && say "[api] the maint claim went to the engine: $(grep -m1 '"image":"maint"' "$W/stub.log" | cut -c1-80)…" \
    || { say "[api] no claim with \"image\":\"maint\""; api_fail api; }
tr -d '\r' < "$W/api.serial" | grep -q "falling back to the resolved target" \
    && { say "[api] the maint claim fell back to the media's target"; api_fail api; }
expect_post api 202 '"accepted":104' "$(signed "$W/cmd.pem" $ME 104 inventory)"
console_wait api "command     : inventory (id 104): collected again: "
status=$(api_get /api/v1/status)
[[ $status == *'"command_id":104'*'"last":{"id":104,"command":"inventory"'* ]] \
    && say "[api] status after: ${status:0:400}" || { say "[api] status: $status"; api_fail api; }
api_get /api/v1/inventory | python3 -c '
import json, sys
d = [x for x in json.load(sys.stdin)["disks"] if x.get("serial") == "APIDISK1"][0]
print("net-ovmf: [api] APIDISK1 after the wipe: table %s" % d.get("table"))
sys.exit(0 if d.get("table") == "none" else 1)' || api_fail api
expect_post api 202 '"accepted":105' "$(signed "$W/cmd.pem" $ME 105 poweroff)"
console_wait api "command     : poweroff (id 105): powering off (ACPI S5)"
for _ in $(seq 20); do kill -0 "$API_QEMU" 2>/dev/null || break; sleep 1; done
kill -0 "$API_QEMU" 2>/dev/null && { say "[api] the VM is still running 20 s after poweroff"; api_fail api; }
wait "$API_QEMU" 2>/dev/null || true
say "[api] the VM powered off"

# The wipe: the first and last MiB zero, the middle as it was.
python3 - "$W/apidisk.img" <<'PY' || die "[api] the disk is not as a fast wipe leaves it"
import sys
d = open(sys.argv[1], "rb").read()
mib = 1 << 20
head, tail = d[:mib], d[-mib:]
mid = d[32 * mib:32 * mib + 16]
print("net-ovmf: [api] disk: first MiB %s, last MiB %s, middle %r" % (
    "zero" if not any(head) else "NOT zero", "zero" if not any(tail) else "NOT zero", mid))
sys.exit(0 if not any(head) and not any(tail) and mid == b"KEEP-THIS-MIDDLE" else 1)
PY
fi

# ---------------------------------------------------------------- boot 2
if [[ -z "${NET_ONLY:-}" || "$NET_ONLY" == apirecord ]]; then
signed "$W/cmd.pem" $ME 200 hold > "$W/command.json"
api_boot apirecord
console_wait apirecord "command     : hold (id 200, from the engine's record, by api-test), signature good" 120
console_wait apirecord "command     : hold (id 200): holding until another command"
console_wait apirecord "hold        : holding until a command; the API at http://10.0.2.15:9180" 10
grep -q "POST /api/v1/synonyms/boothost/.*/command/done {\"id\":200,\"result\":\"holding until another command\"}" "$W/stub.log" \
    && say "[apirecord] the engine was told: $(grep -m1 'command/done' "$W/stub.log")" \
    || { say "[apirecord] no done for id 200 to the engine"; api_fail apirecord; }
grep -q "^POST .*/claim" "$W/stub.log" && { say "[apirecord] it claimed after a hold"; api_fail apirecord; }
expect_post apirecord 403 "id 105 is not above 200" "$(signed "$W/cmd.pem" $ME 105 poweroff)"
rm -f "$W/command.json"
expect_post apirecord 202 '"accepted":201' "$(signed "$W/cmd.pem" $ME 201 boot-next 0000)"
console_wait apirecord "command     : boot-next (id 201): BootNext = Boot0000 \""
for _ in $(seq 20); do kill -0 "$API_QEMU" 2>/dev/null || break; sleep 1; done
kill -0 "$API_QEMU" 2>/dev/null && { say "[apirecord] still running after boot-next"; api_fail apirecord; }
wait "$API_QEMU" 2>/dev/null || true
say "[apirecord] BootNext set and reset: $(tr -d '\r' < "$W/apirecord.serial" | grep -m1 'BootNext = ')"
fi

# ---------------------------------------------------------------- boot 3
if [[ -z "${NET_ONLY:-}" || "$NET_ONLY" == apimaint ]]; then
signed "$W/cmd.pem" $ME 300 maint > "$W/command.json"
: > "$W/maint.ok"
api_boot apimaint
console_wait apimaint "command     : maint (id 300, from the engine's record, by api-test), signature good" 120
console_wait apimaint "command     : maint (id 300): claiming the maintenance image (boothost/maint)"
console_wait apimaint "claim       : boothost/default as 52:54:00:12:34:56 at 10.0.2.2:$PORT  (the maintenance image)" 30
console_wait apimaint "  claimed mac-525400123456's clone of the maintenance image" 60
console_wait apimaint "boot        : starting \EFI\BOOT\BOOTX64.EFI from the attached image" 120
grep -q "command/done {\"id\":300,\"result\":\"claiming the maintenance image (boothost/maint)\"}" "$W/stub.log" \
    && say "[apimaint] the engine was told: $(grep -m1 'command/done' "$W/stub.log")" \
    || { say "[apimaint] no done for id 300 to the engine"; api_fail apimaint; }
grep -q '^POST .*/claim .*"image":"maint"' "$W/stub.log" || { say "[apimaint] no maint claim"; api_fail apimaint; }
grep '^POST .*/claim' "$W/stub.log" | grep -vq '"image":"maint"' \
    && { say "[apimaint] a claim without \"image\":\"maint\""; api_fail apimaint; }
kill "$API_QEMU" 2>/dev/null; wait "$API_QEMU" 2>/dev/null || true
rm -f "$W/command.json" "$W/maint.ok"
say "[apimaint] the maintenance image attached and its bootloader started"
fi
API_QEMU=
