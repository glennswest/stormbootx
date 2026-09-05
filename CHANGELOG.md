# Changelog

## [Unreleased]

### 2026-09-05
- **milestone: first complete NVMe/TCP attach on real hardware.** A Dell R230
  (service tag C2NR0Q2) booted the agent over iDRAC virtual media and attached a
  32 GiB clone from forge over a 25 GbE Mellanox port: `claimed a clone of this
  machine's image`, `namespace 8388608 blocks x 4096 bytes`, `transfer 128 KiB
  per command (controller MDTS 5)`, `blockio published`, `RESULT: remote image
  is a local disk`. Every v0.3.0 fix confirmed on metal in one boot — the
  4096-byte LBA read from FLBAS, the MDTS-derived transfer size, multi-NIC
  selection, and the service-tag claim end to end. The three-day bring-up hit
  five walls (UEFI stack disabled, wrong-NIC `NO_MAPPING`, `GlobalSlotDriver`
  hiding both add-in cards, a switch/Mellanox FEC mismatch, and a red-herring
  DHCP relay), every one infrastructure or firmware rather than the binary. See
  the Status section in CLAUDE.md.


### 2026-09-04
- **fix (tcp4): try every network interface, not the first one.** `connect_within`
  took `handles.first()` and never looked at the others. A server has more than
  one NIC — a 1 GbE management port and a 25 GbE data port — and each carries its
  own network stack, so that was a coin flip. Landing on the port with no cable
  produces exactly the symptom seen on the Dell: `tcp4 : available`, because
  *some* interface has a stack, then `NO_MAPPING` forever, because *that* one has
  no link and never will. No amount of waiting fixes a socket on the wrong NIC.
- **feat (tcp4): try the fastest interface first.** This is the storage path, so
  the NIC that matters is the one somebody wired for it. Interfaces are ranked
  before any is tried: link state, then descending MTU, then enumeration order.
  MTU stands in for speed because it is the honest signal available — 9000 means
  somebody configured that port for storage, 1500 means they did not — and
  because it costs nothing, coming from the SNP mode `EFI_TCP4.GetModeData`
  already returns. No `EFI_ADAPTER_INFORMATION_PROTOCOL`, which is another
  optional stack. A port with no media is ranked last rather than skipped: SNP
  may not know, and dropping the only working interface is worse than one extra
  attempt.
- **feat (dhcp4): get an address ourselves instead of hoping firmware did.**
  `Configure` with `use_default_address` needs the platform's IP4 driver to
  already hold an address, i.e. somebody else's DHCP client to have run. On a
  server that is not a given — the policy may be `STATIC`, or the platform may
  only run DHCP as part of a PXE attempt nobody asked for — and the symptom is
  `NO_MAPPING` with nothing to wait for. `EFI_DHCP4_PROTOCOL` is now driven
  directly and the lease goes into `Tcp4ConfigData` as an explicit
  `station_address`, so nothing downstream depends on the platform's IP4 setup.
  Matched by MAC, since a DHCP4 and a TCP4 binding on the same NIC are different
  handles. A **fallback**, never the first move: DHCP4 is an optional stack, so
  a machine whose firmware lacks it is exactly as well off as before.
- **feat (tcp4): ask the platform to run DHCP before waiting on it.**
  `EFI_IP4_CONFIG2`'s policy is set to `DHCP` on every interface not already on
  it, so leases are in flight everywhere while the retry loop runs.

### 2026-09-03
- **fix (tcp4): wait for the network stack instead of asking once.** On the Dell
  (C2NR0Q2), same firmware and same boot session, the *first* boot option
  reported `EFI_TCP4 is not present` and the *second* — seconds later — found it
  available and already bound. `ensure_available` ran one `ConnectController`
  pass and checked immediately, so a platform that had not yet dispatched the
  NIC's driver was recorded as one that carries no network stack at all.
  `ConnectController` cannot bind a driver the platform has not loaded, so more
  passes were never the answer: it now retries the full pass every 250 ms for up
  to 5 s and reports how long it took, which also tells the two candidate causes
  apart — a driver dispatched late, or a stack that binds asynchronously. The
  window is spent only on a machine that was going to fail anyway, against a
  boot that falls through to the local disk because the network was a moment
  late, which is a machine nobody provisioned. `tcp4probe` reports the new
  verdict too.
- **verified: the UEFI network stack is a setup switch, and flipping it works.**
  With it enabled the Dell reports `tcp4 : available`, already bound.
- **feat (smbios): print the model next to the service tag.** Whether a platform
  carries the TCP/IP driver stack at all is a per-*model* fact — the first
  hardware run stopped at `EFI_TCP4 is not present` — so the console now names
  the machine it is running on, which makes that a note someone can write down
  against a model rather than against one machine. Manufacturer and product come
  from SMBIOS Type 1 offsets 0x04 and 0x05.
- **fix (smbios): bounds-check the Type 1 field offset.** A short Type 1 is
  legal, the fields having been added over successive SMBIOS versions, and
  reading past the structure's own length walks into the string table and
  returns whatever byte sits there as a string index.
- **verified: the agent runs on real hardware.** A Dell (C2NR0Q2) booted it from
  USB and read its own service tag out of SMBIOS with no network, no DHCP and no
  BMC. It stopped at `EFI_TCP4 is not present`, after the full
  `ConnectController` pass — the UEFI network stack disabled in firmware setup,
  not a fault in the binary.

## [v0.3.0] — 2026-09-03

