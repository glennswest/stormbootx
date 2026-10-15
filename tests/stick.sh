#!/usr/bin/env bash
# stormbootx-stick (#151): a running node updates its boot stick. Builds the
# Linux binary (static musl) with an Ed25519 test key compiled in, serves
# signed releases from a local HTTP stub, and runs it against a fake stick (a
# directory standing in for the mounted ESP, --esp) and a fake machine
# (--sys/--proc/--dev trees with image files for disks):
#
#   update    v0.20.0 stick, serial 5: written as .new, read back, swapped
#             with the bootloader last (.prev kept), the trial in `state`,
#             `name =` carried into local.conf beside the stick's own line
#   trial     run again: serial 5 is on trial, nothing new
#   failed    a stick on which serial 6 failed its trial: declined
#   off       `update = off`: left alone
#   older     a stick running a newer version than offered: declined, its
#             serial recorded as the floor (`min`)
#   tampered  a manifest changed after signing: refused before any write
#   dryrun    reports what it would write, writes nothing
#   find      the machine: a fixed disk, a removable disk holding a slab, a
#             mounted USB disk and a held one are refused before anything is
#             opened for writing; the clean removable one is the only one it
#             tries to mount (which an unprivileged test cannot)
#
#   tests/stick.sh
#
# Needs cargo with x86_64-unknown-linux-musl (added with rustup if missing),
# openssl, python3. Unprivileged; everything is under $TMPDIR.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
say() { printf 'stick: %s\n' "$*"; }
die() { say "FAIL: $*"; exit 1; }
for c in cargo openssl python3; do command -v "$c" >/dev/null || die "$c is not installed"; done

T=$(mktemp -d "${TMPDIR:-/tmp}/stick.XXXXXX")
trap 'kill "${srv:-0}" 2>/dev/null || true; rm -rf "$T"' EXIT
TARGET=x86_64-unknown-linux-musl
rustup target list --installed 2>/dev/null | grep -qx "$TARGET" || rustup target add "$TARGET" >/dev/null

openssl genpkey -algorithm ed25519 -out "$T/key.pem"
openssl genpkey -algorithm ed25519 -out "$T/other.pem"
PUB=$(openssl pkey -in "$T/key.pem" -pubout -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n')
say "building stormbootx-stick ($TARGET) with test key $PUB"
( cd "$ROOT" && STORMBOOTX_UPDATE_TEST_KEY=$PUB CARGO_TARGET_DIR="$T/target" \
    cargo build --locked --release -q -p stormbootx-stick --target "$TARGET" )
BIN="$T/target/$TARGET/release/stormbootx-stick"
file "$BIN" 2>/dev/null | grep -q "statically linked" || say "note: $(file "$BIN" 2>/dev/null)"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)

mk_tree() {  # DIR VERSION
    local d=$1 v=$2
    mkdir -p "$d/EFI/BOOT" "$d/stormboot/drivers"
    printf 'MZ stormbootx %s\n' "$v" > "$d/EFI/BOOT/BOOTX64.EFI"
    printf 'MZ driver %s\n' "$v" > "$d/stormboot/drivers/stormnic-ixgbe.efi"
    printf 'MZ old driver\n' > "$d/stormboot/drivers/stormnic-retired.efi"
    printf '@echo -off\r\n' > "$d/startup.nsh"
    cat > "$d/stormboot/stormboot.conf" <<EOF
portal = 192.168.31.202
fallback = none
update = $URL
media = rustnic test
version = $v
commit = test$v
EOF
}

PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
URL="http://127.0.0.1:$PORT/api/v1/boothelpers/stormbootx-test"
SERVE="$T/serve"
mkdir -p "$SERVE"
( cd "$SERVE" && exec python3 -m http.server "$PORT" --bind 127.0.0.1 ) >"$T/http.log" 2>&1 &
srv=$!
for _ in $(seq 50); do curl -s "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && break; sleep 0.1; done

# release SERIAL VERSION KEY [canary=MAC] [tamper]: published under $URL.
release() {
    local serial=$1 v=$2 key=$3 opt canaries=() tamper=
    shift 3
    for opt in "$@"; do
        case $opt in canary=*) canaries+=("${opt#canary=}") ;; tamper) tamper=1 ;; esac
    done
    local r="$T/rel$serial" at="$SERVE/api/v1/boothelpers/stormbootx-test"
    rm -rf "$r" "$at"
    mkdir -p "$r/EFI/BOOT" "$r/stormboot/drivers" "$at"
    printf 'MZ stormbootx %s serial %s\n' "$v" "$serial" > "$r/EFI/BOOT/BOOTX64.EFI"
    printf 'MZ driver %s serial %s\n' "$v" "$serial" > "$r/stormboot/drivers/stormnic-ixgbe.efi"
    printf '@echo -off\r\nrem serial %s\r\n' "$serial" > "$r/startup.nsh"
    cat > "$r/stormboot/stormboot.conf" <<EOF
