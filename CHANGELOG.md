# Changelog

## [Unreleased]

### 2026-09-02
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
  `rustc --test src/sha256.rs` runs the FIPS vectors against it despite the
  crate having no host target — see the note in the module header.
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