The release that stops asking the network where to boot and starts asking the
appliance which image is this machine's. First code to run on real firmware.

### Breaking
- **DNS discovery is gone — the code, not just the default.** `src/dns.rs`,
  `scripts/publish-portal-dns.sh` and `tests/dns-wire/` are removed, along with
  the `zone` and `discover` settings and `Defaults.zone`; `config::resolve` no
  longer takes a `note` callback because nothing in resolution talks to the
  network any more. Discovery was the default while the portal was the thing a
  machine had to be told. The portal is now a fixed appliance address and the
  question worth answering is *which image*, which the service tag answers
  against that appliance — so DNS in front of it was a second place for the
  answer to live, a resolver that had to be right before a machine could boot,
  and a timeout on every boot in a zone nobody published (`storm.lo` does not
  exist on the g8 resolver). This reverses #1 and is in history if a network
  ever needs one image booting everywhere with no per-network config. A stick
  carrying `zone` or `discover` is now unaffected by either.

### Added
- **A machine claims its own image by service tag (#4).** Which image a machine
  boots is a fleet decision and it lives next to the images, as a
  `boothost/<service tag>` synonym on the storage engine rather than on the
  media or in DHCP. `POST /api/v1/synonyms/boothost/<tag>/claim` returns a
  copy-on-write clone of the assigned golden *and* the address, NQN and NSID
  reaching it, in one request — so moving a box to a new version is a `PUT` on
  its name, with nothing on the stick to change and nobody visiting the machine.
  Keyed on the service tag because that names the chassis and survives a NIC
  being swapped. `api_port` (default 9090) says where the engine API is;
  `claim = no` opts a stick out. Verified against forge: `boothost/C2NR0Q2` →
  `stormcos-sno-10.22`, claim answers 201, and that exact body parses correctly.
  **A claim that fails is not a failed boot** — no synonym, a 404, an engine
  that is down: the console says which and the boot continues on whatever
  resolution produced, because a claim that fails must not be what keeps a
  fleet down.
- **SHA-256 in-tree** (`src/sha256.rs`), the half of #2 that waited on nothing.
  `EFI_HASH2` is an optional driver stack, the same trap `EFI_HTTP` already set
  here. Streaming, so the update path can hash a file as it reads it rather
  than holding a whole payload in pool. `Digest::matches_hex` tolerates a
  `sha256:` prefix, either case and surrounding whitespace, and nothing else:
  #2 reads "not a match" as "do not swap". It names no `crate::` item and
  touches only `core`, so `rustc --edition 2021 --test src/sha256.rs` runs the
  FIPS vectors despite the crate having no host target — 7 tests, 0 failed.
- **An attach is read in either spelling.** sbregistry answers `address`/`port`;
  stormblock answers `traddr`/`trsvcid` inside an `addresses` array. Both are
  accepted rather than one being chosen, because the alternative is a boot path
  that fails on a field name while the two ends move independently.

### Fixed
- **The transfer size inverted on a jumbo path.** `chunk_for_mtu` sized a
  command so one reply landed in one frame, so a 9000 path rounded down to
  **8 KiB** while a 1500 path took the 64 KiB fallback — eight times less data
  per round trip on the faster network. The frame argument does not survive
  contact with TCP: NVMe/TCP rides a byte stream, the stack segments it to the
  MSS and IP never fragments it. `read` keeps one command outstanding, so
  throughput is transfer ÷ RTT and bigger is strictly better up to what the
  controller accepts. It now asks — Identify Controller (CNS 01h) after `CC.EN`
  gives MDTS in `CAP.MPSMIN` pages, capped at 512 KiB and floored at one block;
  MDTS 0 or a silent controller keeps the 64 KiB every controller accepts.
  Against the stormblock target (MDTS 5, MPSMIN 0) that is **128 KiB per
  command instead of 8 KiB**, 16× the bytes per round trip. The MTU is still
  read and printed, and the console says `jumbo` when it sees one.
- **A transfer shorter than one block could wrap.** CDW12 carries NLB as a
  0-based count, so a limit below the block size computed `blocks - 1` on zero.
  Reachable on a namespace reporting a block size above the transfer limit,
  which the format permits up to 64 KiB.
- **`config::write_file` claimed to truncate and did not.** `FileMode::
  CreateReadWrite` opens an existing file without truncating, and seeking to
  the new end does not shorten it, so a shrinking rewrite left the tail of the
  previous file behind — a stale `stamp` or `portal` line surviving past the
  value that replaced it is a machine attaching somewhere nobody chose. Latent
  until #2 calls it.

### Changed
- **`Cargo.lock` is tracked.** It was neither committed nor ignored, so every
  build resolved fresh — and the whole dependency surface here is two crates
  that move: a build offered `uefi` 0.40 against the 0.39 the code was written
  for. A firmware binary should not change because a dependency did while
  nobody was looking.

### Documentation
- **The network path runs under Proxmox OVMF.** `tcp4probe` on VM 2062 reports
  every network protocol absent as found and all nine present after a
  `ConnectController` pass, then configures a TCP4 child. Fedora's OVMF cannot
  do this, which had left the network path with no emulator; Proxmox's build
  carries HTTP boot and so carries TCP4. A Proxmox VM can present a service tag
  too — `smbios1` takes a base64 `serial=`, the SMBIOS Type 1 field the agent
  reads, empty unless set.
- The README claimed stormblock exposes no discovery controller. It does —
  `DISCOVERY_NQN`, log page `0x70`, `CNTRLTYPE=2`.

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
