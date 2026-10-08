#!/usr/bin/env bash
# stormbootx's own TCP/IP under OVMF, with no firmware network stack (#56).
#
#   tests/net-ovmf.sh STORMBOOTX.EFI PAYLOAD.EFI
#
# Fedora's OVMF has SNP (virtio-net) and no MNP/IP4/TCP4, and the fw_cfg
# switches below turn the IPv4/IPv6 stacks off on an OVMF that has them, so
# nothing but stormbootx's smoltcp can reach the network. Two stubs run on the
# build box (slirp maps the guest's 10.0.2.2 to the host's loopback):
#
#   - an engine: /api/v1/health answers with a ~120 KB body, so the reply
#     spans many segments and windows. On the first boot claims get 404, so
#     the boot falls back to the target on the media; on the second the
#     default claim answers with the NVMe stub and host `stubhost`, so the
#     claimed name is handed down (#76);
#   - an NVMe/TCP target (the subset nvme.rs speaks) serving a GPT disk at
#     4096-byte blocks whose ESP carries tcp4probe.efi, padded to 96 MiB, as
#     BOOTX64.EFI. The firmware's FAT reads all of it through stormbootx's
#     BlockIO, so the boot moves 96 MiB over smoltcp (and prints blockio's
#     MiB/s line), then starts the payload.
#
#   - an SNTP server (#77) on UDP, named on the media with `ntp =`. On the
#     first boot it answers 2031-05-04 03:02:01 UTC, so stormbootx must set
#     the RTC there and hand `StormBootClock = synced:10.0.2.2` down, and the
#     payload must read the RTC back in 2031. On the second it answers as an
#     unsynchronised server (LI 3), which must not set anything:
#     `StormBootClock = unsynced`.
#
# Seven boots, and three more with stormnic-virtio (#108, below):
#   1. as shipped: `rng : firmware` or `rdrand`;
#   2. the entropy fallback: `rng = cpu` on the media masks the firmware's
#      EFI_RNG, and the CPU is started without RDRAND and RDSEED, so the
#      boot must say `rng : jitter` and still lease, connect and fetch. This
#      boot's claim succeeds, and the payload (tcp4probe) must read back
#      `StormBootTag = stubhost` and the host NQN at attributes 0x6, the
#      volatile `BOOTSERVICE_ACCESS | RUNTIME_ACCESS` Linux reads (#76).
#   3. a new machine (#15): the default claim by MAC answers the
#      provisional host `mac-525400123456`, which is booted and handed down;
#   4. an attach that boots nothing (#54): a blank namespace from a second
#      NVMe stub and a blank local virtio-scsi disk. The fall-through must
#      withdraw the attached disk, count one local disk, and the firmware
#      must try its other boot options for 25 s with no CPU exception.
#   5. a NIC on a dead hub (#88): one `waiting for a lease` line a second,
#      with DHCP out/in and frames in, until the engine's 30 s connect gives
#      up; and every boot names the NIC's driver before its first SNP call.
#   6. the boot intent (#11): the stub engine says `local` for this MAC,
#      and the boot must fall through with no claim POST (boot 2 is told
#      `install` and claims);
#   (#42) `bridge`: `esp = stormbootx` on the media, so stormbootx reads the
#      attached ESP itself, puts its read-only filesystem on the ESP's
#      partition handle, and the payload must find it as its own volume;
#   7. a fall-through gives the NIC back (#68): the firmware's own IPv4
#      stack is on (with a virtio-rng, which its drivers need), the claim
#      404s and the medium names no fallback namespace, as the goldens don't
#      (#36), so stormbootx falls through with nothing attached
#      and must say `1 of 1 NIC(s) given back`; the next boot option, the
#      firmware's PXE on the same NIC, must then lease from slirp and start
#      the payload over TFTP.
#
# Boots 1 and 2 must show the stack, a lease from slirp, the engine's version from
# the stub, the attach, a blockio progress line and the payload's banner; the
# engine stub must log the health GET and a claim POST.
#
# PAYLOAD is any EFI application that prints; the sc-build command uses
# tcp4probe.efi and looks for its banner.
#
# With UPDATE_KEY set (an Ed25519 PEM whose public half STORMBOOTX.EFI was
# built with, as STORMBOOTX_UPDATE_TEST_KEY), the two boots above are
# replaced by the self-update's boots off a writable disk (#83,
# tests/update-boots.sh; tests/update-ovmf.sh sets it all up).
#
# Needs qemu-system-x86_64, OVMF, python3, mtools, mkfs.fat, xorriso. KVM when
# /dev/kvm is writable, TCG otherwise. Unprivileged; everything is under $TMPDIR
# and deleted on exit.
set -euo pipefail

EFI=${1:?usage: $0 STORMBOOTX.EFI PAYLOAD.EFI}
PAYLOAD=${2:?usage: $0 STORMBOOTX.EFI PAYLOAD.EFI}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LIMIT=${LIMIT:-240}
VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
export MTOOLS_SKIP_CHECK=1

