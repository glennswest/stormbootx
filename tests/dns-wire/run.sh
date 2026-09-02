#!/bin/bash
# Extract the wire-format half of src/dns.rs and exercise it on the host.
#
# Verbatim extraction rather than a copy: a test that drifts from the code it
# tests is worse than no test, and this is the one part of stormbootx that can
# be run without a machine to boot.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
DNS="$HERE/../../src/dns.rs"
RESOLVER="${1:-1.1.1.1}"

[[ -f "$DNS" ]] || { echo "no src/dns.rs at $DNS" >&2; exit 1; }

{
    sed -n '/^const TYPE_A/,/^const CLASS_IN/p' "$DNS"
    sed -n '/^fn build_query/,$p' "$DNS"
} | sed -e 's/^use alloc::/use std::/' \
        -e 's/^fn \(build_query\|read_name\|records\|parse_srv\|find_a\|parse_txt\)/pub fn \1/' \
        -e 's/^struct Record {/pub struct Record {/' \
    > "$HERE/src/parser.rs"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/build/cargo/dns-wire}"
cd "$HERE" && exec cargo run --quiet -- "$RESOLVER"
