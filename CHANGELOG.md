# Changelog

## [Unreleased]
<!-- New unreleased changes go here -->

### 2026-10-08
- **refactor:** removed the old sbregistry `/v1/clones/claim` path (#17): `registry::claim`/`registry::existing` and `main.rs`'s `USE_REGISTRY`, `REGISTRY_IP`/`PORT`/`HOST` and `GOLDEN`. It was compiled out since the boothost claim replaced it, sent no credential, and spoke plain HTTP to a node registry moving to TLS with auth (stormcos#355). No behaviour changes: every boot already claimed the engine's `boothost/<name>`.
- **docs:** CLAUDE.md work plan: #25 closed (sc-build of 3be647a passing).
- **fix:** the FEC report knows a ConnectX-3 (#25). A ConnectX-3 or -3 Pro (15b3:1003/1007, the X9 blades' 10/40G card) was "not a ConnectX physical function this code knows", followed by "no ConnectX on the bus"; it now says `ConnectX-3 (15b3:1003): no FEC to report (10/40G, no RS-FEC)`, and "no ConnectX on the bus" is printed only when no Mellanox function was seen. New core-only `src/connectx.rs` (the id tables, from `mlxfec::KNOWN`, with host tests).
- **docs:** CLAUDE.md work plan: #4 closed (sc-build of 80dc3a8 passing).
- **feat:** every claim carries the firmware inventory (#4, #20): each NIC's firmware MAC, link and the driver that bound it (`media_driver` for a stormnic one off the media), PCI location and ID; every mass-storage controller (PCI class 01) and its driver; every whole BlockIO disk, the boot medium marked; and, with no BMC (no SMBIOS Type 38/42), CPU and memory from SMBIOS Types 4 and 17. Sent as `"inventory"` beside `"agent"`; stormblock#177 keeps it in the host's last-claim record. Collected once, before anything is attached; the console says `inventory : …`. New `src/inventory.rs` (core-only, host-tested: SMBIOS parse, JSON, cut to 15 KiB under the engine's 16 KiB) and `src/hardware.rs`; `smbios::table_bytes`, `drivers::pci_info`/`pci_functions`, `snpwatch::pci_driver`, `net::nic_summary`, `blockio::is_published`. `tests/net-ovmf.sh` requires it in the claim, and its stub checks it is what the engine keeps.
- **docs:** CLAUDE.md work plan: #10 closed (sc-build of e5407fa passing; no read-throughput change).
- **refactor:** the NVMe/TCP initiator is its own crate, `crates/nvme-tcp-initiator` (#10), for stormboot4bios to link instead of a third copy: `no_std` + `alloc`, no dependencies, generic over a `Transport` (`send_all`, `recv_exact`, `link_mtu`) and a `Platform` (`connect`, `stall_ms`). The wire code is `src/nvme.rs`'s, moved unchanged; `src/nvme.rs` is now the UEFI end (`net::TcpSocket`, which gains `read_into`, and `boot::stall`). The repository is a Cargo workspace, and `cargo test -p nvme-tcp-initiator` (in the sc-build command) runs host tests against an in-memory controller that refuses a command without PSDT=01b and an admin command before CC.EN.
- **docs:** CLAUDE.md work plan: #42 closed (sc-build of 9d6554e passing); #124 filed for net-ovmf's intermittent `nolease` PEI hang.
- **feat:** a bootloader the bridge starts can open the other files on its ESP (#42). `src/espfs.rs` is a read-only `EFI_SIMPLE_FILE_SYSTEM`/`EFI_FILE_PROTOCOL` over `esp.rs`: Open (absolute and relative), Read (files at any position, directory listings with `.` and `..`, `BUFFER_TOO_SMALL` without losing an entry), Get/SetPosition, GetInfo (`FileInfo`, `FileSystemInfo`, the volume label); Write, SetInfo and opens for writing answer `WRITE_PROTECTED`, Delete warns. `blockio::bridge_boot` installs it on the ESP's partition handle after disconnecting the firmware's FAT (or on a new handle with that path) and loads `BOOTX64.EFI` under it, so it is the image's `DeviceHandle`; `blockio::withdraw` takes it back first. `esp.rs` gains `open_dir`/`next_entry`, `read_at` with a cluster hint, `lookup_in`, entry attributes and times and the volume label, with host tests on every FAT variant. espprobe installs it before starting the payload, and tcp4probe reports what its own boot volume serves (`boot fs :` lines); `tests/esp-ovmf.sh` requires them, and `tests/net-ovmf.sh`'s new `bridge` boot (`build-boot-agent.sh --esp stormbootx`) requires the same of the NVMe-attached image. net-ovmf gains `NET_ONLY=<boot>`, says how each boot ended, and prints a failing boot's serial console.
- **docs:** CLAUDE.md work plan: #53 closed (sc-build of 123528a passing).
- **fix:** `stormbootx.efi` is the same bytes wherever it is built (#53). One commit gave two digests in two goldens: lld-link wrote the link time into the PE `TimeDateStamp` and a PDB-derived CodeView GUID into a debug record, and a panic string carried the `uefi` crate's source path under `CARGO_HOME`. New `build.rs` links every UEFI binary with `/Brepro` and `/DEBUG:NONE`; new `scripts/cargo-repro.sh` remaps the checkout, target dir and `CARGO_HOME` (`--remap-path-prefix`), and `deploy/build-golden.sh` and `build-boot-agent.sh` build through it. New `tests/repro.sh` builds from two paths with two `CARGO_HOME`s and requires identical bytes; it is in the sc-build command.
- **docs:** CLAUDE.md work plan: #36 closed (sc-build of b2f6517 passing).
- **fix:** the golden media name no fallback namespace (#36). `fallback = none` in `stormboot.conf` (`build-boot-agent.sh --no-fallback`) makes a boot that no claim gives an image fall straight through to the local disk, with nothing attached, and say so (`fallback : none …`, `no network boot: no claim gave this machine an image, and the media names no fallback`). Every golden writes it, and `build-golden.sh` refuses a media tree that names `nqn`/`nsid`. Before this, every golden carried the script's default `nqn.2026-09.lo.g16:stormcos` nsid 2, a 32 MB namespace with no ESP on forge, so every unnamed boot attached it and fell through anyway. With no attach coming, a medium on self-update trial counts the engine's answer as good. Media without the key keep the old fallback. `tests/net-ovmf.sh`'s `release` boot is built that way.
- **docs:** CLAUDE.md work plan: #80/#102 closed; v0.24.0 goldens.
- **docs:** CLAUDE.md work plan: #67 closed (sc-build of 71ed9fb passing).
- **fix:** `esp.rs` reads a FAT32-labelled ESP whose cluster count says FAT16 as FAT32, as Linux does (`BPB_FATSz16 == 0` decides). pvetest1's 11.53 ESP (`mkfs.fat -F 32`, 16,384 clusters at 4096-byte sectors) was refused as `a FAT12/16 with no root directory`, so the #37 bridge was stricter than Linux. New host test `fat32_with_a_fat16_cluster_count` (#67).

## [v0.24.0] — 2026-10-08

### Added
- `nic_verbose = true` in `stormboot.conf` (`build-boot-agent.sh --nic-verbose`) turns on every stormnic driver's full bring-up trace (#80, #102). stormbootx sets the volatile `StormnicVerbose` variable (stormnic GUID `ce1479a2-…`, `BOOTSERVICE_ACCESS`, `01`) before it loads `\stormboot\drivers`, and says so. One already set from the EFI shell is left alone and named. No golden sets it. The mlx4 speed/autoneg keys the issue also asked for are dropped: the blades' ConnectX-3 firmware can't force a speed (stormnic-mlx4#20). `tests/net-ovmf.sh`'s `virtio-verbose` boot requires the driver's trace with the key; the quiet `virtio` boot must not print it.

### Fixed
- a refused install-config chunk now says the firmware's volatile variable store is full, how many bytes fit and the size this firmware takes (`install cfg : NOT HANDED DOWN: …`), so a node that booted without its config is visible (#93).

### Changed
- under OVMF every install-config size up to storminstall's 256 KiB cap hands down and reassembles (64/128/192/256 KiB; 342 chunks at 256), so `tests/net-ovmf.sh` now requires it (#93).
- `tests/net-ovmf.sh` hands down 64, 128, 192 and 256 KiB `install-config.yaml` files and reports which ones OVMF's volatile variable store takes; a refused one must be handed down not at all (#93).

### Documentation
- CLAUDE.md: render the deck with `< /dev/null`; on build VMs marp-cli read the piped stdin as a second markdown (#112).
- `StormBootClock` has a reader: stormblock's initramfs skips its own NTP step after `synced` (stormblock#253); README, CLAUDE.md, `clock.rs` and `handoff.rs` said stormcos#213. `docs/presentation.md`'s relationships slide says `group: boot` (the project's group, as `stormcentral check` prints it), and names the component kinds (`media`, `tree`) (#82).
- CLAUDE.md work plan: #89's metal run waits on #92 (a promotion newer than the stick); #92 and stormcentral#459 told to promote v0.23.1.
- README: the variable store is the install-config's real limit; OVMF's takes 256 KiB, and what a full store prints (#93).
- CLAUDE.md work plan: #96 closed; v0.23.1 goldens.
- CLAUDE.md work plan: #69, #70, #71 and #73 wait on their paused stormnic repos' first issues (P3, owner 2026-10-07); #72 (ice) waits on #71.

## [v0.23.1] — 2026-10-08

### Fixed
- the self-update never takes an older release than the running binary (#96). A signed manifest whose `version` is older (semver, `-pre`/`+build` ignored) than `CARGO_PKG_VERSION` is declined with `update : current (running vX; serial N offers vY, which is older); not taken`, and its serial becomes the medium's floor (`min`), so the next promotion is still judged. Before, the serial alone decided, and a v0.15.0 stick would have rewritten itself to stormcentral's serial 1 = v0.14.0 on its first boot. `downgrade true` in the manifest allows a deliberate rollback. `manifest::{version_ok, semver}` and the `downgrade` key, with host tests. `tests/update-boots.sh` boots stormcentral's real serial-1 manifest twice: declined as older, then "declined here before". Test releases now carry the running version with a `-test` suffix.

### Documentation
- CLAUDE.md work plan: #107 is the master's blade boot (v0.23.0's rustnic golden named on the issue).
- CLAUDE.md work plan: v0.23.0 goldens (#108).

## [v0.23.0] — 2026-10-07

### Added
- stormnic-virtio on the rustnic media, taking VMs' virtio-net NICs from OVMF's VirtioNetDxe (#108, #106, #75). `build-nic-drivers.sh` pins stormnic-virtio 1a04808 (v0.1.0) in the default `STORMNIC_DRIVERS`, so the nic-drivers and rustnic goldens carry `stormnic-virtio.efi` (label `virtio@1a04808`). New `stormboot.conf` key `prefer_media_drivers = virtio` (`build-boot-agent.sh --prefer-media-drivers`, set on the rustnic media only). After the media drivers start, `drivers::take_over` disconnects the firmware's driver tree from each virtio-net function (1af4:1041, 1af4:1000), leaf first, in up to three passes, judged by the `BY_DRIVER` opens left on its PciIo. It then connects the function naming only stormnic-virtio, and gives it back to the firmware's driver if ours does not bind. One console line per NIC. Without the key the platform's drivers still win. `tests/net-ovmf.sh` boots three more times with `VIRTIO_EFI` set: takeover of a transitional and a modern-only NIC with the claim and attach over stormnic-virtio, and the firmware keeping the NIC without the key.

### Changed
- `tests/media-ovmf.sh` takes `NIC=virtio` (one virtio-net NIC; otherwise none, as before), so the rustnic golden's own ISO shows stormnic-virtio taking the NIC (#108, #109: the check ran with no NIC, so there was nothing to take).
- rustnic media pins stormnic-mlx4 v0.2.5 (04e7d2c) (#104; asked for in the issue's comment, superseding v0.2.4 32083cd): per-port module EEPROM, PTYS and forced-speed diagnostics (stormnic-mlx4#17), and a quiet console with the full trace behind `StormnicVerbose` (stormnic-mlx4#16). Media label `rustnic ixgbe@728b328 mlx4@04e7d2c`.
- rustnic media pins stormnic-ixgbe 728b328 (#101; asked for in the issue's last comment, superseding 38d6f0c, ed9b719, b240082 and 32c0bfc): a quiet console, one line per NIC with link-down diagnostics (stormnic-ixgbe#22, #26), the X540/X552/X550/X553 simulation-only warnings (#17, #23), and the DMA check's 3 s receive listen only when verbose (#24). Media label `rustnic ixgbe@728b328 mlx4@0e50017`.
- `tests/net-ovmf.sh` boots a machine the engine has never seen (#15): the default claim by MAC answers the provisional host `mac-525400123456`, which must be attached, booted and handed down as `StormBootTag`/the host NQN; the stub must receive the claim carrying the MAC.
- `tests/net-ovmf.sh` checks the boot intent (#11): the stub engine answers stormblock's `intent_body` for the MAC alias. A boot told `local` must print `intent : local` and fall through with `nothing claimed`, with no claim POST and nothing attached. The `jitter` boot is told `install` and claims as before.

### Documentation
- CLAUDE.md work plan: #107 waits on the owner (who swaps the blades' rustnic ISO on minismbd and boots a blade).
- CLAUDE.md work plan: #98 closed, not reproduced (11.80 on stormbootx v0.15.0 reached Ready on pvetest2); the initramfs console after `ublk devices starting` is stormblock#321.
- CLAUDE.md work plan: #104 closed; goldens `golden-stormbootx-rustnic-0585a52012dcd66e`, `golden-nic-drivers-98976cdcd7eb`.
- CLAUDE.md work plan: #101 closed; goldens `golden-stormbootx-rustnic-5673f944f5dab62d`, `golden-nic-drivers-a5962e89b0f7`.
- #33 closed. README: the X9's NOT_FOUND was the release's ESP (stormcos#188), and the ttyS1 kernel console is stormcos#220 / stormuefi#23. The open-issue table drops #33, #11 and #15. CLAUDE.md work plan entry for #33.
- the binary's size is ~320 KB (328,704 bytes at dc09321) in CLAUDE.md and the deck's title slide (was ~230/~270 KB) (#95).
- CLAUDE.md work plan: #14 waits on stormcentral#133 alone, not a decision (#22 answered; KVM via `privileged`; no TCP4 needed since #56) (#95). `docs/presentation.md`: #11, #15 and #68 out of *Planned*, #14 waits on stormcentral#133, #22 no longer an open decision, status at v0.22.0.
- CLAUDE.md work plan: #15 closed (default claim of a new machine verified under OVMF; forge still 13.7.0).
- CLAUDE.md work plan: #11 closed (OVMF `local`/`install` boots verified; forge still 13.7.0).
- CLAUDE.md work plan: v0.22.0 goldens (#41).

## [v0.22.0] — 2026-10-06

### Added
- The USB sticks are their own goldens (#41, owner on #35; #97): `deploy/build-golden.sh stormbootx-disk` and `stormbootx-rustnic-disk` write only `boot/<golden>.img` (+ `SHA256SUMS`, `BUILD`), a GPT disk with a 64 MiB FAT16 ESP at 512-byte sectors carrying the family's medium, `update =` naming the family's boothelper. `tests/media-ovmf.sh` boots an `.img` as a USB stick.

### Documentation
- CLAUDE.md work plan: v0.21.1 goldens (#7).
- CLAUDE.md work plan: #41 closed; registering the disk goldens is stormcentral#190.

## [v0.21.1] — 2026-10-06

### Fixed
- SMBIOS serials refuse the same placeholders as stormipmi (#7, stormipmi#14): `0123456789`, `123456789`, `na`, `empty`, `serial number`, `chassis serial number`, `base board serial number`, NUL padding, and one filler repeated (`FFFFFFFF`, `XXXXXXXX`, `****`, 0xFF bytes). The check moved to `universal::serial_usable` (core-only), host-tested against stormipmi's vectors. The MAC floor's console source says `lowest NIC MAC`, which is what it is (was `first NIC MAC`).

### Documentation
- CLAUDE.md work plan: #70 (stormnic-e1000e) waits on the owner for the repo; QEMU `-device e1000e` verifies it under OVMF.
- CLAUDE.md work plan: #73 (stormnic-mlx5) waits on the owner for the repo; the R230 is the test card once #106 lands.
- CLAUDE.md work plan: #69 (stormnic-igb) waits on the owner for the repo; QEMU `-device igb` verifies it under OVMF.
- CLAUDE.md work plan: #71 (stormnic-i40e) waits on the owner: the repo, and a machine with an i40e NIC to verify on.
- CLAUDE.md work plan: v0.21.0 goldens (#68).
- CLAUDE.md work plan: #7 closed.

## [v0.21.0] — 2026-10-06

### Added
- `net::release` says on the console what it gave back on the fall-through (#68): `net : N of M NIC(s) given back to the firmware (exclusive SNP closed, reconnected)`, and `nic N: SNP not closed (…)` for any it could not. Until now a metal console could not show it.

### Tests
- `tests/net-ovmf.sh` fifth boot, `release` (#68): the firmware's own IPv4 stack on (plus a virtio-rng), stormbootx falls through (claim 404, dead NVMe port), gives the NIC back, and the firmware's PXE on that NIC leases from slirp and starts the payload over TFTP.

### Documentation
- CLAUDE.md work plan: #74 (stormnic-realtek) waits on the owner: the repo, and a machine with a Realtek NIC to verify on.
- CLAUDE.md work plan: #90 closed, v0.20.0 goldens.
- CLAUDE.md work plan: #68 closed.

## [v0.20.0] — 2026-10-06

### Added
- Every claim names the boot agent (#90, stormcentral#286): the claim body carries `"agent":{"name":"stormbootx","version","commit","media","update_serial","update"}` beside `mac`/`serial` (build stamp, `media =` label, the medium's self-update serial, `StormBootUpdate` at claim time). The engine ignores it until stormblock#177 records it. `tests/net-ovmf.sh` and `tests/update-boots.sh` check the body the stub engine received.

### Tests
- `tests/update-boots.sh` covers the self-update's unseen paths (#89): the medium is a USB stick (`qemu-xhci` + `usb-storage`; `MEDIUM=virtio` for the old bus), stormcentral's real serial-1 manifest and hex signature (`tests/fixtures/`) verify against the release key, a manifest for another machine's canary MAC is refused and one naming this MAC is taken, a driver the release drops is retired to `.prev`, and a release larger than the ESP is refused for room with nothing written. Ten boots, was six.

### Documentation
- CLAUDE.md work plan: #89, OVMF half verified (sc-build of 46048c0); the metal half waits on #96.
- CLAUDE.md work plan: #94 closed, v0.19.0 goldens.

## [v0.19.0] — 2026-10-06

### Added
- Every medium names the stormbootx on it (#94, storminstall#10): `build-boot-agent.sh` appends `version = <Cargo.toml version>` and `commit = <the binary's STORMBOOTX_BUILD stamp, or unstamped>` to `\stormboot\stormboot.conf` (ESP, ISO9660 tree, golden `media/`). It dies if the `.efi` it lays down does not carry that version. stormbootx ignores both keys. `tests/iso-layout.sh` checks them.

### Documentation
- CLAUDE.md work plan: #88 closed, v0.18.0 goldens.
- CLAUDE.md work plan: #75 (stormnic-virtio) waits on the owner: who creates the repo and its project.

## [v0.18.0] — 2026-10-06

### Added
- Waiting for the network is never silent (#88, from #78). While no NIC holds a lease, a connection prints `waiting for a lease (N s): nic 0 link UP, DHCP n out n in, n frames in; …` once a second (the shell's `state`/`dhcp` show the same counts). Each NIC's driver is named before its first SNP call (`nic N: driver <ComponentName | image file>`): the agent that opened the parent `BY_CHILD_CONTROLLER` for a MAC child, or PCI I/O / NII `BY_DRIVER` on the handle, never a consumer stacked on it (dac1712). `src/snpwatch.rs` marks every SNP call, and a 1 s periodic timer event at `TPL_NOTIFY` prints `nic N: SNP.<call> (<driver>) has not returned after N s` while one is stuck (again every 30 s; `returned after N s` if it comes back); `net::release` closes it. `tests/net-ovmf.sh` has a fourth boot, its only NIC on a dead QEMU hub.
- The rustnic media and the nic-drivers golden pin stormnic-mlx4 v0.2.3 (0e50017, #66): link diagnostics (speed, autoneg, module) at the link wait and on link changes (stormnic-mlx4#15), and a log line for the first own frame looped back (stormnic-mlx4#21). Console label `media : rustnic ixgbe@563ea8d mlx4@0e50017`; the fw medium is unchanged.

### Documentation
- CLAUDE.md work plan: #92 blocked on stormcentral#459 (no hardware test machine boots stormbootx ≥ 0.15.0, so nothing can be promoted yet).
- CLAUDE.md work plan: #91 closed, v0.17.0 goldens; README and the presentation drop #27 (closed).

## [v0.17.0] — 2026-10-06

### Breaking
- iPXE is retired from every golden (#91, owner on #81): `scripts/build-nic-drivers.sh` no longer fetches or builds iPXE (`IPXE_REF`, `IPXE_DRIVERS` and `IPXE-SOURCE.txt` are gone) and builds only the Rust stormnic drivers, as loadable `.efi` (no more `.efi.off`); `STORMNIC_ON_MEDIA`/`STORMNIC_CARRY` are replaced by `STORMNIC_DRIVERS` (default `ixgbe mlx4`). The `nic-drivers` golden holds `bin/stormnic-ixgbe.efi`, `bin/stormnic-mlx4.efi` and `STORMNIC-SOURCE.txt`.

### Documentation
- CLAUDE.md work plan: #52 closed, v0.16.0 goldens.
- README (*NIC drivers*, *How it ships*: the misplaced `nic-drivers` row put back in its table), `drivers.rs`, CLAUDE.md and the presentation say no medium or golden carries iPXE.

## [v0.16.0] — 2026-10-06

### Added
- Media with no NIC drivers say so: `drivers : none on the media; the firmware's own NIC drivers` (#52).

### Changed
- The `stormbootx` golden is the firmware-drivers medium (#52, owner on #81): no `\stormboot\drivers`, no iPXE, `media : fw`; a `--drivers` given to `build-golden.sh stormbootx` is ignored, and the component no longer takes the `nic-drivers` input. Machines whose firmware has no driver for their NICs (the X9 blades) boot `stormbootx-rustnic`.

### Documentation
- README *How it ships*, firmware requirements and the presentation describe the fw medium (#52).
- CLAUDE.md work plan: #54 closed, v0.15.1 goldens; #52's plan.

## [v0.15.1] — 2026-10-06

### Fixed
- The fall-through takes the attached disk back before returning to the firmware (#54): `blockio::withdraw` disconnects and uninstalls the published BlockIO and device path and drops the NVMe/TCP connection. An attach that booted nothing left them installed in an image the firmware then unloaded, so the boot manager probed a disk whose functions were freed: pvetest1's #UD at RIP 0x47FFFFFCA. It also counted that disk as a local one (`2 found` on a VM with one disk). A refused uninstall resets the machine instead of returning.

### Added
- A start names a disk an earlier start published and left installed (`blockio : N handle(s) an earlier start published are still installed`).

### Testing
- `tests/net-ovmf.sh` boots a third time (#54): a blank namespace from a second NVMe stub over a blank local virtio-scsi disk. It requires `blockio : withdrawn`, `(1 found)`, no stale disk when the boot manager starts stormbootx again, and no CPU exception in the 25 s after. On the old code it failed on the stale disk and `(2 found)`.

### Documentation
- README *When it fails*: step 0, the withdraw; CLAUDE.md work plan for #54; CLAUDE.md work plan: #79 closed, v0.15.0 goldens.

## [v0.15.0] — 2026-10-03

### Added
- `install-config.yaml` from the boot media to the node (#79, stormcos#82): `\stormboot\install-config.yaml` on the media's ESP (storminstall's slot, at most 256 KiB) is handed to Linux in volatile EFI variables under #76's GUID. `StormBootInstallConfig0..N-1` carry 768 bytes each; `StormBootInstallConfig` = `v1:<length>:<N>:<sha256>` is set last, so a header means the whole file. The console names size and digest, never the content. `src/installconf.rs` (core-only, the eighth host suite); `config::read_bytes`; tcp4probe reassembles and checks it. The initramfs's copy to `/state` is stormblock#275.

### Testing
- `tests/net-ovmf.sh` writes a 2 KiB `install-config.yaml` into the ISO's ESP as storminstall does (the isohybrid MBR's 0xEF volume, mtools) and requires it handed down in three chunks with a matching digest; the boot without one must hand down nothing.

### Documentation
- README *install-config.yaml* (the slot, the variables, the reader's rules); sc-build command, module map, CLAUDE.md work plan for #79; CLAUDE.md work plan: #86 closed, v0.14.0 goldens.

## [v0.14.0] — 2026-10-02

### Added
- stormcentral's release key is compiled in (#86, stormcentral#279): `RELEASE_KEYS` holds `4fe28c02…0a171bce`, the Ed25519 public key stormcentral serves at `/api/v1/stormbootx/keys`, so a writable medium with `update =` now checks and takes a release stormcentral promotes. The keys moved to `manifest.rs`, where a host test holds them to the published hex (11 manifest tests).

### Fixed
- `tests/update-boots.sh`'s bad-signature boot expects 2 compiled-in keys (the release key and the test key); the build-failure #87 was that line.

### Documentation
- README *Self-update*, open-issue table and *Not wired*; deck; CLAUDE.md module map.

## [v0.13.0] — 2026-10-02

### Added
- the boot medium updates itself from the current release (#83): `update =` in `stormboot.conf`; a text manifest signed with Ed25519 (`ed25519-compact` 2.6, verify only) and checked against compiled-in keys before it is parsed, then per-file size and SHA-256; a serial that never goes down (`\stormboot\state`, NV `StormBootMinSerial`, never a serial that failed there); `*.new`/`*.prev` swap with the bootloader last and a restart; a trial that passes on an attach (or a `local` intent) and is put back, with the previous binary chain-loaded, after two starts without one; `StormBootUpdate` for Linux; read-only media and `update = off` skip. `RELEASE_KEYS` is empty until stormcentral#279 makes its key, so no medium updates yet. `src/manifest.rs` (core-only, 10 host tests), `src/selfupdate.rs`.
- `\stormboot\local.conf`: the medium's own settings, read first, never updated.
- `build-boot-agent.sh --update URL --tree DIR`; the media goldens carry `media/` and `media.files` and name their `update =` URL on stormcentral.
- `tests/update-ovmf.sh`: six self-update boots off a writable disk with a test key; `tests/net-ovmf.sh` checks an ISO skips its update as read-only. tcp4probe prints `StormBootUpdate`.

### Changed
- `clock::lookup` is shared with the self-update; the banner prints before the trial check.

### Documentation
- README *Self-update*, the conf table (`update`, `local.conf`), outbound connections, goldens, build command; CLAUDE.md module map, load-bearing facts and work plan (#83).
- refreshed from the code (changes since 2026-09-25). README: the failure console's `state`/`dhcp [secs]` (smoltcp, not the firmware's IP4 policy) and `help`; the fall-through's NTP sync and NIC release; `rng` and `media` in the `stormboot.conf` table; the NTP and DNS/UDP outbound connections; six host suites; stormblock#148 released in v20.0.0 (not on forge); v0.12.0 status with server1's 4K boot and server3's smoltcp boot; open-issue table. Deck: v0.12.0, the clock and hand-off, the full config table, how the two media goldens ship, test counts, planned/status slides. CLAUDE.md: #56 verified on metal (#68), #37's server1 boot, the media components registered, #23 closed, #148 released, #3's v0.8.1 override, #19 decided (#32, #59).

## [v0.12.0] — 2026-10-01

### Added
- set the hardware clock from NTP before Linux starts (#77). After the claim (or on the fall-through, before the NICs are released) stormbootx sends one SNTP request over smoltcp UDP to DHCP option 42's server, else `ntp =` in `stormboot.conf` (`host[:port]`, `off`), else `pool.ntp.org` (looked up with one A query over UDP). Two tries of 1 s; a reply is believed only from a synchronised server that echoes the random transmit timestamp. If the RTC is more than a second off, UEFI `SetTime` writes UTC (time zone and daylight as the firmware had them). One `clock :` console line. The outcome goes to Linux in the volatile `StormBootClock` variable (`synced:<server>` / `unsynced`, #76's vendor GUID) for stormcos#213. `src/sntp.rs` (core-only, host-tested), `src/clock.rs`, `dnsname::{a_query, a_answer}`, `net::udp_exchange`; smoltcp gains `socket-udp`; DHCP asks for option 42. `build-boot-agent.sh --ntp`.
- `tests/net-ovmf.sh` runs a stub SNTP server: boot 1 must set the RTC to 2031 and tcp4probe must read it back with `StormBootClock = synced:10.0.2.2`; boot 2's LI-3 answer must set nothing (`unsynced`). tcp4probe prints `StormBootClock` and the RTC.

### Documentation
- README (*What it touches at boot*, step 5a, the network path, `ntp =`, tcp4probe) and CLAUDE.md (module map, sc-build command, work plan).

## [v0.11.0] — 2026-10-01

### Added
- hand the claimed name and host NQN down to Linux in two volatile EFI variables, `StormBootTag` and `StormBootHostNqn` (vendor `ab361f54-0166-44a4-a088-1ac22e98ab76`, attributes `BOOTSERVICE_ACCESS | RUNTIME_ACCESS`), so the initramfs claims as the machine stormbootx claimed as rather than as a chassis serial eight blades share (#76, stormblock#249). The tag is the claim reply's host, else the name claimed; with no claim, only a stated tag or the DNS name. `src/handoff.rs`; `universal::handoff_value_ok` (host-tested) keeps out anything Linux would ignore. tcp4probe prints what was handed down, and `tests/net-ovmf.sh`'s second boot now claims successfully and checks the payload reads both at attributes 0x6.

### Changed
- the rustnic media pins stormnic-ixgbe 563ea8d (stormnic-ixgbe#21): the 82599 MAC reset's EEMNGCTL.CFG_DONE0 wait is logged and `Start` continues, where on server3 it timed out (`EEMNGCTL 0x80000196`) and failed Start. Built `--locked`; console label `media : rustnic ixgbe@563ea8d mlx4@cf37f8b`. The normal media is unchanged (#65). Goldens `golden-stormbootx-rustnic-416b7237c78a29a3` and `golden-nic-drivers-4bd5817b92cc`.
- the rustnic media pins stormnic-mlx4 v0.2.1 (cf37f8b, stormnic-mlx4#15): the UAR's PCI I/O BarIndex comes from `GetBarAttributes`, so doorbells work on AMI Aptio 4, which numbers BARs rather than BAR registers; VPI ports are driven as Ethernet. Built `--locked`; console label `media : rustnic ixgbe@8ea722a mlx4@cf37f8b`. The normal media is unchanged (#64). Goldens `golden-stormbootx-rustnic-bde9ae3a566c4d7d` and `golden-nic-drivers-8cb3943d42f9`.
- the rustnic media pins stormnic-ixgbe 8ea722a (stormnic-ixgbe#19): `Start` no longer fails on AMI Aptio 4, which refuses the PCI I/O attribute calls; it enables memory decode and bus mastering through the command register instead. Built `--locked`; console label `media : rustnic ixgbe@8ea722a mlx4@4c2d318`. The normal media is unchanged (#63). Goldens `golden-stormbootx-rustnic-f32f6e50a16edd5e` and `golden-nic-drivers-fcb89ab03ecb`.

### Documentation
- README (*What it touches at boot*, step 6a, `tcp4probe`) and CLAUDE.md (module map, work plan) describe the handoff variables (#76).

## [v0.10.0] — 2026-09-30

### Added
- every medium carries `\startup.nsh` (`media/startup.nsh`, #60): a machine with no boot option for the media drops to the firmware's EFI Shell, which now finds the stormbootx volume on `fs0`..`fs7` (`\EFI\BOOT\BOOTX64.EFI` and `\stormboot\stormboot.conf`, so a local disk's ESP is never started) and starts it unattended.
- `tests/shell-ovmf.sh ISO old|ovmf 'LINE' …` boots an ISO from an EFI Shell under OVMF, behind a decoy ESP: the old EDK shell (`Shell_Full.efi`, edk2-stable201811, digest-pinned; the shell AMI Aptio 4 carries) and OVMF's own Shell 2.x.

### Documentation
- README, CLAUDE.md and the deck: `startup.nsh`, `tests/shell-ovmf.sh`, and the shells in the sc-build command.

## [v0.9.0] — 2026-09-30

### Breaking
- stormbootx carries its own TCP/IP and never uses the firmware's `EFI_TCP4`, `EFI_DHCP4` or `EFI_IP4_CONFIG2` (owner's decision on #56). `src/net.rs` runs smoltcp 0.14 on every NIC's `EFI_SIMPLE_NETWORK`, which it opens exclusively. That gives DHCP on every NIC at once (options 12/15/6 kept for the name, #23), ARP, and TCP for the engine API and NVMe/TCP. A machine needs only a NIC driver: server3 (X9) had one and no `EFI_TCP4`. The console prints `tcp4 : smoltcp over SNP (nic 0 <mac>, …)`, and the NIC table once. The platform's IP4 policy is no longer rewritten to DHCP in NVRAM. On the fall-through the NICs are given back to the firmware.

### Added
- `src/entropy.rs` supplies the TCP ISN seed, the DHCP xid and the local ports. It uses the firmware's `EFI_RNG_PROTOCOL` (read only; none is ever installed), then RDSEED/RDRAND (RNDR on aarch64), then cycle-counter jitter hashed with SHA-256 over the firmware time, the MAC and the SMBIOS UUID. The console prints `rng : firmware | rdrand | rndr | jitter`. It never refuses to boot for lack of entropy. `rng = cpu | jitter` in `stormboot.conf` (`build-boot-agent.sh --rng`) skips the sources above it.
- `build-boot-agent.sh --engine ADDR` names the portal and engine host with the claim left on.
- the console's `state`/`dhcp` show stormbootx's own leases.
- `tests/net-ovmf.sh` boots stormbootx under OVMF with no firmware network stack, against a stub engine and a stub NVMe/TCP target (4096-byte blocks, a 96 MiB `BOOTX64.EFI` read through BlockIO and started). It boots as shipped, then with the firmware RNG and RDRAND/RDSEED masked, which must print `rng : jitter`.

### Documentation
- README, CLAUDE.md and the deck: no firmware TCP4 requirement; the network path and entropy chain.
- work plan for #56 (the EDK2 network stack on the media): current EDK2's IPv4 drivers need `EFI_RNG` and `EFI_HASH2`, which Aptio 4 lacks. The choice of stack has gone to the owner.

## [v0.8.2] — 2026-09-30

### Fixed
- the ISO media is built the way Debian's netinst is (#55): isohybrid, with an MBR partition of type `0xef` and a GPT entry over `/esp.img` beside the El Torito catalog, and the ESP at `mkfs.fat`'s default geometry (4 MiB: FAT12, 2 KiB clusters; was FAT16 at 512-byte clusters). AMI Aptio 4 (server1, X9) read the first 12 KB of the old ESP over virtual media and hung at POST A2, while Debian's ISO booted. The `.img` stick's ESP uses the same geometry. `tests/iso-layout.sh` checks the layout.

## [v0.8.1] — 2026-09-30

### Fixed
- `auto` and every doubt claim again, as before v0.8.0 (owner's override on #3: until boot intents work on forge v20, every boot claims). The v0.8.0 local-ESP rule is off by default; `local_when_bootable = true` in `stormboot.conf` turns it on.

## [v0.8.0] — 2026-09-30

### Added
- `auto`, and every doubt (a 404, an unreachable engine, an unknown intent), boots the local disk when one can boot: a whole, non-removable disk other than the boot media whose GPT has an ESP carrying `\EFI\BOOT\BOOTX64.EFI`, read with `esp.rs` and no network. A machine with nothing to boot claims as before; `install` always claims and `local` always falls through (#3, the owner's answer; also answers #31). Console: `local : …`. `intent::decide` holds the rule, with host tests.
- the rustnic media carries `stormnic-mlx4.efi` for the ConnectX-3, pinned at cef8dc5 (stormnic-mlx4#2 firmware bring-up, #3 Ethernet data path) and built `--locked`; the nic-drivers golden carries it as `stormnic-mlx4.efi.off`. Every Rust driver is checked to be PE subsystem 11 when built, and `STORMNIC-SOURCE.txt` has a line per driver. `STORMNIC_ON_MEDIA` is now a list (`ixgbe mlx4`). Console label: `media : rustnic ixgbe@9476135 mlx4@cef8dc5`. The normal media is unchanged (#34). Goldens `golden-stormbootx-rustnic-b4f33d9566d6127e` and `golden-nic-drivers-5b867545c91e`.

### Fixed
- `tests/media-ovmf.sh` failed on any console over 20 lines: `grep | head` under `pipefail` exited on grep's broken pipe before checking a line (#49).

### Changed
- the rustnic media pins stormnic-ixgbe 0dd4267: SNP and a MAC device path on a child handle after bring-up and the DMA check (stormnic-ixgbe#4), so the firmware's MNP/IP4/TCP4 can bind to the Intel 10G. Built `--locked`; console label `media : rustnic ixgbe@0dd4267 mlx4@4c2d318`. The normal media is unchanged (#51). Goldens `golden-stormbootx-rustnic-6d88515819338a0e` and `golden-nic-drivers-56ea4782ef2a`.
- the rustnic media pins stormnic-mlx4 v0.2.0 (4c2d318): SNP on a child handle per Ethernet port with a MAC device path (stormnic-mlx4#4); `Start` keeps the ConnectX-3 (~1.5 s per NIC + up to 5 s for link, was ~20 s per port), the #3 broadcast self-test no longer runs, and ExitBootServices stops its DMA. Built `--locked`; console label `media : rustnic ixgbe@2afd319 mlx4@4c2d318`. The normal media is unchanged (#50). Goldens `golden-stormbootx-rustnic-ab4e848a4dcfcaf3` and `golden-nic-drivers-10a25ce0a5a3`.
- the rustnic media pins stormnic-ixgbe 2afd319 (RX/TX descriptor rings with DMA and a broadcast check frame in Start, stormnic-ixgbe#3; still no SNP), built `--locked`; its console label is `media : rustnic ixgbe@2afd319 mlx4@cef8dc5`. The normal media still carries no stormnic-ixgbe (#48). Goldens `golden-stormbootx-rustnic-7f260c307c5ee784` and `golden-nic-drivers-60d62cc3aee9`.
- the rustnic media pins stormnic-ixgbe 9476135 (PHY and link code matched to its spec, stormnic-ixgbe#13; 25 device IDs), built `--locked`; its console label is `media : rustnic ixgbe@9476135` (#47). Golden `golden-stormbootx-rustnic-f82a05f6013ea469`.

### Documentation
- Work plan: #3 answered by the owner and built; README's intent table and `auto`.

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
