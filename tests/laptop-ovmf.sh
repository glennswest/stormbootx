#!/usr/bin/env bash
# stormbootx booting from the laptop app's target (#164): the real
# `stormboot-serve` (stormblock#474), not a stub of it.
#
#   tests/laptop-ovmf.sh GOLDEN.ISO PAYLOAD.EFI STORMBOOT-SERVE
#
# GOLDEN.ISO is a media golden's ISO as `deploy/build-golden.sh stormbootx`
# writes it: forge's portal, `dns =`, `syslog =` and stormcentral's
# `update =` in its stormboot.conf, `fallback = none`. The laptop app
# (storminstall#20) does not rebuild it. It adds two files to the ESP with a
# FAT writer, as this test does with mtools:
#
#   \stormboot\local.conf           read before stormboot.conf, so its keys
#                                   win: portal, api_port, name, syslog,
#                                   update = off
#   \stormboot\install-config.yaml  the node's install-config (#79)
#
# and serves one image with `stormboot-serve`. Here the image is a GPT disk
# at 4096-byte blocks whose ESP carries PAYLOAD (tcp4probe.efi), padded to
# 96 MiB, as BOOTX64.EFI; slirp maps the guest's 10.0.2.2 to the build box's
# loopback, where stormboot-serve listens and advertises 10.0.2.2.
#
# Six boots of the ISO under OVMF, as a BMC's virtual media would carry it,
# with stormboot-serve started afresh for each:
#   1. named:   local.conf `name = server8`, served `--host server8`,
#               intent install: the claim, the attach as
#               nqn.2026-09.lo.storm:host-server8, reads in flight, 96 MiB
#               read, the payload started and reading StormBootTag, the host
#               NQN and the install-config back; the log at the laptop's
#               collector under the name server8;
#   2. bymac:   no name on the media, served `--host server7=<the NIC's MAC>`:
#               the default claim by MAC boots server7's image;
#   3. local:   served `--intent local`: no claim, the boot falls through;
#   4. unknown: `name = server9`, which the laptop does not serve: the claim
#               is refused, nothing attached, and the golden's `hold = 120`
#               with no local disk holds with the API up until a command (#167);
#   5. record:  served `--host server7=<the NIC's MAC>`, no name on the media,
#               `command_key =` in local.conf, and a signed `hold` for server7
#               PUT on the laptop's command record (stormblock#494) before the
#               boot: the intent's host (server7) is a name of this machine, the
#               hold is taken before the claim and reported done; a signed
#               `poweroff` PUT while it holds is read from the record and ends
#               the VM. Nothing is claimed.
#   6. maint:   as record, the command a signed `maint`: the laptop serves
#               one image and no maintenance image, and stormboot-serve
#               answers a claim with `"image":"maint"` with server7's own
#               image (no `"image":"maint"` in its reply), so stormbootx must
#               not attach it: it holds, and a `poweroff` ends it (#167).
#
# Needs qemu-system-x86_64, OVMF, python3, mtools, mkfs.fat, curl. KVM when
# /dev/kvm is writable. Unprivileged; everything is under $TMPDIR.
set -euo pipefail

ISO=${1:?usage: $0 GOLDEN.ISO PAYLOAD.EFI STORMBOOT-SERVE}
PAYLOAD=${2:?usage: $0 GOLDEN.ISO PAYLOAD.EFI STORMBOOT-SERVE}
SERVE=${3:?usage: $0 GOLDEN.ISO PAYLOAD.EFI STORMBOOT-SERVE}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LIMIT=${LIMIT:-240}
START_LIMIT=${START_LIMIT:-60}
MAC=52:54:00:12:34:56
export MTOOLS_SKIP_CHECK=1

