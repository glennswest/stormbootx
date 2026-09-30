#!/usr/bin/env bash
# stormbootx's own TCP/IP under OVMF, with no firmware network stack (#56).
#
#   tests/net-ovmf.sh STORMBOOTX.EFI
#
# Fedora's OVMF has SNP (virtio-net) and no MNP/IP4/TCP4, and the fw_cfg
# switches below turn the IPv4/IPv6 stacks off on an OVMF that has them, so
# nothing but stormbootx's smoltcp can reach the network. A stub engine runs on
# the build box (slirp maps the guest's 10.0.2.2 to the host's loopback); it
# answers /api/v1/health with a ~120 KB body, so the reply spans many
# segments and windows, and 404 for everything else.
#
# Two boots:
#   1. as shipped: `rng : firmware` or `rdrand`;
#   2. the entropy fallback: `rng = cpu` on the media masks the firmware's
#      EFI_RNG, and the CPU is started without RDRAND and RDSEED, so the
#      boot must say `rng : jitter` and still lease, connect and fetch.
#
# Each boot must show the stack, a lease from slirp, the engine's version from
# the stub, and a refused attach (nothing listens on 4420); the stub must log
# the health GET and a claim POST.
#
# Needs qemu-system-x86_64, OVMF, python3, mtools, mkfs.fat, xorriso. KVM when
# /dev/kvm is writable, TCG otherwise. Unprivileged; everything is under $TMPDIR
# and deleted on exit.
set -euo pipefail

EFI=${1:?usage: $0 STORMBOOTX.EFI}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OVMF_CODE=${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}
LIMIT=${LIMIT:-120}

say() { printf 'net-ovmf: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }

command -v qemu-system-x86_64 >/dev/null || die "qemu-system-x86_64 is not installed"
command -v python3 >/dev/null || die "python3 is not installed"
[[ -r "$OVMF_CODE" ]] || die "no OVMF at $OVMF_CODE"
[[ -s "$EFI" ]] || die "no binary at $EFI"

W=$(mktemp -d "${TMPDIR:-/tmp}/net-ovmf.XXXXXX")
STUB_PID=""
cleanup() { [[ -n "$STUB_PID" ]] && kill "$STUB_PID" 2>/dev/null; rm -rf "$W"; }
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

accel=tcg
[[ -w /dev/kvm ]] && accel=kvm

# boot NAME CPU RNG EXPECTED...
boot() {
    local name=$1 cpu=$2 rng=$3; shift 3
    local iso="$W/$name.iso" log="$W/$name.serial" txt="$W/$name.txt"
    local args=(--iso --binary "$EFI" --engine 10.0.2.2 --api-port "$PORT" --output "$iso")
    [[ -n "$rng" ]] && args+=(--rng "$rng")
    "$ROOT/scripts/build-boot-agent.sh" "${args[@]}" >/dev/null
    cp "$OVMF_VARS" "$W/$name.vars"
    : > "$W/stub.log"
    say "[$name] booting under OVMF ($accel, cpu $cpu, rng ${rng:-as shipped}, up to ${LIMIT}s)"
    qemu-system-x86_64 -machine q35,accel="$accel" -cpu "$cpu" -m 512 \
        -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
        -drive if=pflash,format=raw,file="$W/$name.vars" \
        -fw_cfg name=opt/org.tianocore/IPv4Support,string=no \
        -fw_cfg name=opt/org.tianocore/IPv6Support,string=no \
        -netdev user,id=n0 -device virtio-net-pci,netdev=n0,romfile= \
        -cdrom "$iso" -boot d \
        -display none -serial file:"$log" -no-reboot &
    local qemu=$! t=0
    # Stop at the fall-through, which every boot of this test reaches: the
    # stub has no host to give, and nothing answers NVMe/TCP.
    while kill -0 "$qemu" 2>/dev/null && (( t < LIMIT )); do
        grep -q "RESULT: " "$log" 2>/dev/null && { sleep 1; break; }
        sleep 1; t=$((t + 1))
    done
    kill "$qemu" 2>/dev/null; wait "$qemu" 2>/dev/null || true
    tr -d '\r' < "$log" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$txt"
    say "[$name] console:"
    grep -v '^\s*$' "$txt" | sed -n '1,80s/^/  | /p'
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
    "10.0.2.2:4420 refused the connection"
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
