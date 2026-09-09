# Changelog

## [Unreleased]

### 2026-09-09
- **refactor(ops): the switch-side scripts move to their own repo, `dswfecfix`.** `scripts/fec-cycle.sh`, `scripts/port-latched.sh|.py` and `scripts/fec-watchdog.sh` are removed here and superseded by a static Rust binary that runs **on dsw1 itself** under cron, covering all 48 ports with a bounce → FEC cycle → autoneg-toggle ladder and a backoff that never gives up. It needs no password: `cpsusers` can write `fec`, `enabled` and `auto-negotiation` through CPS directly. This is a switch defect, not a stormbootx one, and two implementations of the same recovery would be exactly the second place for the answer to live that this project's own history warns about. Recoverable from history; the shell versions worked and are what proved the mechanism.
- **fix(ops): the S5148F latches a 25G port when the peer powers off, and only a FEC transition clears it.** C2NR0Q2's `1/1/5` and `1/1/7` dropped together at 01:33:32 UTC after six days of clean traffic (63.4M packets in, 1 CRC) and stayed down through four host power cycles, a host bounce and a switch-side `shutdown`/`no shutdown`. Nothing had changed on either end: no admin login on dsw1 since 2026-09-08 12:52, no SEL entry on the host since 09-08 00:31, card NV FEC `device-default` on all four port entries, firmware `14.32.10.10`, boot ISO 0.3.8 with no `fec =` line and the self-heal off. The switch received healthy light throughout (`+0.64` and `−1.53 dBm`) and never achieved PCS lock. Walking the port through `no fec` → `CL91-RS` → `CL74-FC` → `CL108-RS` brought both up at 25G **on the final stage — the value they had been configured with all along**, so the FEC value was never wrong; the transition is what re-programs the serdes. Second occurrence of the signature (see 2026-09-07), and it reproduces on every peer power-off.
- **feat(ops): `scripts/fec-cycle.sh`** — the recovery, scripted. Walks the four FEC stages with a shut/no-shut each, surfaces anything clish rejects instead of swallowing it, reads link state between stages over the passwordless login, and always ends on `CL108-RS`. Never leaves a port on `off`: no FEC on 25GBASE-SR is out of spec and is what broke the fabric on 2026-09-06.
- **feat(ops): `scripts/fec-watchdog.sh`** — the recovery on a timer, because the switch-side fix is not coming: the S5148F is XPliant-based, its last OS10 build is 10.4.3.8, and upgrading is off the table. Conservative by construction: a port must read latched on **two consecutive passes** before anything is written (one sample is what the 0.3.4 self-heal believed, and it cost a card a bad NV write), a cooldown bounds a port the cycle cannot fix to one attempt per window instead of a config write per cron tick, and a powered-off peer cannot trigger it at all because the detector requires real light. Logs whether each port recovered or still needs a human.
- **feat(ops): `scripts/port-latched.sh`** — tells the latch apart from the three faults it imitates. The signature is `admin up + optic present + real light + oper down`; a genuinely dark peer reads the `−40.0` floor and an empty cage produces no media record. Read-only, no password, exits 1 when a port is latched so it can gate the recovery from cron.

## [v0.3.8] — 2026-09-07