say() { printf 'laptop-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

command -v qemu-system-x86_64 >/dev/null || die "qemu-system-x86_64 is not installed"
[[ -r "$OVMF_CODE" ]] || die "no OVMF at $OVMF_CODE"
[[ -s "$ISO" ]] || die "no ISO at $ISO"
[[ -x "$SERVE" ]] || die "no stormboot-serve at $SERVE"

W=$(mktemp -d "${TMPDIR:-/tmp}/laptop-ovmf.XXXXXX")
SERVE_PID=""
SYSLOG_PID=""
cleanup() {
    [[ -n "$SERVE_PID" ]] && kill "$SERVE_PID" 2>/dev/null
    [[ -n "$SYSLOG_PID" ]] && kill "$SYSLOG_PID" 2>/dev/null
    rm -rf "$W"
}
trap cleanup EXIT

say "$("$SERVE" --version)"

# The laptop app's command key (#167): commands are signed with it, and the
# medium names its public half as `command_key =`.
openssl genpkey -algorithm ed25519 -out "$W/cmd.pem" 2>/dev/null
CMD_PUB=$(openssl pkey -in "$W/cmd.pem" -pubout -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n')
[[ ${#CMD_PUB} -eq 64 ]] || die "no Ed25519 public key"

# signed HOST ID COMMAND: the command's JSON body for the record's PUT.
signed() {
    local host=$1 id=$2 cmd=$3 issued=2026-10-22T03:00:00Z
    printf 'stormbootx-command v1\n%s\n%s\n%s\n\n%s\n' "$host" "$id" "$cmd" "$issued" > "$W/cmd.txt"
    openssl pkeyutl -sign -inkey "$W/cmd.pem" -rawin -in "$W/cmd.txt" -out "$W/cmd.sig"
    printf '{"host":"%s","id":%s,"command":"%s","args":"","issued":"%s","by":"laptop-test","sig":"%s"}' \
        "$host" "$id" "$cmd" "$issued" "$(od -An -tx1 "$W/cmd.sig" | tr -d ' \n')"
}

# put_command HOST ID COMMAND: PUT it on the laptop's record; dies unless 200.
put_command() {
    local code
    code=$(curl -s -m 10 -o "$W/put.json" -w '%{http_code}' -X PUT -H 'Content-Type: application/json' \
        --data-binary "$(signed "$1" "$2" "$3")" \
        "http://127.0.0.1:$HTTP_PORT/api/v1/synonyms/boothost/$1/command") || true
    [[ $code == 200 ]] || die "PUT $3 (id $2) on $1's record: HTTP $code $(cat "$W/put.json" 2>/dev/null)"
    say "PUT $1's command record: $(cut -c1-200 "$W/put.json")"
}

# The image the laptop serves.
cp "$PAYLOAD" "$W/payload.efi"
truncate -s 96M "$W/payload.efi"
mkfs.fat -C -S 4096 -F 16 -s 1 -n RELEASE "$W/esp4k.img" 131072 >/dev/null
mmd -i "$W/esp4k.img" ::/EFI ::/EFI/BOOT
mcopy -i "$W/esp4k.img" "$W/payload.efi" ::/EFI/BOOT/BOOTX64.EFI
python3 "$ROOT/tests/gpt4k.py" "$W/esp4k.img" "$W/release.img"
rm -f "$W/esp4k.img" "$W/payload.efi"
IMG_SUM=$(sha256sum < "$W/release.img" | cut -d' ' -f1)

# Two free TCP ports on the loopback, for NVMe/TCP and the boothost API.
read -r NVME_PORT HTTP_PORT < <(python3 - <<'PY'
import socket
s = [socket.socket() for _ in range(2)]
for x in s:
    x.bind(("127.0.0.1", 0))
print(*(x.getsockname()[1] for x in s))
PY
)

# The laptop app's log collector (#162): each datagram as `syslog <frame>`.
cat > "$W/syslog.py" <<'PY'
import socket, sys
log = open(sys.argv[1], "a", buffering=1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", 0))
open(sys.argv[2], "w").write(str(s.getsockname()[1]))
while True:
    m, _ = s.recvfrom(65535)
    log.write("syslog " + m.decode("utf-8", "replace").replace("\n", " ") + "\n")
PY
python3 "$W/syslog.py" "$W/syslog.log" "$W/syslog.port" &
SYSLOG_PID=$!
for _ in $(seq 50); do [[ -s "$W/syslog.port" ]] && break; sleep 0.1; done
SYSLOG_PORT=$(cat "$W/syslog.port" 2>/dev/null) || die "the syslog collector did not start"

# The node's install-config (#79), as the app writes it.
IC="$W/install-config.yaml"
printf 'apiVersion: v1\nmetadata:\n  name: laptopbench\nsystem_drive: /dev/sda\npullSecret: %s\n' \
    "'{\"auths\":{}}'" > "$IC"
IC_LEN=$(stat -c %s "$IC")
IC_SHA=$(sha256sum "$IC" | cut -d' ' -f1)

esp_offset() { # ISO: the byte offset of its 0xEF (ESP) partition
    python3 - "$1" <<'PY'
import struct, sys
mbr = open(sys.argv[1], "rb").read(512)
for i in range(4):
    e = mbr[446 + 16 * i: 462 + 16 * i]
    if e[4] == 0xEF:
        print(struct.unpack_from("<I", e, 8)[0] * 512)
        break
PY
}

accel=tcg
cpu=max
[[ -w /dev/kvm ]] && accel=kvm cpu=host

# laptop NAME 'SERVE ARGS' 'LOCAL.CONF LINES' EXPECTED...
#   EXPECTED: a console line; `serve:` in stormboot-serve's output; `log:` at
#   the collector; `not:`/`not-serve:` absent.
laptop() {
    local name=$1 serve_args=$2 conf=$3; shift 3
    [[ -z "${LAPTOP_ONLY:-}" || "$LAPTOP_ONLY" == "$name" ]] || return 0
    local iso="$W/$name.iso" log="$W/$name.serial" txt="$W/$name.txt"

    # The laptop: stormboot-serve on the loopback, advertising slirp's host.
    # shellcheck disable=SC2086
    "$SERVE" --image "$W/release.img" $serve_args --listen 127.0.0.1 \
        --nvme-port "$NVME_PORT" --http-port "$HTTP_PORT" --advertise 10.0.2.2 \
        --scratch "$W/scratch" > "$W/serve.log" 2>&1 &
    SERVE_PID=$!
    local i
    for i in $(seq 100); do
        curl -fsS "http://127.0.0.1:$HTTP_PORT/api/v1/health" > "$W/health.json" 2>/dev/null && break
        kill -0 "$SERVE_PID" 2>/dev/null || { cat "$W/serve.log"; die "[$name] stormboot-serve exited"; }
        sleep 0.1
    done
    (( i < 100 )) || die "[$name] stormboot-serve did not answer its health"
    say "[$name] stormboot-serve $serve_args on 127.0.0.1:$NVME_PORT/$HTTP_PORT: $(cat "$W/health.json" | cut -c1-160)"
    [[ -n "${BEFORE:-}" ]] && "$BEFORE"

    # The app's medium: the golden ISO, plus local.conf and install-config.yaml.
    cp "$ISO" "$iso"
    local off; off=$(esp_offset "$iso")
    [[ -n "$off" ]] || die "no 0xEF partition in $ISO's MBR"
    printf '# the laptop app (#164)\n%s\n' "$conf" > "$W/local.conf"
    mcopy -o -i "$iso@@$off" "$W/local.conf" ::/stormboot/local.conf || die "could not write local.conf"
    mcopy -o -i "$iso@@$off" "$IC" ::/stormboot/install-config.yaml || die "could not write install-config.yaml"
    say "[$name] local.conf: $(grep -v '^#' "$W/local.conf" | tr '\n' ';' | sed 's/;$//; s/;/; /g')"

    : > "$W/syslog.log"
    say "[$name] booting under OVMF ($accel, up to ${LIMIT}s)"
    local attempt=1 qemu t
    while :; do
        : > "$log"; : > "$W/$name.debug"
        cp "$OVMF_VARS" "$W/$name.vars"
        qemu-system-x86_64 -machine q35,accel="$accel" -cpu "$cpu" -m 1024 \
            -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
            -drive if=pflash,format=raw,file="$W/$name.vars" \
            -fw_cfg name=opt/org.tianocore/IPv4Support,string=no \
            -fw_cfg name=opt/org.tianocore/IPv6Support,string=no \
            -netdev user,id=n0 -device virtio-net-pci,netdev=n0,mac=$MAC,romfile= \
            -device virtio-scsi-pci,id=cdscsi \
            -drive if=none,id=cd0,media=cdrom,format=raw,readonly=on,file="$iso" \
            -device scsi-cd,drive=cd0,bus=cdscsi.0,bootindex=1 \
            -debugcon file:"$W/$name.debug" -global isa-debugcon.iobase=0x402 \
            -display none -serial file:"$log" -no-reboot &
        qemu=$! t=0
        local stuck=
        while kill -0 "$qemu" 2>/dev/null && (( t < LIMIT )); do
            if [[ -n "${DURING:-}" ]]; then
                "$DURING" "$log"
            else
                grep -qE "is there a TCP/IP stack in this firmware|no network boot: " "$log" 2>/dev/null \
                    && { sleep 2; break; }
            fi
            # #124: a boot that has not started stormbootx in a minute never does.
            if (( t == START_LIMIT )) && ! grep -q "NVMe/TCP boot extension" "$log" 2>/dev/null; then
                stuck=1; break
            fi
            sleep 1; t=$((t + 1))
        done
        kill "$qemu" 2>/dev/null || true; wait "$qemu" 2>/dev/null || true
        if [[ -n $stuck && $attempt == 1 ]]; then
            say "[$name] stormbootx has not started after ${START_LIMIT}s; the debug port's last lines (#124):"
            tr -d '\r' < "$W/$name.debug" | grep -v '^\s*$' | tail -20 | sed 's/^/  : /' || true
            say "[$name] booting it once more"
            attempt=2; : > "$W/syslog.log"
            continue
        fi
        break
    done
    say "[$name] stopped after ${t}s"
    # What the laptop holds after the boot, then the laptop stops.
    curl -fsS "http://127.0.0.1:$HTTP_PORT/api/v1/boothost" > "$W/boothost.json" 2>/dev/null || true
    kill "$SERVE_PID" 2>/dev/null || true; wait "$SERVE_PID" 2>/dev/null || true
    SERVE_PID=""

    tr -d '\r' < "$log" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' > "$txt"
    say "[$name] console:"
    grep -v '^\s*$' "$txt" | sed -n '1,120s/^/  | /p'
    say "[$name] stormboot-serve:"
    sed 's/\x1b\[[0-9;]*m//g; s/^/  > /' "$W/serve.log"
    sed -i 's/\x1b\[[0-9;]*m//g' "$W/serve.log"
    say "[$name] GET /api/v1/boothost: $(cut -c1-400 "$W/boothost.json")"
    [[ -n "${DURING:-}" ]] && (( t >= LIMIT )) && die "[$name] still running after ${LIMIT}s"
    say "[$name] log frames at the collector (#162): $(grep -c '^syslog ' "$W/syslog.log" || true)"

    local fail=0 want
    for want in "$@"; do
        case "$want" in
            serve:*) grep -qF -- "${want#serve:}" "$W/serve.log" && say "[$name] laptop saw: ${want#serve:}" \
                        || { say "[$name] laptop missing: ${want#serve:}"; fail=1; } ;;
            not-serve:*) grep -qF -- "${want#not-serve:}" "$W/serve.log" \
                        && { say "[$name] laptop unexpectedly saw: ${want#not-serve:}"; fail=1; } \
                        || say "[$name] laptop never saw, as it should be: ${want#not-serve:}" ;;
            boothost:*) grep -qF -- "${want#boothost:}" "$W/boothost.json" && say "[$name] boothost list: ${want#boothost:}" \
                        || { say "[$name] boothost list missing: ${want#boothost:}"; fail=1; } ;;
            log:*) grep -qF -- "${want#log:}" "$W/syslog.log" && say "[$name] log frame: ${want#log:}" \
                        || { say "[$name] log frame missing: ${want#log:}"; fail=1; } ;;
            not:*) grep -qF -- "${want#not:}" "$txt" && { say "[$name] unexpected: ${want#not:}"; fail=1; } \
                        || say "[$name] absent, as it should be: ${want#not:}" ;;
            *) grep -qF -- "$want" "$txt" && say "[$name] found: $want" \
                        || { say "[$name] missing: $want"; fail=1; } ;;
        esac
    done
    if [[ $fail -ne 0 ]]; then
        say "[$name] the firmware's debug port, last 40 lines:"
        tr -d '\r' < "$W/$name.debug" | grep -v '^\s*$' | tail -40 | sed 's/^/  : /' || true
        die "[$name] expected lines missing"
    fi
}