say() { printf 'net-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

command -v qemu-system-x86_64 >/dev/null || die "qemu-system-x86_64 is not installed"
command -v python3 >/dev/null || die "python3 is not installed"
[[ -r "$OVMF_CODE" ]] || die "no OVMF at $OVMF_CODE"
[[ -s "$EFI" ]] || die "no binary at $EFI"

W=$(mktemp -d "${TMPDIR:-/tmp}/net-ovmf.XXXXXX")
STUB_PID=""
NVME_PID=""
BLANK_PID=""
NTP_PID=""
cleanup() {
    [[ -n "$STUB_PID" ]] && kill "$STUB_PID" 2>/dev/null
    [[ -n "$NTP_PID" ]] && kill "$NTP_PID" 2>/dev/null
    [[ -n "$NVME_PID" ]] && kill "$NVME_PID" 2>/dev/null
    [[ -n "$BLANK_PID" ]] && kill "$BLANK_PID" 2>/dev/null
    rm -rf "$W"
}
trap cleanup EXIT

# The stub engine. Port 0: the kernel picks one, and the stub writes it down.
cat > "$W/stub.py" <<'PY'
import http.server, json, os, sys
log = open(sys.argv[1], "a", buffering=1)
W = os.path.dirname(sys.argv[1])
class H(http.server.BaseHTTPRequestHandler):
    def reply(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)
    def do_GET(self):
        log.write("GET %s\n" % self.path)
        # The self-update's boothelper (#83): whatever is published in serve/.
        pre = "/api/v1/boothelpers/stormbootx-test/"
        if self.path.startswith(pre):
            rel = self.path[len(pre):]
            f = os.path.join(W, "serve", rel)
            if ".." in rel or not os.path.isfile(f):
                self.reply(404, {"error": "stub: no release"})
                return
            data = open(f, "rb").read()
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(data)
            return
        if self.path == "/api/v1/health":
            self.reply(200, {"version": "19.4.0", "status": "ok", "pad": "x" * 120000})
        # intent present (#11): the host this MAC is an alias of has that
        # intent, in stormblock's `intent_body` shape (sorted keys, read by
        # the alias); every other name is the engine's 404.
        elif (self.path == "/api/v1/synonyms/boothost/525400123456/intent"
              and os.path.exists(W + "/intent")):
            self.reply(200, {"host": "stubhost", "intent": open(W + "/intent").read().strip(),
                             "resolved_from": "525400123456", "updated_at": 1790553600})
        else:
            self.reply(404, {"error": "stub: not found"})
    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n).decode(errors="replace")
        log.write("POST %s %s\n" % (self.path, body))
        # claim.ok present: the default claim names this machine `stubhost`
        # and attaches the NVMe stub, the shape stormblock's claim reply has.
        # claim.new present (#15): a machine the engine has never seen, so it
        # mints the provisional host `mac-<hex>` from the default.
        new = os.path.exists(W + "/claim.new")
        if self.path.endswith("/default/claim") and (new or os.path.exists(W + "/claim.ok")):
            port = int(open(W + "/nvme.port").read())
            name = "mac-525400123456" if new else "stubhost"
            self.reply(200, {
                "attach": {"addresses": [{"traddr": "10.0.2.2", "trsvcid": str(port)}],
                           "nqn": "nqn.2026-09.lo.stub:release", "nsid": 1},
                "host": {"aliases": ["52:54:00:12:34:56"] if new else [], "claimed_as": "default",
                         "mac": "52:54:00:12:34:56", "name": name, "new": new, "provisional": new},
            })
            return
        self.reply(404, {"error": "stub: no such host"})
    def log_message(self, *a):
        pass
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
open(sys.argv[2], "w").write(str(s.server_address[1]))
s.serve_forever()
PY
python3 "$W/stub.py" "$W/stub.log" "$W/stub.port" &
STUB_PID=$!
for _ in $(seq 50); do [[ -s "$W/stub.port" ]] && break; sleep 0.1; done
PORT=$(cat "$W/stub.port" 2>/dev/null) || die "the stub engine did not start"
say "stub engine on 127.0.0.1:$PORT (10.0.2.2:$PORT from the guest)"