portal = 192.168.31.202
fallback = none
update = $URL
media = rustnic test
version = $v
commit = rel$serial
EOF
    {
        echo "stormbootx-manifest 1"
        echo "version $v"
        echo "commit rel$serial"
        echo "golden golden-stormbootx-test-$serial"
        echo "serial $serial"
        for opt in "${canaries[@]}"; do echo "canary $opt"; done
        ( cd "$r" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | while read -r f; do
            echo "file $(sha256sum < "$f" | cut -d' ' -f1) $(stat -c %s "$f") $f"
        done )
    } > "$at/current"
    openssl pkeyutl -sign -inkey "$key" -rawin -in "$at/current" -out "$at/current.sig"
    [[ -n $tamper ]] && sed -i "s/^serial $serial$/serial $((serial + 100))/" "$at/current"
    cp -r "$r" "$at/files"
}

sums() { (cd "$1" && find . -type f | LC_ALL=C sort | xargs sha256sum); }

# stick NAME EXPECTED_EXIT ARGS... -- EXPECTED LINES: run it and check.
stick() {
    local name=$1 want_rc=$2; shift 2
    local args=()
    while [[ $# -gt 0 && $1 != -- ]]; do args+=("$1"); shift; done
    shift || true
    local rc=0
    "$BIN" --status "$T/$name.json" --efivars "$T/no-efivars" --mac 52:54:00:12:34:56 "${args[@]}" \
        > "$T/$name.out" 2>&1 || rc=$?
    sed 's/^/  | /' "$T/$name.out"
    [[ $rc -eq $want_rc ]] || die "[$name] exit $rc, not $want_rc"
    local want
    for want in "$@"; do
        if [[ $want == not:* ]]; then
            grep -qF -- "${want#not:}" "$T/$name.out" && die "[$name] unexpected: ${want#not:}"
        elif [[ $want == json:* ]]; then
            grep -qF -- "${want#json:}" "$T/$name.json" || die "[$name] status lacks: ${want#json:}"
        else
            grep -qF -- "$want" "$T/$name.out" || die "[$name] missing: $want"
        fi
    done
    say "[$name] ok"
}

# --- update: an old stick takes serial 5.
S="$T/stick"
mk_tree "$S" 0.20.0
printf 'name = server1\n' >> "$S/stormboot/stormboot.conf"
printf 'dns = 192.168.31.252\n' > "$S/stormboot/local.conf"
release 5 "$VERSION-test" "$T/key.pem"
stick update 0 --esp "$S" -- \
    "read   : stormbootx 0.20.0 (commit test0.20.0), media rustnic test, serial 0" \
    "ask    : $URL: serial 5, v$VERSION-test (golden-stormbootx-test-5), signature verified" \
    "decide : update: v0.20.0 -> v$VERSION-test (serial 5), 4 file(s)" \
    "1 driver(s) retired" \
    "apply  : kept name = server1 in stormboot/local.conf" \
    "apply  : state: serial 5 on trial, prev 0" \
    "the stick's next boot runs it on trial" \
    'json:"outcome":"updated"' 'json:"from":"0.20.0"' "json:\"serial\":5"
cmp -s "$S/EFI/BOOT/BOOTX64.EFI" "$T/rel5/EFI/BOOT/BOOTX64.EFI" || die "[update] BOOTX64.EFI is not serial 5's"
grep -q "^MZ stormbootx 0.20.0" "$S/EFI/BOOT/BOOTX64.EFI.prev" || die "[update] no BOOTX64.EFI.prev"
[[ -f "$S/stormboot/drivers/stormnic-retired.efi.prev" && ! -e "$S/stormboot/drivers/stormnic-retired.efi" ]] \
    || die "[update] the retired driver was not moved to .prev"
grep -qx "trial = 5 0" "$S/stormboot/state" || die "[update] state: $(cat "$S/stormboot/state")"
grep -qx "dns = 192.168.31.252" "$S/stormboot/local.conf" || die "[update] local.conf lost its own line"
grep -qx "name = server1" "$S/stormboot/local.conf" || die "[update] name not carried"
[[ -z "$(find "$S" -name '*.new')" ]] || die "[update] a .new is left"
sums "$S" | sed 's/^/  : /'

# --- trial: nothing until the stick's next boot judges serial 5.
stick trial 0 --esp "$S" -- "serial 5 is on trial" 'json:"outcome":"current"'

# --- failed: serial 6 failed its trial here before.
F="$T/failed"
mk_tree "$F" "$VERSION"
printf 'serial = 5\nmin = 5\nfailed = 6\n' > "$F/stormboot/state"
release 6 "$VERSION-test" "$T/key.pem"
before=$(sums "$F")
stick failed 0 --esp "$F" -- "serial 6 failed its trial on this stick; staying on serial 5" 'json:"outcome":"declined"'
[[ "$(sums "$F")" == "$before" ]] || die "[failed] the stick changed"

# --- off: a pinned stick.
O="$T/off"
mk_tree "$O" 0.20.0
sed -i 's|^update = .*|update = off|' "$O/stormboot/stormboot.conf"
before=$(sums "$O")
stick off 0 --esp "$O" -- "update = off: this stick is pinned; left alone" 'json:"outcome":"off"' "not:ask    :"
[[ "$(sums "$O")" == "$before" ]] || die "[off] the stick changed"

# --- older: the stick runs a newer release than the promotion.
N="$T/newer"
mk_tree "$N" 99.0.0
stick older 0 --esp "$N" -- "the stick runs v99.0.0; serial 6 offers v$VERSION-test, which is older; not taken"
grep -qx "min = 6" "$N/stormboot/state" || die "[older] the floor was not recorded"
[[ ! -e "$N/EFI/BOOT/BOOTX64.EFI.new" ]] || die "[older] a file was written"

# --- tampered: changed after signing, and signed by another key.
X="$T/tampered"
mk_tree "$X" 0.20.0
before=$(sums "$X")
: > "$T/http.log"
release 7 "$VERSION-test" "$T/key.pem" tamper
stick tampered 1 --esp "$X" -- "does not verify against the" "nothing taken" 'json:"outcome":"refused"' "not:apply  :"
release 7 "$VERSION-test" "$T/other.pem"
stick otherkey 1 --esp "$X" -- "does not verify against the" "not:apply  :"
[[ "$(sums "$X")" == "$before" ]] || die "[tampered] the stick changed"
grep -q "/files/" "$T/http.log" && die "[tampered] a file was fetched: $(grep /files/ "$T/http.log" | head -1)"

# --- canary: serial 8 for another machine only.
release 8 "$VERSION-test" "$T/key.pem" canary=52:54:00:00:00:01
stick canary 0 --esp "$X" -- "serial 8 is for its 1 canaries only; not this machine (52:54:00:12:34:56)"
[[ "$(sums "$X")" == "$before" ]] || die "[canary] the stick changed"

# --- dryrun: what it would do, nothing written.
release 9 "$VERSION-test" "$T/key.pem"
stick dryrun 0 --dry-run --esp "$X" -- "decide : update: v0.20.0 -> v$VERSION-test (serial 9)" "dry run: nothing written" \
    'json:"outcome":"would-update"' 'json:"dry_run":true'
[[ "$(sums "$X")" == "$before" ]] || die "[dryrun] the stick changed"

# --- find: a machine of image files. sda fixed; sdb removable with a slab
# at its partition; sdc on USB and mounted; sdd removable and held by dm-0;
# sde removable and clean, the one candidate.
M="$T/machine"
mkdir -p "$M/sys/block" "$M/sys/devices/pci0000:00/0000:00:14.0/usb1/1-1/block" "$M/dev" "$M/proc/self"
disk() {  # NAME MAJ:MIN REMOVABLE [usb]
    local n=$1 num=$2 rem=$3 usb=${4:-}
    local d="$M/sys/devices/pci0000:00/0000:00:1f.2/ata1/block/$n"
    [[ -n $usb ]] && d="$M/sys/devices/pci0000:00/0000:00:14.0/usb1/1-1/block/$n"
    mkdir -p "$d/holders" "$d/${n}1/holders"
    echo "$rem" > "$d/removable"; echo "$num" > "$d/dev"
    : > "$d/${n}1/partition"; echo 2048 > "$d/${n}1/start"; echo "${num%:*}:$(( ${num#*:} + 1 ))" > "$d/${n}1/dev"
    ln -s "../${d#"$M/sys/"}" "$M/sys/block/$n"
    truncate -s 4M "$M/dev/$n"
}
disk sda 8:0 0
disk sdb 8:16 1
printf 'STRMSLAB' | dd of="$M/dev/sdb" bs=512 seek=2048 conv=notrunc status=none
disk sdc 8:32 0 usb
disk sdd 8:48 1
mkdir -p "$M/sys/block/sdd/holders/dm-0"
disk sde 8:64 1
printf '22 1 8:33 / /stuck rw - vfat /dev/sdc1 rw\n29 1 253:0 / / rw - xfs /dev/ublkb0 rw\n' > "$M/proc/self/mountinfo"
stick find 0 --sys "$M/sys" --proc "$M/proc" --dev "$M/dev" --mnt "$T/mnt" -- \
    "find   : sda: not removable or USB; not a stick" \
    "find   : sdb: refused: it holds a stormblock slab (STRMSLAB at byte 1048576)" \
    "find   : sdc: refused: sdc1 (8:33) is mounted" \
    "find   : sdd: refused: something holds it" \
    "find   : sde1: not read: mount" \
    "no stormbootx stick on this node; nothing to do" 'json:"outcome":"no-stick"'

say "PASS: stormbootx-stick ($(stat -c %s "$BIN") bytes, $TARGET)"