app_conf() { # [name = …]
    printf 'portal   = 10.0.2.2\napi_port = %s\n%ssyslog   = 10.0.2.2:%s\nupdate   = off\nntp      = off\n' \
        "$HTTP_PORT" "${1:+$1$'\n'}" "$SYSLOG_PORT"
}

attached=(
    "engine      : stormblock 2"
    "  (universal boot)"
    "intent      : install"
    "  portal    : 10.0.2.2:$NVME_PORT  nsid "
    "x 4096 bytes"
    " 8 in flight"
    "blockio     : 96 MiB read in all"
    "RESULT: image attached; starting its bootloader."
    "is there a TCP/IP stack in this firmware"
    "handoff     : StormBootInstallConfig ($IC_LEN bytes in 1 chunk(s)) = v1:$IC_LEN:1:$IC_SHA"
    "install cfg : $IC_LEN bytes reassembled from 1 chunk(s), length and sha256 match"
    "not:no network boot"
    "not:update      : ->"
    "serve:(stormbootx"
    "log:syslog <134>1 - "
)

laptop named "--host server8" "$(app_conf 'name     = server8')" "${attached[@]}" \
    "claim       : boothost/server8 at " \
    "service tag : server8  (stated in \\stormboot\\local.conf)" \
    "(syslog in \\stormboot\\local.conf) as server8" \
    "update      : off (update = off); this medium is pinned" \
    "  claimed a clone of server8's image" \
    "handed down : StormBootTag = server8  (attributes 0x6)" \
    "handed down : StormBootHostNqn = nqn.2026-09.lo.storm:host-server8  (attributes 0x6)" \
    "serve:claim of boothost/server8 by 127.0.0.1:" \
    "serve:: server8, nsid 1, intent install" \
    "boothost:\"name\":\"server8\"" \
    "boothost:\"volume\":\"stormboot-server8-1\"" \
    "log: server8 stormbootx - - - RESULT: image attached"