### Added
- **feat(config): `fec = MODE` on the media, and `--fec MODE` to build a recovery stick (#7).** Stated in `stormboot.conf` and read by `config::stated_fec()` before the network is touched, it writes the ConnectX NV FEC override once and warm-resets. Needed because the v0.3.7 shell command is only reachable *after* the attach fails, and on a machine with no link that means sitting through four DHCP timeouts before the prompt appears — with the SOL session dropping mid-boot more often than not. A recovery stick does it unattended, on the first boot, with no console at all. **Absent from every normal stick, and that is the point:** this is an operator saying so on one piece of media, not the boot path inferring it from link state, which is exactly the distinction step 2b exists to draw. An unparseable value is ignored rather than guessed at.

## [v0.3.7] — 2026-09-07

### Added
- **feat(shell): `fec [MODE]` and `reset` — the operator-driven way back (#7).** `fec` alone reads every ConnectX port's current and next-boot FEC; `fec default|rs|fc|off|autoneg` writes the NV override; `reset` warm-resets so firmware re-reads it. This is now the **only** thing in stormbootx that writes FEC, and a person has to type it. Needed because switching the self-heal off in v0.3.6 stops the write recurring but does not undo the one already on the card: C2NR0Q2 booted v0.3.6 and reported `02:00.0/02:00.1 ConnectX-4 Lx port 1/2: current rs, next boot rs` on all four entries — the self-heal's write, confirmed on the metal. Both ports lit and neither linked against a switch on `fec CL108-RS`, so the card's NV `rs` (TLV value 2) is not the same RS the S5148F means by CL108. `fec default` restores value 0, which is what carried these links for 16 h 51 m before anything wrote to them.

## [v0.3.6] — 2026-09-07

### Fixed
- **fix(fec): switch off the boot-time FEC self-heal (#7).** The step that pinned the ConnectX NV FEC override to RS and warm-reset when every 25G port read link-down no longer runs. Its first live firing is the R230's last: on 2026-09-07 both of C2NR0Q2's 25G ports (dsw1 `1/1/5` and `1/1/7`) dropped together at 16:22 UTC and never came back, after **16 h 51 m of continuous link** on a fabric already correctly set to `fec CL108-RS`. dsw1 was not involved — up 4 days, no login since the previous evening, FEC `CL108-RS` configured *and* operational on all 48 ports, uplinks forwarding throughout. The card stayed powered with one port lasing at −2.1 dBm without PCS lock and the other dark, which is a card-configuration state, not a fabric one. The trigger is the defect: `matched_all_down` samples `snp.media_present` **once, with no settle wait**, so a healthy card probed early enough in UEFI reads all-down — the exact trap `ensure_available` already documents and retries around, because a 25G RS link needs seconds after a reset and *time was the answer*. The write self-limits (skipped once FEC is already RS), which is why this cost one bad write and not a boot loop. There was nothing to fix regardless: the ConnectX-4 Lx device-default *negotiates* cl108-rs, so a correct fabric matches it with no card-side change. `mlxfec` keeps the write path and `apply(None)` still reports current/next-boot FEC per port at boot.

### 2026-09-06
- **docs:** the bring-up history's item 4 recorded `fec off` on all 48 dsw1
  SFP28 ports as the fix for the 25G link-down; that was the wrong conclusion
  and the change that later broke the fabric (ports dark for days, faults on
  the links that came up). Rewritten to name it as the breaking change and to
  point at the `#7` correction: switch ports on `fec CL108-RS`, card untouched.

### 2026-09-06
- **fix(tcp4): stop running our own DHCP on every NIC before the platform answers.** `connect_within` ran its own EFI_DHCP4 client on all interfaces (four failing rounds, "no client reached BOUND") before waiting for the platform DHCP that `request_dhcp` had already kicked off — which is the path that actually works. Swapped: wait for the platform lease first (the common case), then our own client only as a last resort when the platform produced nothing in the whole budget (STATIC policy or no EFI_DHCP4). Same fallback, no noisy failing rounds when the platform was about to answer.
- **feat(fec): self-heal when the 25G ConnectX links are down (#7).** If every recognised ConnectX 25G port is link-down after the stack binds (matched by the interface device-path PCI device/function against the card BDFs — never a 1G onboard NIC), stormbootx pins the card FEC override to RS and warm-resets once. Idempotent: skipped when FEC is already RS, so it resets at most once then falls through instead of looping; a machine with any live 25G link never reaches it. Read path stays the boot diagnostic.
- **RESOLVED (#7): the flaky 25G links were switch-side, not the card.** The Dell S5148F fabric was pinned to `fec off` while the ConnectX-4 Lx device-default negotiates cl108-rs; setting the switch to `CL108-RS` (a standalone, uppercase OS10 interface command) matched both ends and every SR link came up stable, including a port dead for days, with no shut/no-shut. Rolled to all 48 SFP28 ports. `mlxfec` is kept **read-only** (`apply(None)`) — a boot-time FEC diagnostic. The proven write path (`apply(Some(Fec::Rs))`) is retained but uncalled: the card negotiates RS on its own.
- **fix(fec): correct OperationTlv dword 1 bit layout (#7).** With the parked cap9 lock cleared, the MNVDA access-register ICMD came back status 4 (bad parameter). The OperationTlv dword 1 mirrored its fields — tlv_class at bit 24, method at 17, register_id at 0 — where packets_layout.h places tlv_class[0:8), method[8:15), register_id[16:32). Dword 0 (Type[27:32), len[16:27)) was already right; only dword 1 was wrong. Now `register_id<<16 | method<<8 | 1`.
- **fix(fec): clear the parked cap9 semaphore and take the window (#7).** The R230 boot showed the Mellanox UEFI driver parks cap9 for its whole lifetime (held value 0x3/0x7, vsec @ 0xc0 — a good read, not 0xffffffff), so mlxconfig-style access never gets in cooperatively. It parks the lock to keep tools out but does not use the address/data window itself (its path is the HCA command queue), so after waiting out a genuine holder, a parked ticket is cleared and retaken. Guarded to a plausible ticket, never a dead bus (0x0/0xffffffff). Still read-only (want=None): only the FEC is read.
- **fix(fec): report the stuck VSC semaphore value; drop the build-count stamp.** The FEC scan reached the card (confirmed ConnectX-4 Lx on the R230) but the VSC semaphore never read zero. The error now prints the held value and the VSC offset, to tell a driver-held semaphore (small non-zero ticket) from a bad read (0xffffffff). Version now increments per build (0.3.1) instead of a separate build counter.
- **build: monotonic build number in the banner.** The stamp was just the short SHA, which does not order — `7141486` vs `1a9e601` gives no clue which is newer. Prepend the git commit count (`b<n>-<sha>`), so every commit-then-build shows a higher `b<n>` and the console always says which build is newest.
- **fix(fec): open the PCI root bridge with GetProtocol, not exclusively (#7).** `mlxfec::apply` opened `EFI_PCI_ROOT_BRIDGE_IO` exclusively; an exclusive open makes UEFI `DisconnectController` every BY_DRIVER agent under the bridge — PciBusDxe and the NIC drivers with it — so running before `tcp4` tore down the network stack (both NICs down) and often failed outright when PciBusDxe would not detach mid-boot (empty FEC output, and the boot could not attach). Borrow the interface with `GetProtocol` instead; nothing is disconnected.
- **build: `--iso` output on `build-boot-agent.sh`.** The hand-built El Torito UEFI ISO the iDRAC mounts over NFS had no script — reconstructed from the working ISO (`esp.img` as the no-emul UEFI boot image, `.efi` and `stormboot.conf` also loose in the tree) and folded into the build script, reusing the same ESP as the raw `.img`. `--iso` writes `/build/images/stormbootx.iso`.
- **feat: wire `mlxfec` into the boot, read-only first (#7).** The FEC module was written but never compiled — declared `mod mlxfec` and called `mlxfec::apply(None)` right after identity, before the network is used. It names every ConnectX on the bus (`15b3:XXXX <model>`) and prints each port's current and next-boot FEC, so the exact silicon and its FEC state are on the console before any write is enabled. Nothing is written and no reset is issued in this mode; an unrecognised card is named and skipped. Placed before `tcp4` because the eventual write path warm-resets the card and that must happen before the link is needed.
- **feat: `tag = <id>` in `stormboot.conf` states the machine's identity (#9).** A stated tag wins over anything SMBIOS says. Discovery is a convenience for a machine nobody has told; one that has been told should not have its answer second-guessed by firmware. It is also the only way to bench-test a box as another host without touching the boot server, and the only way to name a board whose SMBIOS serial is a placeholder.
- **feat: identity falls through the SMBIOS structures that actually carry it (#7).** Type 1 (System) covers Dell, HPE, Lenovo and Cisco — the same field, named differently by each. Type 2 (Baseboard) is second because the ODM boards commonly leave the system serial as a placeholder and burn the real number into the baseboard; Type 3 (Chassis) catches the remainder. Placeholders are now rejected rather than used: `Default string`, `To be filled by O.E.M.`, `System Serial Number`, all-zero and the rest are shared by every board of a model, so a node claiming one boots as somebody else. The console line names which source answered, because a machine identified by one thing and later re-identified by another is a failure nobody connects to a hardware change unless it was on screen the day it worked.

### 2026-09-05
- **COMPLETE: full diskless boot on real hardware.** A PowerEdge R230 (service
  tag C2NR0Q2) booted stormcos end to end over 25 GbE NVMe/TCP: stormbootx read
  the service tag, claimed `boothost/C2NR0Q2`, attached the 4K golden from
  forge, published it as `EFI_BLOCK_IO`, and chain-loaded `\\EFI\\BOOT\\
  BOOTX64.EFI` — which is stormuefi 0.5.2, now finding all 4 pallets (kernel1/
  system1/kube1/data1), verifying the manifest, and starting Linux
  6.17.1-300.fc43. The mlx5 25G port came up and stormcos began bridging. Every
  fix in v0.3.0 plus the chain-load, strict-ESP, 4K image geometry (stormcos#31)
  and stormuefi GPT-LBA probe (stormuefi 0.5.2, stormcos#32) converged in one
  boot. stormbootx's job — get a machine from firmware to a booting OS image
  over the network, with nothing local — is proven.


### 2026-09-05
- **fix (blockio): chain-load, and boot only the attached disk.** The agent now
  loads `\\EFI\\BOOT\\BOOTX64.EFI` off the attached image's ESP and starts
  it, instead of publishing a block device and hoping the firmware boot manager
  picks it up (it does not — a disk that appears mid-boot-option is not in
  BootOrder, so the machine dropped to setup). Two parts: a vendor device-path
  node is installed on the published handle so EDK2's PartitionDxe will bind and
  parse the GPT (BlockIO alone is skipped on real firmware; OVMF was lenient and
  hid this), and the ESP is matched **strictly** by that vendor node — an early
  loose version booted a stale Windows install off a local SAS disk instead of
  the image, which a boot agent must never do.
- **milestone: the chain-load hands off to stormcos's own bootloader.** On the
  R230, stormbootx started `\\EFI\\BOOT\\BOOTX64.EFI` off the attached
  image and it was `stormuefi 0.5.1`, which ran and took over — the full path
  is proven: tag → claim → attach → publish → chain-load → stormcos's loader.
- **diagnostic (stormcos#31):** `stormcos-sno-10.22` is built for **512-byte
  sectors** but served on a **4096-byte-block namespace**, confirmed at every
  layer: the GPT header is at byte 512 (zeros at 4096), and each pallet
  superblock's `STORMPAL` magic sits at `LBA*512`, not `LBA*4096`. So stormuefi,
  reading the media at its 4096-byte block size, finds 0 pallets and exits. The
  image is intact; the geometry is wrong for how it is served. stormbootx read
  the 4096-byte block size from the namespace and published it faithfully — the
  fix is to compose the golden at 4096-byte geometry. Not a stormbootx bug.


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
