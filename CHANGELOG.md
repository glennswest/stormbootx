# Changelog

## [Unreleased]
<!-- New unreleased changes go here -->

### 2026-09-30
- **feat:** `auto`, and every doubt (a 404, an unreachable engine, an unknown intent), boots the local disk when one can boot: a whole, non-removable disk other than the boot media whose GPT has an ESP carrying `\EFI\BOOT\BOOTX64.EFI`, read with `esp.rs` and no network. A machine with nothing to boot claims as before; `install` always claims and `local` always falls through (#3, the owner's answer; also answers #31). Console: `local : …`. `intent::decide` holds the rule, with host tests.

### 2026-09-29
- **chore:** the rustnic media pins stormnic-ixgbe 0dd4267: SNP and a MAC device path on a child handle after bring-up and the DMA check (stormnic-ixgbe#4), so the firmware's MNP/IP4/TCP4 can bind to the Intel 10G. Built `--locked`; console label `media : rustnic ixgbe@0dd4267 mlx4@4c2d318`. The normal media is unchanged (#51). Goldens `golden-stormbootx-rustnic-6d88515819338a0e` and `golden-nic-drivers-56ea4782ef2a`.
- **chore:** the rustnic media pins stormnic-mlx4 v0.2.0 (4c2d318): SNP on a child handle per Ethernet port with a MAC device path (stormnic-mlx4#4); `Start` keeps the ConnectX-3 (~1.5 s per NIC + up to 5 s for link, was ~20 s per port), the #3 broadcast self-test no longer runs, and ExitBootServices stops its DMA. Built `--locked`; console label `media : rustnic ixgbe@2afd319 mlx4@4c2d318`. The normal media is unchanged (#50). Goldens `golden-stormbootx-rustnic-ab4e848a4dcfcaf3` and `golden-nic-drivers-10a25ce0a5a3`.
- **chore:** the rustnic media pins stormnic-ixgbe 2afd319 (RX/TX descriptor rings with DMA and a broadcast check frame in Start, stormnic-ixgbe#3; still no SNP), built `--locked`; its console label is `media : rustnic ixgbe@2afd319 mlx4@cef8dc5`. The normal media still carries no stormnic-ixgbe (#48). Goldens `golden-stormbootx-rustnic-7f260c307c5ee784` and `golden-nic-drivers-60d62cc3aee9`.
- **fix:** `tests/media-ovmf.sh` failed on any console over 20 lines: `grep | head` under `pipefail` exited on grep's broken pipe before checking a line (#49).
- **feat:** the rustnic media carries `stormnic-mlx4.efi` for the ConnectX-3, pinned at cef8dc5 (stormnic-mlx4#2 firmware bring-up, #3 Ethernet data path) and built `--locked`; the nic-drivers golden carries it as `stormnic-mlx4.efi.off`. Every Rust driver is checked to be PE subsystem 11 when built, and `STORMNIC-SOURCE.txt` has a line per driver. `STORMNIC_ON_MEDIA` is now a list (`ixgbe mlx4`). Console label: `media : rustnic ixgbe@9476135 mlx4@cef8dc5`. The normal media is unchanged (#34). Goldens `golden-stormbootx-rustnic-b4f33d9566d6127e` and `golden-nic-drivers-5b867545c91e`.
- **chore:** the rustnic media pins stormnic-ixgbe 9476135 (PHY and link code matched to its spec, stormnic-ixgbe#13; 25 device IDs), built `--locked`; its console label is `media : rustnic ixgbe@9476135` (#47). Golden `golden-stormbootx-rustnic-f82a05f6013ea469`.

## [v0.7.0] — 2026-09-29

### Added
- `blockio :` console lines while the bootloader reads the attached image: a failed read with its NVMe/TCP error, a read over 2 s, and progress every 64 MiB with MiB/s (#46).

### Fixed
- `esp =` is read at start-up with the rest of `stormboot.conf`, not after the attach, so nothing on the media is opened once an image is attached, as before #37 (#46).

### Documentation
- Work plan: #3 re-scoped by the owner's #19 decision (install only on request); the no-request case is waiting on the owner.
- Work plan: #45 closed; goldens `golden-stormbootx-8732577aa6bda0ce` and `golden-stormbootx-rustnic-0e449102ceb5882d` (v0.6.0).

## [v0.6.0] — 2026-09-29

### Added
- **a second media golden, `stormbootx-rustnic` (#45):** `deploy/build-golden.sh stormbootx-rustnic OUT` writes `bin/stormbootx.efi` and `boot/stormbootx-rustnic.iso`, which carries the Rust `stormnic-ixgbe.efi` and no iPXE NIC driver, built from the commit's pins. Registered in stormcentral as a `media` component.
- **`media = <label>` in `stormboot.conf`** (`build-boot-agent.sh --media`), printed as `media : …` under the banner. The goldens write `normal` and `rustnic ixgbe@<sha>`, so the console says which variant booted (#45).
- `tests/media-ovmf.sh ISO 'LINE' …`: boots a media ISO under OVMF and requires those lines on the console.

### Changed
- `STORMNIC_IXGBE_REF` → 884cf18 (stormnic-ixgbe#2's NIC bring-up in Start; committed `Cargo.lock`, so built `--locked`) (#43, #44).
- `build-nic-drivers.sh` fetches and builds no iPXE when no iPXE driver is wanted, and then writes no `IPXE-SOURCE.txt`.

### Documentation
- README (*How it ships*: the two media), CLAUDE.md (build, work plan); work plan: #37 handed off for the metal run.

## [v0.5.1] — 2026-09-29

### Fixed
- **deploy:** the `stormbootx` media golden failed to build from 12452f6 (#29) on: `build-nic-drivers.sh` fetched the private `glennswest/stormnic-ixgbe`, and the golden builder has no GitHub credentials (`could not read Username`). The media carries only `*.efi`, so the carried `stormnic-ixgbe.efi.off` was never on it. `build-golden.sh stormbootx` now passes `STORMNIC_CARRY=no`, and the ixgbe build is skipped unless `STORMNIC_ON_MEDIA=ixgbe`. `IPXE-SOURCE.txt` is still written. The `nic-drivers` golden still fetches it.

## [v0.5.0] — 2026-09-29

### Added
- **boot a 4096-byte-sector ESP on firmware whose FAT can't read it.** Volumes stay 4K. When the firmware's own FAT loads no `\EFI\BOOT\BOOTX64.EFI` from the attached image (AMI Aptio 4 mounts a 4K FAT and then answers `NOT_FOUND`, #33), stormbootx reads the file itself and `LoadImage`s it from the buffer, with the path `Vendor(disk)/HD(n,GPT,…)/\EFI\BOOT\BOOTX64.EFI`. `src/esp.rs` (core-only) handles a CRC-checked GPT and FAT12/16/32 at any sector size from 512 to 4096, with 8.3 and long names. `src/espboot.rs` does the load. `esp = firmware | stormbootx` in `stormboot.conf` picks one reader alone; the default is the firmware first.
- **`espprobe.efi`, a third binary.** It reports whether the firmware's FAT loads a disk's bootloader, and starts it through stormbootx's reader. `tests/esp-ovmf.sh` boots it under OVMF against a 4096-byte virtio disk with a 4096-byte-sector FAT16 ESP; it is now part of the sc-build command.
- `esp.rs` host tests (12) on `mkfs.fat`/mtools images: FAT12/16/32, 512- and 4096-byte sectors on 512- and 4096-byte disks, long names, a fragmented file, nested directories and `..`, GPT CRC failures, and at most a few reads per lookup.

### Fixed
- the ESP's GPT entry is judged only after the entry array passes its CRC (#38).

### Documentation
- README (step 9, `esp =`, `espprobe`, build), CLAUDE.md (build command, module map, a load-bearing fact, work plan).

### Also in this release: the changes since v0.4.0, by date

#### 2026-09-28
- **docs(#26):** closed on server1's run of `golden-stormbootx-74a242a6f0e89f75` (a709f9f): `ipxe-intelx.efi` loaded from the media, `tcp4 : available`, the name `server1` from the PTR, a claim of `boothost/server1` and an NVMe/TCP attach with no `CreateEvent` failure. The `BOOTX64.EFI` `NOT_FOUND` after the attach is #33. README, the deck and the work plan updated
- **feat:** `stormnic-ixgbe.efi`, the Rust driver for the blades' Intel 10G, is built into the NIC drivers from a pinned commit (`STORMNIC_IXGBE_REF`, glennswest/stormnic-ixgbe) (#29). Carried as `stormnic-ixgbe.efi.off`, which is not loaded — a scaffold that binds the NIC without a network would take the blades' network away; `STORMNIC_ON_MEDIA=ixgbe` puts it on the media in place of iPXE's intelx, for the on-hardware check (stormnic-ixgbe#7). `STORMNIC-SOURCE.txt` records the commit and digest
- **docs:** refreshed README, CLAUDE.md and the deck against the code (changes since 2026-09-18):
  - the intro and flow now start with the media's NIC drivers and the name order;
  - the drivers step is in the step list;
  - universal boot applies when there is no name the engine knows;
  - the placeholder rule is the code's own (only `0`, `.`, `-` and spaces);
  - sockets and timeouts include the health read and the 5 s PTR query;
  - `build-nic-drivers.sh` builds `intelx` from `glennswest/ipxe`, with hermon opt-in;
  - the DNS server can come from `dns =`;
  - minismbd serving from the golden is marked as planned;
  - the R230 console quote is the verbatim first-attach capture (ea26be1), with the chain-load stated separately, and the server1 run is added;
  - the open-issues table is current;
  - CLAUDE.md's module map has `main.rs` and an accurate `smbios.rs` entry, and the #26 plan no longer points at an ISO on dev.
- **docs(#13):** re-checked the presentation against the code at HEAD and rewrote the slides that had drifted: 152 KB, the four host suites, NIC drivers from the media, the NIC-name and shared-serial rules, `dns =`, the intent read down the name list, the engine's open intent read, shipping as goldens (#21), the server1 run, and the planned and status slides. The R230 console quote is now verbatim from that build.
- **feat(deploy): `deploy/build-golden.sh` writes the `stormbootx` and `nic-drivers` goldens' trees (#21).** The owner decided that everything is a golden. The script compiles (`--locked`) and writes one golden into the directory it's given, and does nothing else. The `stormbootx` golden holds both `.efi`s, the ISO, the USB image and a tcp4probe ISO, with the drivers on the media. `nic-drivers` holds the iPXE driver and its source note. Each golden has `SHA256SUMS` and a `BUILD` record.
- **chore(scripts):** `build-boot-agent.sh` and `build-nic-drivers.sh` now default to `tmp/` in the checkout instead of `/build/images`, since nothing may be left on the build box. `build-boot-agent.sh` now builds `--locked`.
- **docs:** README (*How it ships*), CLAUDE.md and the presentation say stormbootx ships as goldens.
- **chore:** iPXE is built from our own copy, `glennswest/ipxe` at the same pinned commit (629e28b), never from upstream (owner: "pull the drivers out, and create our own git repo")
- **fix(build-nic-drivers):** iPXE's `make` prints a line every 20 s while it runs. Its silence once ended an sc-build with the connection closed by the build box (#28).
- **docs(build-boot-agent):** the closing hint no longer says "move the DNS record"; portal discovery over DNS was removed on 2026-09-03, and which image boots is the boothost synonym.
- **fix(intent): read the boot intent down the claim's name list (#11).** It was read under one name: a DNS name the engine did not yet know (stormblock#204) gave a 404, read as `auto`, and hid a `local` set on the host the engine knows by the machine's MAC. It now asks the DNS name, the MAC (when claiming the default) and the serial in the claim's order, moving on only on a 404; a stated name is still the only one asked. A host test covers the engine's own reply shape (stormblock 0e3c47b, #148). README and `intent.rs` describe the landed route.
- **docs(#23):** work plan records 648e366's verification (sc-build: build and all four host suites) and that the issue's test now waits on server1 booting an ISO of 648e366 or later.
- **fix(identity): the name comes through on microdns networks, and is the machine's (#26).** The reply of a lease stormbootx ran itself is kept and read for the name, since firmware may not give it back (server1 found none). `dns =` in `stormboot.conf` (`build-boot-agent.sh --dns`) names the PTR server when the reply has none. Every way the name comes up empty is printed. A reservation's NIC name (`server1a`) maps to its machine (`server1`): one lowercase letter after a digit is dropped (`dnsname::machine_label`, host-tested).
- **fix(smbios): a serial shared by several machines is no identity (#26).** A Type 1 serial equal to the Type 3 serial on a multi-node chassis (multi-system, blade, blade enclosure) is skipped along with Type 3, and `S11075924402016`, which seven X9 blades report, is rejected outright.
- **fix(tcp4): never close an event the TCP driver can still signal (#26).** A token that timed out stayed queued, and its event was closed anyway; the connection's abort (at the latest in `Drop`) then signalled freed pool. EDK2 shrugs that off, AMI Aptio 4 did not: server1 failed the NVMe/TCP connect with `CreateEvent failed: INVALID_PARAMETER`. A timed-out token now aborts the connection (`Configure(NULL)`) before its event is closed. Events are created with notify TPL `CALLBACK` instead of `APPLICATION`, and a failing `CreateEvent` names the operation and the current TPL.
- **fix(drivers): a hanging driver names itself (#26).** Each step (platform bind, each driver's start, the final connect) is printed before it runs, with seconds elapsed. On server1, `ipxe-hermon.efi` hung the boot silently.
- **chore(drivers):** `build-nic-drivers.sh` builds `intelx` only by default; hermon is opt-in (`IPXE_DRIVERS="intelx hermon"`) until its hang on server1 is understood.

#### 2026-09-27
- **chore(smbios):** removed the unused `service_tag()`; `identity()` reads the Type 1 serial (then Type 2, Type 3), so the build is warning-free again (#18).
- **docs(#26):** iPXE `.efidrv` (EFI drivers only, no PXE) is the owner-approved interim; the long-term driver is a Rust crate written from the Intel datasheets (#27).
- **feat(drivers): NIC UEFI drivers from the boot media (#26).** Every `*.efi` in `\stormboot\drivers\` on the boot volume is loaded (`LoadImage` by device path) and started before TCP4 is looked for, after one `ConnectController` pass so the platform's own drivers claim their NICs first; then every handle is connected again so the new driver's SNP gets the platform's MNP/IP4/TCP4. The console prints `drivers : N of M started` and one line per file. For the Supermicro X9 blades, which have the stack but only legacy option ROMs on their Intel 10G and ConnectX-3. `scripts/build-nic-drivers.sh` builds iPXE's `intelx` and `hermon` as EFI drivers at a pinned commit (GPL-2, separate binaries, with a source note), and `build-boot-agent.sh --drivers DIR` lays them on the media. No directory, no change.
- **docs(#15):** re-checked against stormblock v19.4.0, the first release with #200. Its default-claim reply and request body match what `universal.rs` parses and sends, so no client change is needed. What is still open is a stormblock golden with #200, forge running it, and `boothost/default` being set.
- **feat(identity): a machine claims `boothost/<its DNS name>` (#23).** With no stated name, the name comes from DHCP option 12 on the lease of the interface that reached the engine (option 15 adds the domain for the console), else the PTR of that interface's address, asked of the option 6 DNS server over DNS/TCP. microdns answers over TCP, so no UDP stack is added. The engine knows hosts by the first label, so `server3.g10.lo` claims `boothost/server3`, and the console prints `name : server3.g10.lo (from DHCP)`. That claim carries `{mac, serial}`, and a 404 moves on to #15's default-by-MAC and then the serial. The health read now runs first on every claim path, because it brings the network up and says which interface to read. The intent is read under the same name, and the host NQN falls back to it. Parsing and the wire format are in `src/dnsname.rs`, `core`-only, with host tests against microdns replies captured on g8 and g10. `dhcp4::reply_for` reads the bound lease's reply from a fresh child's mode data, and `Tcp4Socket::interface` reports the NIC and address a connection used. stormblock#204 was filed so a name the engine has not seen reaches the host its MAC or serial already belongs to.
- **feat(config):** `name =` in `stormboot.conf` states the identity; `tag =` still works.
- **docs(#23):** README, CLAUDE.md (the "no DNS" fact now says no DNS *discovery*, plus a new fact on names), and `docs/presentation.md`.
- **docs(#11):** re-checked: the client side is complete (v0.4.0, and the MAC-keyed read from #15). Still waiting on stormblock#148, which is not on stormblock main. #148 now also asks for alias resolution (the MAC read) and a GET open without a token.
- **docs(#3):** re-checked: still blocked on the owner decision #19 and on stormcos#30. New constraint recorded on #19 and in the work plan: since stormblock v17, `GET boothost/<tag>` needs a token, so the clone-free compare also needs an engine change.
- **feat(claim): universal boot — one boot medium for every machine (#15, stormblock#200).** A machine whose media states no `tag =` claims `POST …/boothost/default/claim` with `{"mac": …, "serial": …}` and gets a copy-on-write clone of the default release of its own, as host `mac-<hex>` until it is named. The console prints a `mac` line and `booting the default image as mac-<hex>`, or `booting <name>'s image` once the MAC is an alias of a named host. The MAC is the lowest usable unicast permanent address of every NIC (`tcp4::machine_mac`), so bind order can't change it. Not the SMBIOS serial, because serials are not unique (seven MicroCloud nodes share one). **Gated on the engine:** only when the public `/api/v1/health` reports a version strictly after 19.3.0. Engines from v17 to 19.3.0 would read `default` as one shared tag and release each other's clones. Older engines, or an unreadable version, are claimed by serial as before. A #200 engine's 404 (no default to give) retries by serial, which can then only find an existing host. The host NQN is the engine's name for the machine. A board with no usable serial now falls back to its MAC instead of failing (#7's floor). The logic is in `src/universal.rs`, `core`-only with 6 host tests (version gate, MAC choice, the reply's `host` object in the engine's sorted-key shape). stormblock#202 was filed so a machine the engine already knows by serial keeps its host.
- **fix(build):** `tcp4probe` carries `universal.rs` (#24).
- **fix(claim):** `host.provisional` is read when it is the last key of the reply's `host` object.
- **docs(#15):** README, CLAUDE.md (a load-bearing fact replaces "never claim `boothost/default`"; the build command runs the third host suite), `docs/presentation.md` and `build-boot-agent.sh` describe universal boot.
- **docs(#13):** `docs/presentation.md`, a 13-slide Marp deck: what it is, where it sits (matching `stormcentral check`: kind `boot`, depends on stormblock), how it works, what it does today from the code, interfaces and `stormboot.conf`, how it ships (media, no golden, #21), the R230 hardware run, planned work marked as planned, and status. Linked from the README.
- **docs:** refresh from the code since 2026-09-18. The only code changes in that window are the boot-intent read (#11) and relaying the engine's 404 text (#15), and both were already reflected in the README, CLAUDE.md and module docs by the #12 audit. CLAUDE.md work-plan entries for #2 and #4 now carry the 2026-09-27 validation findings (priorities, stormipmi's Redfish inventory, no published `stormbootx.efi`). No new promise-without-code found: every not-yet-done item in the docs is tracked (#2, #3, #4, #7, #11, #15, stormblock#148).
- **docs(#12):** documentation re-audited against the code. README: the build section uses `sc-build` (it still showed a direct cargo build on dev); the build stamp is the short commit plus `-dirty`, not `b<n>-<sha>`; the intent read is counted among the sockets; a new *Ports, health and shipping* section (no listener, no health or metrics endpoint, outbound to the engine API and the NVMe/TCP portal, and shipped as boot media with no golden); the stormblock v17 token rule and what it means for the intent route; open-issue table and the stormnetboot link corrected. `main.rs` and `registry.rs` module docs name the engine rather than sbregistry. A stray doc line moved in `config.rs`. `smbios::identity` no longer claims a MAC fallback that `main.rs` never passes (#7 tracks it). Three comments that still described the removed DNS discovery (`main.rs`, `config.rs`, `tcp4::connect_within`) corrected. Binary sizes from the sc-build run: 117,248 and 35,328 bytes. CLAUDE.md now points at `sc-build` and `tmp/`.
- **chore:** `.gitignore` covers `tmp/`, logs and key/env files.
- **feat(claim): a machine with no `boothost/<tag>` boots the default image (#15).** The fallback is the engine's: since stormblock v17.0.0 a claim for an unassigned tag pins it to `boothost/default` and boots that. stormbootx keeps claiming its own tag, because claiming `default` directly would give every new machine one shared boot clone name. The console now relays the engine's 404 text, which says whether `boothost/default` is missing too, instead of a fixed "no synonym" line.
- **docs:** README explains the default image. CLAUDE.md records the rule never to claim `boothost/default` directly.
- **docs(#15):** work plan moves #15 to blocked. The client side builds and passes under sc-build, but forge runs stormblock 13.7.0 with no `boothost/default`, so the fallback can't be served until forge is on v17 or later and stormcentral#29 sets the default.

## [v0.4.0] — 2026-09-27

### Added
- **feat(boot): a per-machine boot intent, read before the claim (#11).** `GET /api/v1/synonyms/boothost/<tag>/intent`, the contract proposed on stormblock#148. `local` falls through to the local disk at once, with no claim and so no clone minted on forge. `install` and `auto` claim and boot as before. **Any doubt reads as `auto`** (404, non-2xx, unreachable engine, no intent or an unknown one), so a failed read can't keep a machine off an install it was asked for. The console prints the intent and why it defaulted. The engine doesn't serve the route yet, so until stormblock#148 lands every boot is `auto`: unchanged behaviour. The parse-and-decide logic is `src/intent.rs`, `core`-only like `sha256.rs`, with 7 host tests run by `rustc --test`.
- **feat(ops): `scripts/fec-cycle.sh`** — the recovery, scripted. Walks the four FEC stages with a shut/no-shut each, surfaces anything clish rejects instead of swallowing it, reads link state between stages over the passwordless login, and always ends on `CL108-RS`. Never leaves a port on `off`: no FEC on 25GBASE-SR is out of spec and is what broke the fabric on 2026-09-06.
- **feat(ops): `scripts/fec-watchdog.sh`** — the recovery on a timer, because the switch-side fix is not coming: the S5148F is XPliant-based, its last OS10 build is 10.4.3.8, and upgrading is off the table. Conservative by construction: a port must read latched on **two consecutive passes** before anything is written (one sample is what the 0.3.4 self-heal believed, and it cost a card a bad NV write), a cooldown bounds a port the cycle cannot fix to one attempt per window instead of a config write per cron tick, and a powered-off peer cannot trigger it at all because the detector requires real light. Logs whether each port recovered or still needs a human.
- **feat(ops): `scripts/port-latched.sh`** — tells the latch apart from the three faults it imitates. The signature is `admin up + optic present + real light + oper down`; a genuinely dark peer reads the `−40.0` floor and an empty cage produces no media record. Read-only, no password, exits 1 when a port is latched so it can gate the recovery from cron.

### Fixed
- **fix(boot): the no-local-disk fall-through no longer says "could not reach a portal".** It is now also reached by a `local` intent, so it says "nothing booted from the network".
- **fix(ops): the S5148F latches a 25G port when the peer powers off, and only a FEC transition clears it.** C2NR0Q2's `1/1/5` and `1/1/7` dropped together at 01:33:32 UTC after six days of clean traffic (63.4M packets in, 1 CRC) and stayed down through four host power cycles, a host bounce and a switch-side `shutdown`/`no shutdown`. Nothing had changed on either end: no admin login on dsw1 since 2026-09-08 12:52, no SEL entry on the host since 09-08 00:31, card NV FEC `device-default` on all four port entries, firmware `14.32.10.10`, boot ISO 0.3.8 with no `fec =` line and the self-heal off. The switch received healthy light throughout (`+0.64` and `−1.53 dBm`) and never achieved PCS lock. Walking the port through `no fec` → `CL91-RS` → `CL74-FC` → `CL108-RS` brought both up at 25G **on the final stage — the value they had been configured with all along**, so the FEC value was never wrong; the transition is what re-programs the serdes. Second occurrence of the signature (see 2026-09-07), and it reproduces on every peer power-off.

### Changed
- **refactor(ops): the switch-side scripts move to their own repo, `dswfecfix`.** `scripts/fec-cycle.sh`, `scripts/port-latched.sh|.py` and `scripts/fec-watchdog.sh` are removed here and superseded by a static Rust binary that runs **on dsw1 itself** under cron, covering all 48 ports with a bounce → FEC cycle → autoneg-toggle ladder and a backoff that never gives up. It needs no password: `cpsusers` can write `fec`, `enabled` and `auto-negotiation` through CPS directly. This is a switch defect, not a stormbootx one, and two implementations of the same recovery would be exactly the second place for the answer to live that this project's own history warns about. Recoverable from history; the shell versions worked and are what proved the mechanism.

### Documentation
- **docs:** README documents the intent step. CLAUDE.md's Build section uses `sc-build` instead of `ssh root@dev.g8.lo` and names the full build-and-test command. The module map gains `intent.rs`, and the work plan notes that stormblock#123 is closed.
- **docs: module comments in `main.rs` and `blockio.rs` said "firmware boots it".** They predated the chain-load (2026-09-05). They now say the agent loads the attached ESP's `BOOTX64.EFI` itself and why. Comment-only change.
- **docs: README rewritten from the code.** Step-by-step as `run()` executes it; the exact SMBIOS placeholder list; the three-phase socket open (existing address → wait for platform DHCP → own `EFI_DHCP4` client) with its 30 s budget; the NVMe/TCP specifics (no PDU digests, MDTS formula, 64 KiB/512 KiB bounds); the full `stormboot.conf` key table with compiled defaults; the failure console's commands; what the agent writes at boot, including that it switches any `STATIC` IP4 policy to `DHCP`, which the old README never mentioned; and what is in the code but inactive (`USE_REGISTRY`, the self-update helpers, the switched-off FEC self-heal). Corrects this morning's edit, which said `--iso` writes an ISO *as well as* the `.img` (it is instead). Packaging (`build-boot-agent.sh`) is now a short subsection under Build rather than a section of its own.
- **docs: README and CLAUDE.md caught up with the code.** The README still said "not yet run on hardware" and described the boot as "publish BlockIO and the firmware boots it"; both have been wrong since 2026-09-05. Now documented: the two-stage chain (stormbootx attaches, the image's `BOOTX64.EFI` — stormuefi on stormcos — selects and boots a pallet; a locally installed node runs stormuefi alone), why chain-load and the strict vendor-node ESP match, identity via SMBIOS types 1→2→3 with placeholder rejection and the `tag =` override, multi-NIC ranking and the DHCP fallback, Proxmox's OVMF as the working emulator, the fall-through console, `--iso`/`--fec` and the `tag`/`fec` config keys, the real binary size (~110 KB, not 57 KB), and the module map (`dhcp4`, `shell`, `mlxfec`, `sha256`). CLAUDE.md notes that the FEC work's `#7` references do not match GitHub #7 (boot identity), and lists #8–#11. stormboot4bios is named as the planned legacy-BIOS counterpart.
- **docs(#3):** work plan records why the local-disk skip is still blocked — stormblock#123 (installed disks have no ESP to boot), stormcos#30 (no installed marker), and the compare key, an owner decision. Notes that `GET /synonyms/boothost/<tag>` names the intended golden without minting a clone, and that #11's rule (new golden **and** requested) supersedes "different → reinstall".

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