laptop bymac "--host server7=$MAC" "$(app_conf)" "${attached[@]}" \
    "claim       : boothost/default as $MAC at " \
    "  booting server7's image (this MAC is its alias)" \
    "handed down : StormBootTag = server7  (attributes 0x6)" \
    "handed down : StormBootHostNqn = nqn.2026-09.lo.storm:host-server7  (attributes 0x6)" \
    "serve:claim of boothost/default by 127.0.0.1:" \
    "serve:: server7, nsid 1, intent install" \
    "log: server7 stormbootx - - - RESULT: image attached"

laptop local "--host server8 --intent local" "$(app_conf 'name     = server8')" \
    "engine      : stormblock 2" \
    "intent      : local" \
    "no network boot: boot intent for server8 is \`local\`: nothing claimed" \
    "not:claim       : boothost/" \
    "not:RESULT: image attached" \
    "not-serve:claim of"

laptop unknown "--host server8" "$(app_conf 'name     = server9')" \
    "claim       : boothost/server9 at " \
    "no network boot: no claim gave this machine an image, and the media names no fallback (fallback = none)" \
    "hold        : holding until a command; the API at http://" \
    "not:RESULT: image attached" \
    "not-serve:claim of"

# record: the laptop's command record (stormblock#494), as the app writes it.
record_before() { put_command server7 7 hold; }
RECORD_POWEROFF=
record_during() { # the hold taken: put a poweroff for the hold to read
    if [[ -z $RECORD_POWEROFF ]] && grep -q "hold        : holding until a command" "$1" 2>/dev/null; then
        RECORD_POWEROFF=1
        put_command server7 8 poweroff
    fi
}
BEFORE=record_before DURING=record_during \
laptop record "--host server7=$MAC" "$(app_conf "command_key = $CMD_PUB")" \
    "engine      : stormblock 2" \
    "command     : hold (id 7, from the engine's record, by laptop-test), signature good" \
    "command     : hold (id 7): holding until another command" \
    "hold        : holding until a command; the API at http://" \
    "command     : poweroff (id 8, from the engine's record, by laptop-test), signature good" \
    "command     : poweroff (id 8): powering off (ACPI S5)" \
    "serve:server7: command hold  put (id 7)" \
    "serve:server7: command 7 done: holding until another command" \
    "serve:server7: command 8 done: powering off" \
    "not:RESULT: image attached" \
    "not-serve:claim of"

