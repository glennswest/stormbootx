# Changelog

## [Unreleased]

### 2026-09-03
- **BREAKING: DNS discovery is gone — the code, not just the default.**
  `src/dns.rs`, `scripts/publish-portal-dns.sh` and `tests/dns-wire/` are
  removed, along with the `zone` and `discover` settings and the `zone` field of
  `Defaults`; `config::resolve` no longer takes a `note` callback because
  nothing in it talks to the network any more. Discovery was the default while
  the portal was the thing a machine had to be told. The portal is now a fixed
  appliance address and the question worth answering is *which image*, which the
  service tag answers against that appliance — so DNS in front of it was a
  second place for the answer to live, a resolver that had to be right before a
  machine could boot, and a timeout on every boot in a zone nobody published
  (`storm.lo` does not exist on the g8 resolver). This reverses #1; it is in
  history if a network ever needs one image booting everywhere with no
  per-network config. The stick now names its portal and claims its image by
  tag, and `build-boot-agent.sh` gains `--api-port` and loses `--zone`.
- **docs:** the README claimed stormblock exposes no discovery controller. It
  does — `DISCOVERY_NQN`, log page `0x70`, `CNTRLTYPE=2`. TXT carries the NQN
  because one answer in DNS beats a second Connect and a log-page walk, not
  because there is nothing to ask.
