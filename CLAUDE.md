# CLAUDE.md — stormbootx

A UEFI application that attaches a remote image over NVMe/TCP and publishes it
as `EFI_BLOCK_IO_PROTOCOL`, so the firmware's own partition, FAT and boot
manager machinery boots a disk that is not in the chassis. 57 KB, `no_std`, one
firmware protocol dependency (`EFI_TCP4`).

Read the cross-project rules in `../CLAUDE.md` first — in particular **build on
`dev.g8.lo`, never on the Mac**, and **nothing persists on the SSD**.

## Build

```bash
ssh root@dev.g8.lo
cd /root/work/stormbootx && git pull
export CARGO_TARGET_DIR=/build/cargo/stormbootx
cargo build --release --target x86_64-unknown-uefi
```

There is no `cargo test`: this is a `no_std` UEFI binary with no host target, so
the compile *is* the check. A macOS build is not possible at all here — the
target is `x86_64-unknown-uefi` and the code is entirely firmware-facing.

`src/sha256.rs` is the one exception, and it is worth keeping. It touches only
`core` and names no `crate::` item, so it compiles standalone as its own crate
and the FIPS vectors can actually be run:

```bash
rustc --edition 2021 --test src/sha256.rs -o $CARGO_TARGET_DIR/sha256-test && \
  $CARGO_TARGET_DIR/sha256-test
```

`--edition 2021` is not optional: bare `rustc` defaults to edition 2015, where
`core` is not in scope and the file will not compile even though it is correct.

Anything that gives that module a dependency on the rest of the crate takes
those vectors out of reach. Don't.

`Cargo.lock` is tracked, as it should be for anything that produces a binary.
Without it every build resolved fresh, and this is a firmware binary whose
whole dependency surface is two crates that move: a `cargo build` on 2026-09-02
offered `uefi` 0.40 against the 0.39 the code was written for. Nothing here
should change under a build nobody asked to change it. Bump the pin
deliberately, in its own commit, and rebuild.

`./scripts/build-boot-agent.sh` writes the GPT/ESP image to `/build/images`.

## Version locations

| File | Field |
|---|---|
| `Cargo.toml` | `version` |
| `CHANGELOG.md` | latest release heading |
| git tag | `vX.Y.Z` |

## Module map

| File | Job |
|---|---|
| `src/smbios.rs` | the service tag, before any network exists |
| `src/tcp4.rs` | a blocking socket over the firmware's own TCP stack |
| `src/dns.rs` | SRV/TXT discovery of the portal over DNS/TCP |
| `src/nvme.rs` | the NVMe/TCP initiator |
| `src/blockio.rs` | publish the namespace as a block device, then `ConnectController` |
| `src/registry.rs` | claim an image from sbregistry, keyed on the service tag |
| `src/sha256.rs` | the digest, because `EFI_HASH2` is optional |
| `src/config.rs` | the target, read from the media rather than compiled in |
| `src/tcp4probe.rs` | second binary: does this machine's firmware carry TCP4? |

## Load-bearing facts

These have each cost a debugging session. Do not "simplify" them away.

- **PSDT = 01b (`FLAGS_SGL`) on every NVMe command.** A zero FLAGS byte says
  "PRPs are used", and there are no PRPs over a fabric. stormblockmk does not
  validate it; the Linux target rejects the command with Invalid Field.
- **`CC.EN` before any admin command.** Fabrics Connect only establishes a
  queue. Identify before the controller is enabled gets Command Sequence Error.
- **`Poll` or nothing completes.** EFI networking is asynchronous; a token that
  is never pumped never retires and the boot hangs with no error.
- **`ConnectController` after installing BlockIO.** Installing the protocol
  alone leaves a block device nothing has looked at — no GPT parsed, no ESP.
- **Fedora's OVMF has no upper network stack.** SNP present, MNP/IP4/TCP4
  absent, and `ConnectController` over every handle does not change it. The
  obvious emulator cannot test the network path.
- **A boot path must never need the network in order to boot without it.**
  Every failure in discovery or attach falls through to the local disk. One
  provisioning outage must not become a fleet outage.

## Work plan

### Done

- [x] #6 — derive the NVMe transfer size from the path MTU (`GetModeData`)
- [x] #5 — bind layered network drivers before declaring TCP4 absent; land
      `tcp4probe` as a permanent per-server-model diagnostic
- [x] #1 — discover the portal over DNS SRV/TXT (`_nvme-disc._tcp.<domain>`)
- [x] #3 (the load-bearing half) — every failure path falls through to the
      local disk instead of stopping
- [x] SHA-256 in-tree (`src/sha256.rs`) — the part of #2 that waited on
      nothing. Verified on dev: 7 tests over the FIPS vectors, the padding
      boundary and every streaming split. Unreferenced until #2 wires it up,
      and LTO drops it, so it costs the image 0 bytes today.

### Blocked on other repos

- [ ] #3 (the rest) — the version compare needs `stormblock-pallet-format`
      (stormblock) linked in for the intended version, and a marker that a
      booted stormcos node writes where firmware can read it before any OS
      runs. The marker is filed as **stormcos#30** with a proposed shape.
- [ ] #4 — registration by service tag against a `BootHost` object. The client
      half is small; the server half (a registration endpoint, a service-tag
      key, a `bootAgent` field) is filed as **stormnetboot#8**.
- [ ] #2 — self-update of the boot media. SHA-256 is done; what is left is
      gated on #4, because updating to "whatever was on the last image
      attached" is exactly the uncontrolled update this must not become. The
      next piece that needs no one else is hashing a file through
      `EFI_FILE_PROTOCOL` a buffer at a time — the streaming API is already
      shaped for it.

## Status

v0.2.0. Built and verified as an artifact; **not yet run on hardware**. The
first line to watch on a real machine is `tcp4 : available` — and on a model
nobody has tried, run `tcp4probe` before writing an agent stick at all.
