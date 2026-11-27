#!/usr/bin/env bash
# tests/ping-ovmf.sh STORMBOOTX.EFI (#217): a peer on stormbootx's wire can
# ping it once it holds a lease (smoltcp's `auto-icmp-echo-reply`).
#
# slirp never pings its guest, so the NIC here is QEMU's UDP socket netdev,
# one Ethernet frame per datagram, and the peer at the other end is the
# Python below:
# - it is 10.9.0.1 and answers ARP for it;
# - it leases the guest 10.9.0.10 (DHCP offer and ack, router and server
#   10.9.0.1);
# - it answers every TCP SYN with a RST, so the claim fails at once;
# - then it pings 10.9.0.10 while stormbootx holds (`hold = forever`, nothing
#   local boots).
# Three replies to five echoes pass; none fails, which was #217.
set -euo pipefail

EFI=${1:?usage: $0 STORMBOOTX.EFI}
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LIMIT=${LIMIT:-240}
ROOT=$(cd "$(dirname "$0")/.." && pwd)

say() { printf 'ping-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

for cmd in qemu-system-x86_64 python3; do
    command -v "$cmd" >/dev/null || die "$cmd is not installed"
done
[[ -r "$OVMF_CODE" ]] || die "no OVMF at $OVMF_CODE"
[[ -s "$EFI" ]] || die "no stormbootx at $EFI"

W=$(mktemp -d "${TMPDIR:-/tmp}/ping-ovmf.XXXXXX")
trap '{ [[ -z "${qemu:-}" ]] || kill "$qemu" 2>/dev/null || true; [[ -z "${peer:-}" ]] || kill "$peer" 2>/dev/null || true; }; rm -rf "$W"' EXIT

"$ROOT/scripts/build-boot-agent.sh" --iso --binary "$EFI" --engine 10.9.0.1 --api-port 9 \
    --ntp 10.9.0.1:123 --syslog off --hold forever --output "$W/ping.iso" >/dev/null \
    || die "could not build the medium"

cat > "$W/peer.py" <<'PY'
import socket, struct, sys, time

GUEST_PORT, PEER_PORT, LOG = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
ME_MAC = bytes.fromhex("525400009901")
ME_IP, GUEST_IP = socket.inet_aton("10.9.0.1"), socket.inet_aton("10.9.0.10")
BCAST = b"\xff" * 6
log = open(LOG, "a", buffering=1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", PEER_PORT))
s.settimeout(0.2)

def csum(b):
    if len(b) % 2:
        b += b"\0"
    t = sum(struct.unpack("!%dH" % (len(b) // 2), b))
    while t >> 16:
        t = (t & 0xffff) + (t >> 16)
    return (~t) & 0xffff

def send(dst_mac, ethertype, payload):
    s.sendto(dst_mac + ME_MAC + struct.pack("!H", ethertype) + payload, ("127.0.0.1", GUEST_PORT))

def ip(dst, proto, body, src=ME_IP):
    h = struct.pack("!BBHHHBBH4s4s", 0x45, 0, 20 + len(body), 0, 0, 64, proto, 0, src, dst)
    h = h[:10] + struct.pack("!H", csum(h)) + h[12:]
    return h + body

def dhcp_reply(req, kind, guest_mac):
    xid = req[4:8]
    bootp = struct.pack("!BBBB4sHH4s4s4s4s16s64s128s", 2, 1, 6, 0, xid, 0, 0,
                        b"\0" * 4, GUEST_IP, ME_IP, b"\0" * 4, guest_mac + b"\0" * 10, b"", b"")
    opts = b"\x63\x82\x53\x63" + bytes([53, 1, kind]) + bytes([54, 4]) + ME_IP
    opts += bytes([51, 4]) + struct.pack("!I", 3600) + bytes([1, 4]) + socket.inet_aton("255.255.255.0")
    opts += bytes([3, 4]) + ME_IP + b"\xff"
    udp_body = bootp + opts
    udp = struct.pack("!HHHH", 67, 68, 8 + len(udp_body), 0) + udp_body
    send(guest_mac, 0x0800, ip(b"\xff" * 4, 17, udp))

guest_mac, leased, sent, replies, seq = None, None, 0, 0, 0
next_ping = None
deadline = time.time() + 600
while time.time() < deadline:
    try:
        f, _ = s.recvfrom(2048)
    except socket.timeout:
        f = None
    if f and len(f) >= 14:
        dst, src, et = f[:6], f[6:12], struct.unpack("!H", f[12:14])[0]
        p = f[14:]
        if et == 0x0806 and len(p) >= 28:
            op, tpa = struct.unpack("!H", p[6:8])[0], p[24:28]
            if op == 1 and tpa == ME_IP:
                arp = struct.pack("!HHBBH6s4s6s4s", 1, 0x0800, 6, 4, 2, ME_MAC, ME_IP, p[8:14], p[14:18])
                send(src, 0x0806, arp)
        elif et == 0x0800 and len(p) >= 20:
            ihl = (p[0] & 15) * 4
            proto, body = p[9], p[ihl:]
            if proto == 17 and len(body) >= 8 and struct.unpack("!H", body[2:4])[0] == 67:
                req = body[8:]
                opts = req[240:]
                kind, i = None, 0
                while i < len(opts) and opts[i] != 255:
                    if opts[i] == 0:
                        i += 1; continue
                    if opts[i] == 53:
                        kind = opts[i + 2]
                    i += 2 + opts[i + 1]
                guest_mac = req[28:34]
                if kind == 1:
                    log.write("dhcp: discover from %s; offering 10.9.0.10\n" % guest_mac.hex(":"))
                    dhcp_reply(req, 2, guest_mac)
                elif kind == 3:
                    log.write("dhcp: request; ack 10.9.0.10\n")
                    dhcp_reply(req, 5, guest_mac)
                    if leased is None:
                        leased = time.time()
                        next_ping = leased + 3
            elif proto == 6 and len(body) >= 20:
                sport, dport, sseq = struct.unpack("!HHI", body[:8])
                flags = body[13]
                if flags & 0x02:  # SYN: refuse it
                    tcp = struct.pack("!HHIIBBHHH", dport, sport, 0, (sseq + 1) & 0xffffffff, 5 << 4, 0x14, 0, 0, 0)
                    pseudo = ME_IP + p[12:16] + struct.pack("!BBH", 0, 6, len(tcp))
                    tcp = tcp[:16] + struct.pack("!H", csum(pseudo + tcp)) + tcp[18:]
                    send(src, 0x0800, ip(p[12:16], 6, tcp))
            elif proto == 1 and len(body) >= 8 and body[0] == 0 and p[12:16] == GUEST_IP:
                replies += 1
                log.write("ping: reply %d from 10.9.0.10 (seq %d)\n" % (replies, struct.unpack("!H", body[6:8])[0]))
    if next_ping and time.time() >= next_ping and sent < 5 and guest_mac:
        seq += 1; sent += 1
        icmp = struct.pack("!BBHHH", 8, 0, 0, 0x217, seq) + b"stormbootx#217 ping"
        icmp = icmp[:2] + struct.pack("!H", csum(icmp)) + icmp[4:]
        send(guest_mac, 0x0800, ip(GUEST_IP, 1, icmp))
        log.write("ping: echo %d sent to 10.9.0.10\n" % seq)
        next_ping = time.time() + 1
    if sent == 5 and time.time() > next_ping + 2:
        log.write("ping: %d/5 replied\n" % replies)
        break
PY

ports=$(python3 -c 'import socket
a=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); a.bind(("127.0.0.1",0))
b=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); b.bind(("127.0.0.1",0))
print(a.getsockname()[1], b.getsockname()[1])')
read -r GUEST_PORT PEER_PORT <<<"$ports"
: > "$W/peer.log"
python3 "$W/peer.py" "$GUEST_PORT" "$PEER_PORT" "$W/peer.log" &
peer=$!

accel=tcg
[[ -w /dev/kvm ]] && accel=kvm
cp "$OVMF_VARS" "$W/vars.fd"
say "booting stormbootx on a UDP-socket NIC with a peer at 10.9.0.1 ($accel, up to ${LIMIT}s)"
qemu-system-x86_64 -machine q35,accel="$accel" -m 512 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file="$W/vars.fd" \
    -device virtio-scsi-pci,id=cdscsi -device scsi-cd,drive=cd0,bus=cdscsi.0,bootindex=0 \
    -drive if=none,id=cd0,media=cdrom,format=raw,readonly=on,file="$W/ping.iso" \
    -netdev dgram,id=n0,local.type=inet,local.host=127.0.0.1,local.port="$GUEST_PORT",remote.type=inet,remote.host=127.0.0.1,remote.port="$PEER_PORT" \
    -device virtio-net-pci,netdev=n0,mac=52:54:00:12:34:56,romfile= \
    -display none -serial file:"$W/serial.log" -no-reboot &
qemu=$!

t=0
until grep -q "^ping: [0-9]/5 replied" "$W/peer.log" 2>/dev/null; do
    kill -0 "$qemu" 2>/dev/null || break
    kill -0 "$peer" 2>/dev/null || break
    (( t < LIMIT )) || break
    sleep 1; t=$((t + 1))
done
tr -d '\r' < "$W/serial.log" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g' > "$W/console.txt"
say "peer:"
sed 's/^/  > /' "$W/peer.log"
grep -E "^(nic|dhcp|lease|hold|api) " "$W/console.txt" | head -12 | sed 's/^/  | /' || true
got=$(sed -n 's/^ping: \([0-9]\)\/5 replied$/\1/p' "$W/peer.log")
if [[ -z "$got" ]]; then
    say "console (last 40 lines):"
    grep -v '^\s*$' "$W/console.txt" | tail -40 | sed 's/^/  | /'
    die "the peer never finished its pings (no lease, or no hold)"
fi
(( got >= 3 )) || die "only $got of 5 echoes answered: stormbootx does not answer ICMP echo (#217)"
say "found: $got of 5 echoes answered while stormbootx held its lease"
say "PASS"