- **feat: a machine claims its own image by service tag (#4).** Which image a
  machine boots is a fleet decision, and it now lives next to the images as a
  `boothost/<service tag>` synonym on the storage engine rather than on the
  media or in DHCP. `POST /api/v1/synonyms/boothost/<tag>/claim` returns a
  copy-on-write clone of the assigned golden *and* the address, NQN and NSID
  reaching it, in one request — so moving a box to a new version is a `PUT` on
  its name, with nothing on the stick to change and nobody visiting the machine.
  Keyed on the service tag because that names the chassis and survives a NIC
  being swapped. The engine API is the same host as the portal: `api_port`
  (default 9090) says where, `claim = no` opts a stick out.

  Verified against forge: `boothost/C2NR0Q2` → `stormcos-sno-10.22`, claim
  answers 201 with `attach.address/port/nqn/nsid`, and that exact body parses
  correctly — `"protocol"` is not mistaken for `"port"`, and the `?nsid=` inside
  the returned URI is not reached before the real `nsid` key.

  **A claim that fails is not a failed boot.** No synonym, a 404, an engine that
  is down: the console says which and the boot continues on whatever resolution
  produced, because a claim that fails must not be what keeps a fleet down.
- **feat (registry): an attach is read in either spelling.** sbregistry answers
  `address`/`port`; stormblock answers `traddr`/`trsvcid` inside an `addresses`
  array. Both are now accepted rather than one being chosen, because the
  alternative is a boot path that fails on a field name while the two ends move
  independently. The nesting costs nothing — `field` scans for a key with its
  closing quote, so it reads `traddr` out of the array without a JSON parser,
  and `"address"` does not false-match inside `"addresses"`.
- **verified: the network path runs under Proxmox OVMF.** `tcp4probe` on VM
  2062 reports every network protocol absent as found and all nine present
  after a `ConnectController` pass, then configures a TCP4 child successfully.
  Fedora's OVMF cannot do this, which had left the whole network path with no
  emulator; Proxmox's build carries HTTP boot and so carries TCP4. A Proxmox VM
  can also present a service tag — `smbios1` takes a base64 `serial=`, which is
  the SMBIOS Type 1 field the agent reads.
- **fix (nvme): the transfer size inverted on a jumbo path.** `chunk_for_mtu`
  sized a command so one reply landed in one frame, which meant a 9000 path
  rounded down to **8 KiB** a command while a 1500 path took the 64 KiB
  fallback — eight times less data per round trip on the faster network. The
  frame argument does not survive contact with TCP: NVMe/TCP rides a byte
  stream, the stack segments it to the MSS and IP never fragments it, so a
  large PDU on a 9000 path is several segments, not a reassembly problem.
  `read` issues one command at a time and waits for it, so throughput is
  transfer ÷ RTT and bigger is strictly better up to what the controller
  accepts. It now asks: Identify Controller (CNS 01h) after `CC.EN` gives
  MDTS, counted in `CAP.MPSMIN` pages, capped at 512 KiB and floored at one
  block; MDTS 0, or a controller that will not answer, keeps the 64 KiB every
  controller accepts. Against the stormblock target (MDTS 5, MPSMIN 0) this is
  **128 KiB per command instead of 8 KiB** — 16× the bytes per round trip.
  The MTU is still read and still printed, and the console now says `jumbo`
  when it sees one; it no longer sizes anything.
- **fix (nvme): a transfer shorter than one block could wrap.** CDW12 carries
  NLB as a 0-based count, so a limit below the block size computed
  `blocks - 1` on zero. Reachable on a namespace reporting a block size above
  the transfer limit, which the format permits up to 64 KiB.

### 2026-09-02
- **chore:** `Cargo.lock` is tracked, for both this crate and the `dns-wire`
  test helper. It was neither committed nor ignored, so every build resolved
  fresh — and the whole dependency surface here is two crates that move: a
  build today offered `uefi` 0.40 against the 0.39 the code was written for. A
  firmware binary should not change because a dependency did while nobody was
  looking. The pin now moves deliberately, in its own commit.
- **feat:** SHA-256 in-tree (`src/sha256.rs`), the half of #2 that waits on
  nothing. `EFI_HASH2` is an optional driver stack, the same trap `EFI_HTTP`
  already set here — code written against a protocol firmware is allowed to
  omit works on the desk and fails on the one server model that matters, at
  the point in the boot with nothing to read the failure from. Streaming, so
  the update path can hash a file as it reads it rather than holding a whole
  payload in pool. `Digest::matches_hex` is tolerant of a `sha256:` prefix,
  either case and surrounding whitespace, and of nothing else: a short, long
  or non-hex stamp is not a match, and #2 reads "not a match" as "do not
  swap". The module names no `crate::` item and touches only `core`, so
  `rustc --edition 2021 --test src/sha256.rs` runs the FIPS vectors against it
  despite the crate having no host target — see the note in the module header.
  Verified on dev: 7 tests over the FIPS vectors, the 55/56/64-byte padding
  boundary and every streaming split, 0 failed. Nothing references it yet, so
  LTO drops it and the image is the same size it was.
- **fix:** `config::write_file` claimed to truncate and did not. `FileMode::
  CreateReadWrite` opens an existing file without truncating and seeking to the
  new end does not shorten it, so a shrinking rewrite left the tail of the
  previous file behind — where a stale `stamp` or `portal` line surviving past
  the value that replaced it is a machine attaching somewhere nobody chose. It
  now does the `SetInfo` with a smaller `FileSize` that actually shortens a
  file. Latent (nothing calls it until #2), found while writing up #2.

## [v0.2.0] — 2026-09-02

### Breaking
- No failure stops the boot any more (#3). Every path — no service
  tag, no TCP stack, no resolver, no portal, a target that refuses — falls
  through to the local disk, because a boot path that needs the network in
  order to boot *without* the network turns one provisioning outage into a
  fleet outage. The console says which case it is, and `blockio::local_disks`
  counts what there actually is to fall back to so the message is honest: five
  seconds when a local disk exists, thirty when nothing does and a human is
  genuinely needed.

### Added
- The portal is discovered over DNS (#1). `_nvme-disc._tcp.<zone>`
  SRV and TXT, resolved over DNS/TCP (RFC 7766) through the existing `tcp4.rs`,
  against the resolvers `EFI_IP4_CONFIG2` holds from DHCP. Resolution order is
  now config file, then DNS, then the compiled floor; a `portal` line in the
  file pins a machine and turns discovery off.
- `scripts/publish-portal-dns.sh` publishes the A/SRV/TXT records to a
  network's microdns, and `tests/dns-wire/` exercises the wire parser — the one
  part of this that can be tested without a machine to boot — against a real
  resolver and against compression pointers, pointer loops, priority/weight
  selection and every truncation of a valid answer.
- `Tcp4Socket::connect_within` bounds a connect, so a resolver that is
  not there costs five seconds rather than thirty. The attach keeps the long
  budget: by then there is nothing to fall through to.
- `tcp4probe`, a second UEFI binary (24 KB) that answers "will
  stormbootx run on this server model?" before anyone writes a stick (#5). It
  surveys the nine protocols of the network stack layer by layer, runs a
  `ConnectController` pass if TCP4 is missing, surveys again, then creates and
  configures a TCP4 child — because presence is necessary and not sufficient.
- `stormbootx` binds the firmware's own layered network drivers before
  declaring `EFI_TCP4` absent (#5). Drivers that are built in but unbound are
  the likeliest cause on enterprise firmware and the fix for them is free; the
  console says which of the three ways TCP4 turned out to be reachable.

### Fixed
- `scripts/build-boot-agent.sh` pointed `cargo build` at
  `crates/stormbootx/Cargo.toml`, which does not exist in this repo — the script
  could not have built anything. It now takes `--pin` (write a portal and
  disable discovery) and `--probe` (a stick that boots `tcp4probe`), and
  defaults to a stick that names no target at all.
- The NVMe transfer size is derived from the path MTU rather than a
  hand-edited constant (#6). `EFI_TCP4.GetModeData` reports the link MTU; a
  jumbo path gets a command sized to one frame (8 KiB at MTU 9000) and every
  other path gets 64 KiB, which is faster where nothing aligns to a frame
  anyway because this client has no read pipelining. The chosen size and the
  MTU it came from are printed on the console.

### Documentation
- Build is warning-free, so a new warning is visible as one.
- Project `CLAUDE.md` (build, module map, load-bearing facts, work plan)
  and this changelog.

## [v0.1.0] — 2026-09-02

### Added
- `stormbootx`, a UEFI NVMe/TCP boot extension: read the service tag out of
  SMBIOS, attach a remote image over `nvme-tcp://`, publish it as
  `EFI_BLOCK_IO_PROTOCOL` and let the firmware boot it.
- `smbios.rs` — SMBIOS type 1 serial number, read with no network.
- `tcp4.rs` — a blocking socket over `EFI_TCP4_PROTOCOL`.
- `nvme.rs` — NVMe/TCP initiator: ICReq/ICResp, Fabrics Connect, admin and I/O
  queues, R2T/H2CData writes.
- `blockio.rs` — install `EFI_BLOCK_IO_PROTOCOL` and `ConnectController`.
- `registry.rs` — claim an image from sbregistry over plain HTTP on TCP4.
- `config.rs` — read the target from `\stormboot\stormboot.conf` on the volume
  found via `EFI_LOADED_IMAGE_PROTOCOL`.
- `scripts/build-boot-agent.sh` — build the GPT/ESP boot image.