# The disk: a 128 MiB FAT16 at 4096-byte sectors (the shape stormblock lays
# on a 4K release), the payload padded to 96 MiB, in a GPT at 4096-byte blocks.
cp "$PAYLOAD" "$W/payload.efi"
truncate -s 96M "$W/payload.efi"
mkfs.fat -C -S 4096 -F 16 -s 1 -n RELEASE "$W/esp4k.img" 131072 >/dev/null
mmd -i "$W/esp4k.img" ::/EFI ::/EFI/BOOT
mcopy -i "$W/esp4k.img" "$W/payload.efi" ::/EFI/BOOT/BOOTX64.EFI
python3 - "$W/esp4k.img" "$W/disk4k.img" <<'PY'
import struct, sys, uuid, zlib
esp, out = sys.argv[1], sys.argv[2]
bs = 4096
data = open(esp, 'rb').read()
first = (1 << 20) // bs
last = first + -(-len(data) // bs) - 1
total = last + 1 + 1 + 4 + 256
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

# The NVMe/TCP target: ICReq, Fabrics Connect, Property Get/Set, Identify
# (namespace and controller) and Read, which is all nvme.rs sends on a boot.
# Reads go back as 32 KiB C2HData PDUs, the last one flagged SUCCESS.
cat > "$W/nvme.py" <<'PY'
import socket, socketserver, struct, sys
disk = open(sys.argv[1], 'rb').read()
BS = 4096
state = {"cc": 0}
def pdu(t, flags, hlen, pdo, body):
    return struct.pack('<BBBBI', t, flags, hlen, pdo, 8 + len(body)) + body
def resp(cid, dw0=0, dw1=0, status=0):
    return pdu(0x05, 0, 24, 0, struct.pack('<IIHHHH', dw0, dw1, 0, 0, cid, status))
def c2h(cid, data):
    out, off, step = b'', 0, 32768
    while True:
        chunk = data[off:off + step]
        last = off + step >= len(data)
        hdr = struct.pack('<HHIII', cid, 0, off, len(chunk), 0)
        out += pdu(0x07, 0x0C if last else 0, 24, 24, hdr + chunk)
        off += step
        if last:
            return out
class H(socketserver.BaseRequestHandler):
    def read(self, n):
        b = b''
        while len(b) < n:
            c = self.request.recv(n - len(b))
            if not c:
                raise EOFError
            b += c
        return b
    def handle(self):
        s = self.request
        s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        try:
            while True:
                t, flags, hlen, pdo, plen = struct.unpack('<BBBBI', self.read(8))
                rest = self.read(plen - 8)
                if t == 0x00:
                    s.sendall(pdu(0x01, 0, 128, 0, struct.pack('<HBBI', 0, 0, 0, 131072).ljust(120, b'\0')))
                    continue
                if t != 0x04:
                    return
                sqe = rest[:64]
                opc, cid, nsid = sqe[0], struct.unpack_from('<H', sqe, 2)[0], struct.unpack_from('<I', sqe, 4)[0]
                if opc == 0x7F:
                    fct = sqe[4]
                    ofst = struct.unpack_from('<I', sqe, 44)[0]
                    if fct == 0x01:
                        s.sendall(resp(cid, 1))
                    elif fct == 0x04:
                        v = {0x00: 127 | 1 << 16 | 1 << 24 | 1 << 37, 0x08: 0x10400,
                             0x14: state["cc"], 0x1C: state["cc"] & 1}.get(ofst, 0)
                        s.sendall(resp(cid, v & 0xFFFFFFFF, v >> 32))
                    elif fct == 0x00:
                        v = struct.unpack_from('<Q', sqe, 48)[0]
                        if ofst == 0x14:
                            state["cc"] = v
                        s.sendall(resp(cid))
                    else:
                        s.sendall(resp(cid, status=0x01 << 1))
                elif opc == 0x06:
                    cns = sqe[40]
                    d = bytearray(4096)
                    if cns == 0:
                        n = len(disk) // BS
                        struct.pack_into('<QQQ', d, 0, n, n, n)
                        d[130] = 12
                    else:
                        d[77] = 5
                    s.sendall(c2h(cid, bytes(d)))
                elif opc == 0x02:
                    slba = struct.unpack_from('<Q', sqe, 40)[0]
                    nlb = struct.unpack_from('<H', sqe, 48)[0] + 1
                    s.sendall(c2h(cid, disk[slba * BS:(slba + nlb) * BS]))
                else:
                    s.sendall(resp(cid, status=0x01 << 1))
        except (EOFError, ConnectionError):
            pass
class S(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
srv = S(("127.0.0.1", 0), H)
open(sys.argv[2], "w").write(str(srv.server_address[1]))
srv.serve_forever()
PY
python3 "$W/nvme.py" "$W/disk4k.img" "$W/nvme.port" &
NVME_PID=$!
for _ in $(seq 100); do [[ -s "$W/nvme.port" ]] && break; sleep 0.1; done
NVME_PORT=$(cat "$W/nvme.port" 2>/dev/null) || die "the NVMe/TCP stub did not start"
say "NVMe/TCP stub on 127.0.0.1:$NVME_PORT, $(( $(stat -c %s "$W/disk4k.img") / 1048576 )) MiB at 4096-byte blocks"

# A second target serving a blank 64 MiB namespace: an image with no ESP to
# read, so the attach works and nothing boots (#54).
truncate -s 64M "$W/blank4k.img"
python3 "$W/nvme.py" "$W/blank4k.img" "$W/blank.port" &
BLANK_PID=$!
for _ in $(seq 100); do [[ -s "$W/blank.port" ]] && break; sleep 0.1; done
BLANK_PORT=$(cat "$W/blank.port" 2>/dev/null) || die "the blank NVMe/TCP stub did not start"
say "blank NVMe/TCP stub on 127.0.0.1:$BLANK_PORT"

# The SNTP stub (#77): a fixed time, or with ntp.bad present an
# unsynchronised answer (LI 3) that must not set the clock.
cat > "$W/ntp.py" <<'PY'
import os, socket, struct, sys
W = os.path.dirname(sys.argv[1])
log = open(sys.argv[1], "a", buffering=1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", 0))
open(sys.argv[2], "w").write(str(s.getsockname()[1]))
T = 1935630121 + 2208988800  # 2031-05-04 03:02:01 UTC
while True:
    q, peer = s.recvfrom(512)
    if len(q) < 48:
        continue
    bad = os.path.exists(W + "/ntp.bad")
    log.write("NTP request from %s:%d%s\n" % (peer[0], peer[1], " (answered LI 3)" if bad else ""))
    first = (0xC0 if bad else 0) | (4 << 3) | 4
    r = struct.pack("!BBbb", first, 2, 0, -20) + b"\0" * 8 + b"STUB"
    r += struct.pack("!II", T, 0) + q[40:48] + struct.pack("!IIII", T, 0, T, 0x80000000)
    s.sendto(r, peer)
PY
python3 "$W/ntp.py" "$W/ntp.log" "$W/ntp.port" &
NTP_PID=$!
for _ in $(seq 50); do [[ -s "$W/ntp.port" ]] && break; sleep 0.1; done
NTP_PORT=$(cat "$W/ntp.port" 2>/dev/null) || die "the SNTP stub did not start"
say "SNTP stub on 127.0.0.1:$NTP_PORT (udp)"

accel=tcg
[[ -w /dev/kvm ]] && accel=kvm

# install-config.yaml (#79), written into a built ISO the way storminstall
# does (its docs/config-slot.md): into the FAT volume the isohybrid MBR's 0xEF
# entry names, with mtools, nothing else touched. 2 KiB, so it takes three of
# stormbootx's 768-byte chunk variables.
IC="$W/install-config.yaml"
{
    printf 'apiVersion: v1\nbaseDomain: stub.lo\nmetadata:\n  name: stubcluster\n'
    printf 'pullSecret: '"'"'{"auths":{"registry.stub.lo":{"auth":"%s"}}}'"'"'\n' "$(head -c 900 /dev/zero | tr '\0' 'A')"
    printf 'sshKey: ssh-ed25519 %s stub@test\n' "$(head -c 1000 /dev/zero | tr '\0' 'B')"
} > "$IC"
IC_LEN=$(stat -c %s "$IC")
IC_SHA=$(sha256sum "$IC" | cut -d' ' -f1)
write_install_config() { # ISO [FILE]
    local file=${2:-$IC}
    local off
    off=$(python3 - "$1" <<'PY'
import struct, sys
mbr = open(sys.argv[1], "rb").read(512)
for i in range(4):
    e = mbr[446 + 16 * i: 462 + 16 * i]
    if e[4] == 0xEF:
        print(struct.unpack_from("<I", e, 8)[0] * 512)
        break
PY
)
    [[ -n "$off" ]] || die "no 0xEF partition in $1's MBR"
    mcopy -o -i "$1@@$off" "$file" ::/stormboot/install-config.yaml || die "could not write install-config.yaml into $1"
    say "install-config.yaml ($(stat -c %s "$file") bytes, sha256 $(sha256sum "$file" | cut -d' ' -f1)) written to the ESP at byte $off"
}

# boot NAME CPU RNG CLAIM NTP EXPECTED...   (CLAIM: 404, ok or new; NTP: good or bad)
# BOOT_PORT names another NVMe target for the media, BOOT_DISK adds a local
# virtio-scsi disk, and BOOT_SETTLE is how long the firmware runs on after
# the stop line (the payload, or a fall-through).
BOOT_PORT=
BOOT_DISK=
BOOT_SETTLE=2
# BOOT_NET=dead puts the only NIC on a hub with nothing else on it (#88), and
# STOP_AT ends a boot at a console line of its own.
BOOT_NET=
STOP_AT=
# BOOT_PXE=1 (#68) turns the firmware's own IPv4 stack on (its drivers need
# an EFI_RNG, so a virtio-rng too) and makes the NIC the second boot option,
# PXE-booting the payload over slirp's TFTP; WAIT_FOR is then the only line
# that ends the boot, so a fall-through goes on to the next option.
BOOT_PXE=
WAIT_FOR=
# BOOT_INTENT (#11) is the intent the stub engine answers for this MAC.
BOOT_INTENT=
# BOOT_DRIVERS (#108) is a directory laid on the media as \stormboot\drivers,
# BOOT_PREFER its `prefer_media_drivers`, BOOT_NICDEV more virtio-net-pci
# properties (disable-legacy=on: a modern-only 1af4:1041).
BOOT_DRIVERS=
BOOT_PREFER=
# BOOT_NIC_VERBOSE=1 (#80) writes `nic_verbose = true` on the media.
BOOT_NIC_VERBOSE=
# BOOT_ESP (#42) is the medium's `esp =`: `stormbootx` runs the bridge.
BOOT_ESP=
# BOOT_NOFALLBACK=1 (#36) builds the medium as the goldens are: no nqn/nsid.
BOOT_NOFALLBACK=
# BOOT_IC (#93): an install-config.yaml of another size for this boot's ESP.
BOOT_IC=
BOOT_NICDEV=
boot() {
    local name=$1 cpu=$2 rng=$3 claim=$4 ntp=$5; shift 5
    # NET_ONLY=<name>: run that boot alone (a rerun of one failure).
    [[ -z "${NET_ONLY:-}" || "$NET_ONLY" == "$name" ]] || return 0
    rm -f "$W/claim.ok" "$W/claim.new" "$W/ntp.bad" "$W/intent"
    [[ $claim == ok ]] && : > "$W/claim.ok"
    [[ $claim == new ]] && : > "$W/claim.new"
    [[ -n $BOOT_INTENT ]] && printf '%s\n' "$BOOT_INTENT" > "$W/intent"
    [[ $ntp == bad ]] && : > "$W/ntp.bad"
    local iso="$W/$name.iso" log="$W/$name.serial" txt="$W/$name.txt"
    local args=(--iso --binary "$EFI" --engine 10.0.2.2 --api-port "$PORT" --port "${BOOT_PORT:-$NVME_PORT}"
                --ntp "10.0.2.2:$NTP_PORT" --output "$iso")
    [[ -n "$rng" ]] && args+=(--rng "$rng")
    [[ -n "$BOOT_DRIVERS" ]] && args+=(--drivers "$BOOT_DRIVERS")
    [[ -n "$BOOT_PREFER" ]] && args+=(--prefer-media-drivers "$BOOT_PREFER")
    [[ -n "$BOOT_NIC_VERBOSE" ]] && args+=(--nic-verbose)
    [[ -n "$BOOT_ESP" ]] && args+=(--esp "$BOOT_ESP")
    if [[ -n "$BOOT_NOFALLBACK" ]]; then args+=(--no-fallback); else args+=(--nsid 1); fi
    # The shipped boot's medium names a self-update (#83), which an ISO must
    # skip as read-only; the other names none.
    [[ $name == shipped ]] && args+=(--update "http://10.0.2.2:$PORT/api/v1/boothelpers/stormbootx-test")
    # It also names a media label, which every claim carries (#90).
    [[ $name == shipped ]] && args+=(--media "agent test")
    "$ROOT/scripts/build-boot-agent.sh" "${args[@]}" >/dev/null
    [[ $name == jitter ]] && write_install_config "$iso"
    [[ -n "$BOOT_IC" ]] && write_install_config "$iso" "$BOOT_IC"
    cp "$OVMF_VARS" "$W/$name.vars"
    : > "$W/stub.log"
    : > "$W/ntp.log"
    local extra=() net=(-netdev user,id=n0) nicopt= ipv4=no media=(-cdrom "$iso" -boot d)
    [[ $BOOT_NET == dead ]] && net=(-netdev hubport,id=n0,hubid=7)
    if [[ -n $BOOT_PXE ]]; then
        mkdir -p "$W/tftp" && cp "$PAYLOAD" "$W/tftp/payload.efi"
        net=(-netdev user,id=n0,tftp="$W/tftp",bootfile=payload.efi)
        nicopt=,bootindex=2 ipv4=yes
        media=(-drive if=none,id=cd0,media=cdrom,format=raw,file="$iso" -device ide-cd,drive=cd0,bootindex=1)
        extra+=(-device virtio-rng-pci)
    fi
    [[ -n "$BOOT_DISK" ]] && extra+=(-device virtio-scsi-pci,id=scsi0
        -drive if=none,id=d0,format=raw,file="$BOOT_DISK" -device scsi-hd,drive=d0,bus=scsi0.0)
    say "[$name] booting under OVMF ($accel, cpu $cpu, rng ${rng:-as shipped}, up to ${LIMIT}s)"
    qemu-system-x86_64 -machine q35,accel="$accel" -cpu "$cpu" -m 1024 \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$W/$name.vars" \
        -fw_cfg name=opt/org.tianocore/IPv4Support,string=$ipv4 \
        -fw_cfg name=opt/org.tianocore/IPv6Support,string=no \
        "${net[@]}" -device virtio-net-pci,netdev=n0,romfile=$nicopt${BOOT_NICDEV:+,$BOOT_NICDEV} \
        "${media[@]}" "${extra[@]}" \
        -debugcon file:"$W/$name.debug" -global isa-debugcon.iobase=0x402 \
        -display none -serial file:"$log" -no-reboot &
    local qemu=$! t=0
    # Stop once the payload has spoken, or at a fall-through.
    while kill -0 "$qemu" 2>/dev/null && (( t < LIMIT )); do
        grep -qE "${WAIT_FOR:-is there a TCP/IP stack in this firmware|no network boot: ${STOP_AT:+|$STOP_AT}}" "$log" 2>/dev/null \
            && { sleep "$BOOT_SETTLE"; break; }
        sleep 1; t=$((t + 1))
    done
    local how="stopped at its stop line"
    kill -0 "$qemu" 2>/dev/null || how="qemu exited by itself"
    (( t < LIMIT )) || how="ran the whole ${LIMIT}s"
    kill "$qemu" 2>/dev/null; wait "$qemu" 2>/dev/null || true
    say "[$name] $how after ${t}s"
    # The firmware's debug port is read too: an exception dump may go there.
    cat "$log" "$W/$name.debug" 2>/dev/null | tr -d '\r' | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$txt"
    say "[$name] console:"
    grep -v '^\s*$' "$txt" | sed -n '1,90s/^/  | /p'
    say "[$name] stub log:"
    sed 's/^/  > /' "$W/stub.log" "$W/ntp.log"
    local fail=0 want
    for want in "$@"; do
        if [[ "$want" == stub:* ]]; then
            grep -qF -- "${want#stub:}" "$W/stub.log" "$W/ntp.log" && say "[$name] stub saw: ${want#stub:}" \
                || { say "[$name] stub missing: ${want#stub:}"; fail=1; }
        elif [[ "$want" == not-stub:* ]]; then
            grep -qF -- "${want#not-stub:}" "$W/stub.log" && { say "[$name] stub unexpectedly saw: ${want#not-stub:}"; fail=1; } \
                || say "[$name] stub never saw, as it should be: ${want#not-stub:}"
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
        # The head above is mostly the firmware's debug port; the boot's own
        # console is the serial log's end.
        say "[$name] serial console, last 60 lines:"
        tr -d '\r' < "$log" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' | grep -v '^\s*$' | tail -60 | sed 's/^/  | /'
        die "[$name] expected lines missing"
    fi
}

if [[ -n "${UPDATE_KEY:-}" ]]; then
    # shellcheck source=tests/update-boots.sh
    source "$ROOT/tests/update-boots.sh"
    say "PASS"
    exit 0
fi

common=(
    "nic 0: driver Virtio Network Driver"
    "tcp4        : smoltcp over SNP (nic 0 "
    "nic 0: leased 10.0.2.15/24 gw 10.0.2.2"
    "engine      : stormblock 19.4.0  (universal boot)"
    "namespace : "
    "x 4096 bytes"
    "blockio     : published on handle"
    "blockio     : 96 MiB read in all"
    "is there a TCP/IP stack in this firmware"
    "not:no network boot"
    "stub:GET /api/v1/health"
    "stub:POST /api/v1/synonyms/boothost/"
    "stub:{\"agent\":{\"name\":\"stormbootx\",\"version\":\"$VERSION\""
    "not:EFI_TCP4 is not present"
    "stub:NTP request from "
)
host_cpu=max
[[ $accel == kvm ]] && host_cpu=host
boot shipped "$host_cpu" "" 404 good "${common[@]}" "rng         : " \
    "handoff     : StormBootHostNqn = nqn.2026-09.lo.storm:host-" \
    "handed down : StormBootHostNqn = nqn.2026-09.lo.storm:host-" \
    "clock       : was " \
    ", set to 2031-05-04 03:02:0" \
    " UTC from 10.0.2.2:$NTP_PORT (step +" \
    "handoff     : StormBootClock = synced:10.0.2.2" \
    "handed down : StormBootClock = synced:10.0.2.2  (attributes 0x6)" \
    "rtc         : 2031-05-04 03:0" \
    "update      : the boot medium is read-only (an ISO or virtual media); not updated" \
    "stub:\"media\":\"agent test\"}" \
    "install cfg : none on the media (\\stormboot\\install-config.yaml)" \
    "not:handed down : StormBootInstallConfig"
grep -q "rng         : jitter" "$W/shipped.txt" && say "note: the shipped boot fell back to jitter"
BOOT_INTENT=install \
boot jitter "$host_cpu,-rdrand,-rdseed" cpu ok bad "${common[@]}" "rng         : jitter" \
    "intent      : install" \
    "clock       : NTP unreachable at 10.0.2.2:$NTP_PORT (the server says it is not synchronised (LI 3)); left at " \
    "handed down : StormBootClock = unsynced  (attributes 0x6)" \
    "not:rtc         : 2031" \
    "booting stubhost's image" \
    "handoff     : StormBootTag = stubhost" \
    "handed down : StormBootTag = stubhost  (attributes 0x6)" \
    "handed down : StormBootHostNqn = nqn.2026-09.lo.storm:host-stubhost  (attributes 0x6)" \
    "update      : not configured (no update = in \\stormboot\\stormboot.conf)" \
    "handoff     : StormBootInstallConfig ($IC_LEN bytes in 3 chunk(s)) = v1:$IC_LEN:3:$IC_SHA" \
    "handed down : StormBootInstallConfig = v1:$IC_LEN:3:$IC_SHA  (attributes 0x6)" \
    "install cfg : $IC_LEN bytes reassembled from 3 chunk(s), length and sha256 match" \
    "not:stub@test"

# #15: a machine nothing has named and the engine has never seen. It must
# claim `boothost/default` by its MAC (the engine is newer than 19.3.0),
# boot the provisional host the engine minted, and hand that name down.
boot newhost "$host_cpu" "" new good \
    "engine      : stormblock 19.4.0  (universal boot)" \
    "name        : none from DHCP or reverse DNS" \
    "claim       : boothost/default as 52:54:00:12:34:56 at " \
    "  booting the default image as mac-525400123456" \
    "handed down : StormBootTag = mac-525400123456  (attributes 0x6)" \
    "handed down : StormBootHostNqn = nqn.2026-09.lo.storm:host-mac-525400123456  (attributes 0x6)" \
    "blockio     : 96 MiB read in all" \
    "is there a TCP/IP stack in this firmware" \
    "stub:POST /api/v1/synonyms/boothost/default/claim {" \
    "stub:\"mac\":\"52:54:00:12:34:56\"" \
    "not:no network boot"

# #54: an attach that boots nothing, on a machine with one blank local disk.
# The fall-through must take the attached disk back before returning, or
# the firmware, probing disks for the next boot option, calls into the
# unloaded image (pvetest1: #UD at RIP 0x47FFFFFCA). The firmware gets 25 s
# after the fall-through to try every boot option.
truncate -s 64M "$W/local.img"
BOOT_PORT=$BLANK_PORT BOOT_DISK="$W/local.img" BOOT_SETTLE=25 \
boot noesp "$host_cpu" "" 404 good \
    "blockio     : published on handle" \
    "no network boot: " \
    "blockio     : withdrawn" \
    "RESULT: falling through to the local disk (1 found)" \
    "not:handle(s) an earlier start published are still installed" \
    "not:X64 Exception" \
    "not:!!!!"

# #42: the bridge on the attached image. With `esp = stormbootx` the
# firmware's FAT is not asked: stormbootx reads the ESP over NVMe/TCP, puts
# its own read-only filesystem on the ESP's partition handle in place of
# OVMF's FAT, and starts the 96 MiB BOOTX64.EFI under it. The payload must
# find that filesystem as its own volume, read-only, and be refused a write.
BOOT_ESP=stormbootx \
boot bridge "$host_cpu" "" ok good \
    "boot        : the firmware did not load it (esp = stormbootx); reading the ESP here" \
    "esp fs      : read-only filesystem on the ESP's partition handle, the firmware's FAT disconnected from it" \
    "boot        : starting \\EFI\\BOOT\\BOOTX64.EFI from the attached image (read by stormbootx)" \
    "boot fs     : " \
    ", read-only, label " \
    'boot fs     : \EFI\BOOT lists . | .. | BOOTX64.EFI' \
    "boot fs     : BOOTX64.EFI is 100663296 bytes; not hashed here" \
    "boot fs     : create refused (WRITE_PROTECTED)" \
    "is there a TCP/IP stack in this firmware"

# #88: a NIC nothing answers. The boot must say so once a second (link, DHCP
# out and in, frames in) instead of sitting silent until the engine's 30 s
# connect gives up, and must name the NIC's driver before its first SNP call.
BOOT_NET=dead STOP_AT="engine      : version unknown" BOOT_SETTLE=1 \
boot nolease "$host_cpu" "" 404 good \
    "nic 0: driver Virtio Network Driver" \
    "waiting for a lease (1 s): nic 0 link UP, DHCP " \
    "waiting for a lease (10 s): nic 0 link UP, DHCP " \
    " 0 in, 0 frames in" \
    "engine      : version unknown (no address after 30 s: nothing answered DHCP on any of 1 NIC(s))" \
    "not:nic 0: leased" \
    "not:has not returned after"

# #11: an engine that says `local` for this machine. The intent is read down
# the claim's names (the DNS name, if any, 404s; the MAC is the host's
# alias), and `local` must fall through with no claim made, so no clone is
# minted.
BOOT_INTENT=local \
boot intent "$host_cpu" "" ok good \
    "engine      : stormblock 19.4.0  (universal boot)" \
    "intent      : local" \
    "no network boot: boot intent for 525400123456 is \`local\`: nothing claimed" \
    "stub:GET /api/v1/synonyms/boothost/525400123456/intent" \
    "not-stub:/claim" \
    "not:blockio     : published on handle" \
    "not:claim       : boothost/"

# #108: stormnic-virtio on the media, with `prefer_media_drivers = virtio`.
# OVMF's virtio drivers bind the NIC in stormbootx's first pass; the media's
# driver must take it from them and carry the whole boot (lease, claim, the
# 96 MiB attach), on a transitional NIC (1af4:1000) and a modern-only one
# (1af4:1041). Without the key, the firmware keeps it. VIRTIO_EFI names the
# driver (scripts/build-nic-drivers.sh); without it these boots are skipped.
if [[ -n "${VIRTIO_EFI:-}" ]]; then
    [[ -s "$VIRTIO_EFI" ]] || die "no stormnic-virtio driver at $VIRTIO_EFI"
    mkdir -p "$W/vdrv" && cp "$VIRTIO_EFI" "$W/vdrv/stormnic-virtio.efi"
    virtio=(
        "starting stormnic-virtio.efi"
        "stormnic-virtio.efi started"
        ": taken from "
        "; stormnic-virtio.efi drives it"
        "stormnic-virtio 0.1.0: "
        ", SNP installed"
        "not:nic 0: driver Virtio Network Driver"
        "nic 0: leased 10.0.2.15/24 gw 10.0.2.2"
        "not:given back to"
        "not:would not let go"
        "not:stormnic drivers verbose"
    )
    BOOT_DRIVERS="$W/vdrv" BOOT_PREFER=virtio \
    boot virtio "$host_cpu" "" ok good "${virtio[@]}" " 1af4:1000: " \
        "booting stubhost's image" \
        "blockio     : 96 MiB read in all" \
        "is there a TCP/IP stack in this firmware"
    BOOT_DRIVERS="$W/vdrv" BOOT_PREFER=virtio BOOT_NICDEV=disable-legacy=on,disable-modern=off \
    STOP_AT="engine      : stormblock 19.4.0" BOOT_SETTLE=1 \
    boot virtio-modern "$host_cpu" "" 404 good "${virtio[@]}" " 1af4:1041: "
    # The quiet trace stays out of the boot above; nic_verbose brings it back
    # (#80): StormnicVerbose is set before the driver loads.
    # The driver's trace lines start `stormnic-virtio: `; its one quiet line is
    # `stormnic-virtio 0.1.0: …` (and OVMF's debug port has `…: Supported` of
    # its own).
    grep -qE "stormnic-virtio: .*: Supported" "$W/virtio.txt" && die "[virtio] a quiet boot printed the driver's trace"
    BOOT_DRIVERS="$W/vdrv" BOOT_PREFER=virtio BOOT_NIC_VERBOSE=1 \
    STOP_AT="engine      : stormblock 19.4.0" BOOT_SETTLE=1 \
    boot virtio-verbose "$host_cpu" "" 404 good \
        "drivers     : stormnic drivers verbose (nic_verbose in " \
        "StormnicVerbose set until reset)" \
        "; stormnic-virtio.efi drives it" \
        "stormnic-virtio 0.1.0: "
    grep -qE "stormnic-virtio: .*: Supported" "$W/virtio-verbose.txt" \
        && say "[virtio-verbose] found the driver's trace: $(grep -E 'stormnic-virtio: .*: Supported' "$W/virtio-verbose.txt" | head -1)" \
        || die "[virtio-verbose] nic_verbose set, but the driver printed no trace"
    BOOT_DRIVERS="$W/vdrv" STOP_AT="engine      : stormblock 19.4.0" BOOT_SETTLE=1 \
    boot virtio-kept "$host_cpu" "" 404 good \
        "stormnic-virtio.efi started" \
        "nic 0: driver Virtio Network Driver" \
        "not:taken from" \
        "not:SNP installed"
else
    say "note: VIRTIO_EFI not set; the stormnic-virtio boots (#108) are skipped"
fi

# #93: how large an install-config.yaml OVMF's volatile variable store takes.
# Each size is handed down in 768-byte chunks and the payload reassembles it.
# Measured 2026-10-08: OVMF takes every size up to storminstall's 256 KiB cap
# (342 chunks), so each must reassemble here. A store that is full on other
# firmware says how much fit and hands down nothing.
for kb in 64 128 192 256; do
    f="$W/ic-$kb.yaml"
    # Written whole, then cut to size: a `| head -c` would close the pipe on
    # its writer, which pipefail turns into a failed test (#111).
    head -c $((kb * 1024)) /dev/urandom > "$f.raw"
    { printf 'apiVersion: v1\nmetadata:\n  name: size-%s\nfiller: |\n' "$kb"
      base64 -w 76 < "$f.raw" | sed 's/^/  /'
    } > "$f"
    truncate -s $((kb * 1024)) "$f"
    len=$(stat -c %s "$f"); sha=$(sha256sum "$f" | cut -d' ' -f1)
    n=$(( (len + 767) / 768 ))
    BOOT_IC="$f" boot "ic$kb" "$host_cpu" "" ok good "install cfg : " "is there a TCP/IP stack in this firmware"
    [[ -z "${NET_ONLY:-}" || "$NET_ONLY" == "ic$kb" ]] || continue
    if grep -qF "install cfg : $len bytes reassembled from $n chunk(s), length and sha256 match" "$W/ic$kb.txt"; then
        say "[ic$kb] SIZE RESULT: $kb KiB ($len bytes, $n chunks) handed down and reassembled"
    elif grep -qF "install cfg : NOT HANDED DOWN: the firmware's volatile variable store is full" "$W/ic$kb.txt"; then
        die "[ic$kb] OVMF took 256 KiB on 2026-10-08 and now refuses $kb KiB: $(grep -F 'NOT HANDED DOWN' "$W/ic$kb.txt" | head -1)"
    else
        die "[ic$kb] neither reassembled nor refused: $(grep -F 'install cfg' "$W/ic$kb.txt" | tr '\n' ' ')"
    fi
done

# #68: a fall-through gives the NICs back. The firmware's own IPv4 stack is
# on, so stormbootx's exclusive SNP open first takes the NIC from the
# firmware's MNP; the claim 404s and the media names no fallback, so it
# falls through, and the next boot option is the firmware's PXE on that
# NIC, which must lease and TFTP the payload, which then starts.
# The medium is built as the goldens are (#36): no fallback namespace, so
# the 404 alone falls through, with nothing attached.
BOOT_NOFALLBACK=1 BOOT_PXE=1 WAIT_FOR="is there a TCP/IP stack in this firmware" BOOT_SETTLE=3 \
boot release "$host_cpu" "" 404 good \
    "tcp4        : smoltcp over SNP (nic " \
    "fallback    : none (a claim that finds nothing falls through to the local disk)" \
    "no network boot: no claim gave this machine an image, and the media names no fallback (fallback = none)" \
    "not:attaching   : " \
    "not:blockio     : published on handle" \
    "net         : 1 of 1 NIC(s) given back to the firmware (exclusive SNP closed, reconnected)" \
    "is there a TCP/IP stack in this firmware" \
    "not:SNP not closed"
grep -qE "Start PXE over IPv4|Station IP address is" "$W/release.txt" \
    && say "[release] the firmware's PXE ran on the NIC after the fall-through" \
    || say "note: no PXE banner on the console (the payload's banner is the evidence)"
say "PASS"
