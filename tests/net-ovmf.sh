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
#     spans many segments and windows; claims get 404, so the boot falls back
#     to the target on the media;
#   - an NVMe/TCP target (the subset nvme.rs speaks) serving a GPT disk at
#     4096-byte blocks whose ESP carries tcp4probe.efi, padded to 96 MiB, as
#     BOOTX64.EFI. The firmware's FAT reads all of it through stormbootx's
#     BlockIO, so the boot moves 96 MiB over smoltcp (and prints blockio's
#     MiB/s line), then starts the payload.
#
# Two boots:
#   1. as shipped: `rng : firmware` or `rdrand`;
#   2. the entropy fallback: `rng = cpu` on the media masks the firmware's
#      EFI_RNG, and the CPU is started without RDRAND and RDSEED, so the
#      boot must say `rng : jitter` and still lease, connect and fetch.
#
# Each boot must show the stack, a lease from slirp, the engine's version from
# the stub, the attach, a blockio progress line and the payload's banner; the
# engine stub must log the health GET and a claim POST.
#
# PAYLOAD is any EFI application that prints; the sc-build command uses
# tcp4probe.efi and looks for its banner.
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
cleanup() {
    [[ -n "$STUB_PID" ]] && kill "$STUB_PID" 2>/dev/null
    [[ -n "$NVME_PID" ]] && kill "$NVME_PID" 2>/dev/null
    rm -rf "$W"
}
trap cleanup EXIT

# The stub engine. Port 0: the kernel picks one, and the stub writes it down.
cat > "$W/stub.py" <<'PY'
import http.server, json, sys
log = open(sys.argv[1], "a", buffering=1)
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
        if self.path == "/api/v1/health":
            self.reply(200, {"version": "19.4.0", "status": "ok", "pad": "x" * 120000})
        else:
            self.reply(404, {"error": "stub: not found"})
    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n).decode(errors="replace")
        log.write("POST %s %s\n" % (self.path, body))
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

accel=tcg
[[ -w /dev/kvm ]] && accel=kvm

# boot NAME CPU RNG EXPECTED...
boot() {
    local name=$1 cpu=$2 rng=$3; shift 3
    local iso="$W/$name.iso" log="$W/$name.serial" txt="$W/$name.txt"
    local args=(--iso --binary "$EFI" --engine 10.0.2.2 --api-port "$PORT" --port "$NVME_PORT"
                --nsid 1 --output "$iso")
    [[ -n "$rng" ]] && args+=(--rng "$rng")
    "$ROOT/scripts/build-boot-agent.sh" "${args[@]}" >/dev/null
    cp "$OVMF_VARS" "$W/$name.vars"
    : > "$W/stub.log"
    say "[$name] booting under OVMF ($accel, cpu $cpu, rng ${rng:-as shipped}, up to ${LIMIT}s)"
    qemu-system-x86_64 -machine q35,accel="$accel" -cpu "$cpu" -m 1024 \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$W/$name.vars" \
        -fw_cfg name=opt/org.tianocore/IPv4Support,string=no \
        -fw_cfg name=opt/org.tianocore/IPv6Support,string=no \
        -netdev user,id=n0 -device virtio-net-pci,netdev=n0,romfile= \
        -cdrom "$iso" -boot d \
        -display none -serial file:"$log" -no-reboot &
    local qemu=$! t=0
    # Stop once the payload has spoken, or at a fall-through.
    while kill -0 "$qemu" 2>/dev/null && (( t < LIMIT )); do
        grep -qE "is there a TCP/IP stack in this firmware|no network boot: " "$log" 2>/dev/null \
            && { sleep 2; break; }
        sleep 1; t=$((t + 1))
    done
    kill "$qemu" 2>/dev/null; wait "$qemu" 2>/dev/null || true
    tr -d '\r' < "$log" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$txt"
    say "[$name] console:"
    grep -v '^\s*$' "$txt" | sed -n '1,90s/^/  | /p'
    say "[$name] stub log:"
    sed 's/^/  > /' "$W/stub.log"
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
    [[ $fail -eq 0 ]] || die "[$name] expected lines missing"
}

common=(
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
    "not:EFI_TCP4 is not present"
)
host_cpu=max
[[ $accel == kvm ]] && host_cpu=host
boot shipped "$host_cpu" "" "${common[@]}" "rng         : "
grep -q "rng         : jitter" "$W/shipped.txt" && say "note: the shipped boot fell back to jitter"
boot jitter "$host_cpu,-rdrand,-rdseed" cpu "${common[@]}" "rng         : jitter"
say "PASS"