# maint: the laptop has no maintenance image (stormblock#497 is forge's).
maint_before() { put_command server7 7 maint; }
MAINT_POWEROFF=
maint_during() {
    if [[ -z $MAINT_POWEROFF ]] && grep -q "hold        : holding until a command" "$1" 2>/dev/null; then
        MAINT_POWEROFF=1
        put_command server7 8 poweroff
    fi
}
BEFORE=maint_before DURING=maint_during \
laptop maint "--host server7=$MAC" "$(app_conf "command_key = $CMD_PUB")" \
    "command     : maint (id 7, from the engine's record, by laptop-test), signature good" \
    "command     : maint (id 7): claiming the maintenance image (boothost/maint)" \
    "intent      : maint  (a command said so; the engine's is " \
    "  (the maintenance image)" \
    "  the engine answered with server7's own image, not the maintenance image (no \"image\":\"maint\" in the reply: an engine before stormblock#497); not attached" \
    "no network boot: a command said maint, and the engine answered with this machine's own image" \
    "command     : poweroff (id 8): powering off (ACPI S5)" \
    "serve:server7: command 7 done: claiming the maintenance image (boothost/maint)" \
    "not:RESULT: image attached" \
    "not:attaching   : "

# The image is never written: every write a node makes lands in its overlay.
[[ $(sha256sum < "$W/release.img" | cut -d' ' -f1) == "$IMG_SUM" ]] || die "the served image changed"
say "the served image is unchanged (sha256 ${IMG_SUM:0:16}…)"
say "PASS"
