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
| `src/nvme.rs` | the NVMe/TCP initiator |
| `src/blockio.rs` | publish the namespace as a block device, then `ConnectController` |
| `src/registry.rs` | claim this machine's image, keyed on the service tag |
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
- **Proxmox's OVMF does, and it is the emulator to use.** Verified 2026-09-03
  on pve.g8.lo (`pve-edk2-firmware`, Nov 2025) with `tcp4probe` on VM 2062:
  every protocol reads *absent* as found and all nine appear after a
  `ConnectController` pass — the `BoundAfterFullPass` case #5 added. The boot
  menu lists `UEFI HTTPv4/v6`, which is why: HTTP boot pulls TCP4 in. So the
  network path *can* be exercised in a VM, on Proxmox rather than on Fedora's
  OVMF.
- **The transfer size comes from MDTS, never from the MTU.** Sizing a command
  to fit one frame inverts: a 9000 path lands on 8 KiB and a 1500 path on
  64 KiB. TCP segments to the MSS and never IP-fragments, so frames are not
  the constraint; round trips are, because `read` keeps one command
  outstanding.
- **There is no DNS in this binary, and the service tag is the selection
  path.** The portal is a fixed appliance address named on the media or
  compiled in; *which image* is a `boothost/<tag>` synonym claimed from the
  engine. Discovery was removed rather than switched off — it was a second
  place for the answer to live and a timeout on every boot in a zone nobody
  published. Don't reintroduce it without a network that needs one image
  booting everywhere with no per-network config.
- **A boot path must never need the network in order to boot without it.**
  Every failure in discovery or attach falls through to the local disk. One
  provisioning outage must not become a fleet outage.

## Work plan

### Done

- [x] #6 — size the NVMe transfer from the controller's MDTS. Was derived
      from the path MTU, which inverted: a 9000 path got 8 KiB and a 1500 path
      64 KiB. Against the stormblock target (MDTS 5, MPSMIN 0) it is now
      128 KiB a command.
- [x] #5 — bind layered network drivers before declaring TCP4 absent; land
      `tcp4probe` as a permanent per-server-model diagnostic
- [x] #1 — discover the portal over DNS SRV/TXT. **Removed 2026-09-03**, code
      and all (`src/dns.rs`, `scripts/publish-portal-dns.sh`,
      `tests/dns-wire/`). It answered *where*, and where turned out to be a
      fixed appliance address; *which image* is the question worth asking and
      the service tag answers it. Recoverable from history if a network ever
      needs one image booting everywhere with no per-network config.
- [x] #3 (the load-bearing half) — every failure path falls through to the
      local disk instead of stopping
- [x] #4 (the selection half) — a machine claims its own image with
      `POST /api/v1/synonyms/boothost/<tag>/claim`. Verified against forge with
      the real `boothost/C2NR0Q2` synonym. The *registration* half — reporting
      memory, MACs, CPU, class, storage and storage controllers back — is still
      open on #4, blocked on the payload the appliance accepts.
- [x] SHA-256 in-tree (`src/sha256.rs`) — the part of #2 that waited on
      nothing. Verified on dev: 7 tests over the FIPS vectors, the padding
      boundary and every streaming split. Unreferenced until #2 wires it up,
      and LTO drops it, so it costs the image 0 bytes today.

### Blocked on other repos

- [ ] #3 (the rest) — the version compare needs `stormblock-pallet-format`
      (stormblock) linked in for the intended version, and a marker that a
      booted stormcos node writes where firmware can read it before any OS
      runs. The marker is filed as **stormcos#30** with a proposed shape.
- [ ] #4 (the registration half) — reporting this machine's inventory back.
      Everything wanted is reachable before any OS: MACs from
      `EFI_SIMPLE_NETWORK`, memory/CPU/chassis from SMBIOS types 17/16/4/3,
      storage from `EFI_BLOCK_IO`, controllers from `EFI_PCI_IO` class `0x01`.
      Two constraints: collect it **before** `blockio::publish`, or the machine
      reports the namespace it just attached as its own hardware; and `BLOCK_IO`
      only shows what firmware bound a driver for, so the PCI scan is needed as
      well as, not instead of. Blocked on the payload shape.
- [ ] #2 — self-update of the boot media. SHA-256 is done; what is left is
      gated on #4, because updating to "whatever was on the last image
      attached" is exactly the uncontrolled update this must not become. The
      next piece that needs no one else is hashing a file through
      `EFI_FILE_PROTOCOL` a buffer at a time — the streaming API is already
      shaped for it.

## Status

v0.3.0. Built and verified as an artifact. **First real execution: 2026-09-03**
— `tcp4probe` ran under Proxmox OVMF on VM 2062 (`stormbootx-test.g8.lo`) and
reported TCP4 available after a full `ConnectController` pass, then configured
a TCP4 child and got as far as a connect timeout, which is the probe's own
"the stack works" case. The agent itself has still not attached anything.

A Proxmox VM can carry a service tag: `smbios1` takes `serial=` base64-encoded
(`QemuServer.pm:1593`), which is the SMBIOS Type 1 field `smbios.rs` reads. It
is empty unless set — a VM with only `uuid=` reports no serial and the agent
falls through to the local disk before it ever reaches the network.

On a *physical* model nobody has tried, still run `tcp4probe` before writing an
agent stick at all.
