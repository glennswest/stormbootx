# CLAUDE.md — stormbootx

A UEFI application that attaches a remote image over NVMe/TCP, publishes it
as `EFI_BLOCK_IO_PROTOCOL` so the firmware's partition and FAT drivers see its
GPT and ESP, and chain-loads the image's `\EFI\BOOT\BOOTX64.EFI`. ~230 KB,
`no_std`. It carries its own TCP/IP (smoltcp on SNP, #56), so the one thing it
needs from the firmware's network side is a NIC driver's `EFI_SIMPLE_NETWORK`.

**It is stage one of two, not a duplicate of stormuefi.** stormbootx answers
*which image* and attaches it; the `BOOTX64.EFI` it starts on a stormcos image
is **stormuefi**, which selects, verifies and boots a pallet off that disk and
has no network code at all. A node booting from its own drive runs stormuefi
alone. Legacy BIOS is **stormboot4bios** (planned, own repo; both stages in one
loader, and #10 is its prerequisite here).

Read the cross-project rules in `../CLAUDE.md` first — in particular **build
with `sc-build` after pushing, never on this VM and never as root**, and
scratch files go in `tmp/`.

## Build

Push first, then `sc-build` from this checkout. It builds the pushed commit
on dev.g8.lo as `stormbuild`. There is no checkout on dev, and no `ssh root@`.
The plain `cargo build && cargo test` default does not suit a `no_std` UEFI
crate, so name the command. This builds the three binaries, runs the eight
host suites, boots `espprobe` and stormbootx under OVMF, boots an ISO's
`startup.nsh` from two EFI Shells, and runs the self-update's boots:

```bash
sc-build 'cargo build --release --target x86_64-unknown-uefi && mkdir -p t && \
  rustc --edition 2021 --test src/intent.rs -o t/intent-test && ./t/intent-test && \
  rustc --edition 2021 --test src/sha256.rs -o t/sha256-test && ./t/sha256-test && \
  rustc --edition 2021 --test src/universal.rs -o t/universal-test && ./t/universal-test && \
  rustc --edition 2021 --test src/dnsname.rs -o t/dnsname-test && ./t/dnsname-test && \
  rustc --edition 2021 --test src/esp.rs -o t/esp-test && ./t/esp-test && \
  rustc --edition 2021 --test src/sntp.rs -o t/sntp-test && ./t/sntp-test && \
  rustc --edition 2021 --test src/manifest.rs -o t/manifest-test && ./t/manifest-test && \
  rustc --edition 2021 --test src/installconf.rs -o t/installconf-test && ./t/installconf-test && \
  R=${CARGO_TARGET_DIR:-target}/x86_64-unknown-uefi/release && \
  tests/esp-ovmf.sh $R/espprobe.efi $R/tcp4probe.efi && \
  tests/net-ovmf.sh $R/stormbootx.efi $R/tcp4probe.efi && \
  scripts/build-boot-agent.sh --iso --binary $R/stormbootx.efi --media shelltest --output $PWD/t/s.iso && \
  LAYOUT=cd-last tests/shell-ovmf.sh t/s.iso old "media       : shelltest" && \
  LAYOUT=cd-last tests/shell-ovmf.sh t/s.iso ovmf "media       : shelltest" && \
  tests/update-ovmf.sh $R/tcp4probe.efi'
```

`tests/update-ovmf.sh` (#83) must stay last: it rebuilds `stormbootx.efi`
with an Ed25519 test key compiled in (`STORMBOOTX_UPDATE_TEST_KEY`) and boots
a writable GPT medium on a USB stick ten times through
`tests/update-boots.sh`, sourced by `net-ovmf.sh` for its stubs (#89):
stormcentral's real serial-1 manifest (`tests/fixtures/`) verifies; bad
signature refused; serial 5 written, restarted, trial passed on the attach;
serial 5 `current`; serial 6 refused as another machine's canary, then taken
as this one's, retiring a driver; serial 7 refused for room; serial 8 (a
dead NVMe port in its conf) fails two trial starts and the third puts serial
6 back. mtools checks the disk after each boot.

`tests/net-ovmf.sh` (#56) boots stormbootx under OVMF with no firmware
network stack, against a stub engine, a stub NVMe/TCP target (4096-byte
blocks, a 96 MiB `BOOTX64.EFI`) and a stub SNTP server (#77: boot 1 must set
the RTC to 2031, boot 2's unsynchronised answer must set nothing). It runs twice: as shipped, and with the
firmware RNG and RDRAND/RDSEED masked (`rng : jitter`). The shipped boot's
ISO names an `update =`, which must be skipped as read-only (#83).

`tests/shell-ovmf.sh ISO old|ovmf 'LINE' …` (#60) boots an ISO from an
EFI Shell with no boot option for it, and requires its `startup.nsh` to
start stormbootx: `old` is the EDK shell Aptio 4 carries, `ovmf` the build
box's Shell 2.x. OVMF connects only devices in the boot order, so the test's
CD carries a bootindex after the shell's; without it the CD is never mapped.

`tests/iso-layout.sh ISO …` checks an ISO is isohybrid with a FAT12 ESP,
the layout Aptio 4 boots (#55). `tests/media-ovmf.sh ISO 'LINE' …` boots a media ISO under OVMF and requires
console lines (#45: `media : …`); build the goldens under `t/` in the same
job to use it.

The last line boots `espprobe` under dev's OVMF (KVM there) against a
4096-byte disk (#37). `R=` is the one place the target dir is named, and it
reads `CARGO_TARGET_DIR` rather than assuming `target/`.

Don't `ls target/...` afterwards. dev sets its own `CARGO_TARGET_DIR`, so the
`ls` fails, and sc-build files that as a `build-failure` issue (#16 was one).

There is no `cargo test`: this is a `no_std` UEFI binary with no host target, so
the compile *is* the check. A macOS build is not possible at all here — the
target is `x86_64-unknown-uefi` and the code is entirely firmware-facing.

`src/sha256.rs` is the one exception, and it is worth keeping. It touches only
`core` and names no `crate::` item, so it compiles standalone as its own crate
and the FIPS vectors can actually be run (the `rustc --test` lines in the
`sc-build` command above).

`--edition 2021` is not optional: bare `rustc` defaults to edition 2015, where
`core` is not in scope and the file will not compile even though it is correct.

Anything that gives that module a dependency on the rest of the crate takes
those vectors out of reach. Don't.

`src/intent.rs` follows the same rule for the same reason: it parses the boot
intent reply and decides, using `core` only, and the HTTP exchange stays in
`registry.rs`. Its tests are the proof that every doubt reads as `auto`. `src/universal.rs`
(#15) is the third: the engine-version gate, the MAC choice and the claim
reply's `host` object, `core` only. `tcp4probe.rs` includes it by `#[path]`
because `tcp4.rs` uses it, so a new `crate::` use in `tcp4.rs` must be
carried there too (#24 was that). `src/dnsname.rs` (#23) is the fourth:
DHCP option parsing and the PTR wire format, tested against microdns
replies captured on 2026-09-27. `src/esp.rs` (#37) is the fifth: GPT and
FAT, tested on images `mkfs.fat` and mtools build inside the test, so they
must be on the build box's `PATH` (they are on dev). `src/sntp.rs` (#77) is
the sixth: the SNTP packet, the reply checks and the UTC calendar.
`src/manifest.rs` (#83) is the seventh: the self-update's manifest, state
file, start/serial decisions and HTTP framing. `src/installconf.rs` (#79) is
the eighth: `install-config.yaml`'s chunk variables and header; tcp4probe
includes it and `sha256.rs` by `#[path]` to read the hand-down back.

`Cargo.lock` is tracked, as it should be for anything that produces a binary.
Without it every build resolved fresh, and this is a firmware binary whose
whole dependency surface is two crates that move: a `cargo build` on 2026-09-02
offered `uefi` 0.40 against the 0.39 the code was written for. Nothing here
should change under a build nobody asked to change it. Bump the pin
deliberately, in its own commit, and rebuild.

`./scripts/build-boot-agent.sh` is packaging only: it puts the built `.efi` and a
`stormboot.conf` onto a GPT `.img` (or, with `--iso`, an `.iso` instead), by
default in `tmp/images` in the checkout. Nothing in the agent creates media.

**What ships is a golden, never a file on dev** (owner, 2026-09-28, #21).
`deploy/build-golden.sh stormbootx|nic-drivers OUT` writes that golden's tree
into OUT and nothing else (README, *How it ships*). Every build gets a fresh
drive, and anything left on dev is deleted, so never write outside the job
(no `/build/images`, no `/build/stormbuild/images`). To look at media in an
`sc-build`, build it under `t/` and inspect it in the same job. The golden
request, once stormbootx is a component, is `stormcentral component build
stormbootx --url http://stormcentral.g8.lo`.

## Version locations

| File | Field |
|---|---|
| `Cargo.toml` | `version` |
| `CHANGELOG.md` | latest release heading |
| git tag | `vX.Y.Z` |

## Module map

| File | Job |
|---|---|
| `src/main.rs` | `run()`: the boot, step by step, and the fall-through |
| `src/smbios.rs` | the serials (Type 1 → 2 → 3, placeholders and shared chassis serials rejected), before any network exists; the MAC as the floor |
| `src/net.rs` | the TCP/IP stack (#56): smoltcp on every NIC's SNP (opened exclusively), DHCP, `TcpSocket`, the TSC clock; `release` gives the NICs back on the fall-through |
| `src/snpwatch.rs` | which driver is under each SNP (named before the first call), and a `TPL_NOTIFY` timer that names an SNP call that has not returned (#88) |
| `src/entropy.rs` | randomness for the ISN, DHCP xid and ports: firmware `EFI_RNG`, then RDSEED/RDRAND (RNDR on aarch64), then jitter through SHA-256; never fails |
| `src/tcp4.rs` | tcp4probe only since #56: a blocking socket over the firmware's TCP4 |
| `src/drivers.rs` | load NIC drivers from `\stormboot\drivers` on the media (#26), after the platform's own bind |
| `src/dhcp4.rs` | tcp4probe only since #56: DHCP through the firmware's `EFI_DHCP4` |
| `src/nvme.rs` | the NVMe/TCP initiator |
| `src/handoff.rs` | `StormBootTag`/`StormBootHostNqn`, volatile EFI variables naming the machine to Linux's initramfs (#76, stormblock#249); `StormBootClock` (#77); `StormBootUpdate` (#83); `StormBootInstallConfig` + chunks (#79) |
| `src/selfupdate.rs` | the boot medium updates itself (#83): trial count and revert at start; fetch, Ed25519 check, `*.new`/`*.prev` swap and restart after `net::up`; `mark_good` after the attach; `StormBootMinSerial` (NV) |
| `src/manifest.rs` | the self-update's signed manifest, `RELEASE_KEYS` (stormcentral's, #86), `\stormboot\state`, trial and serial decisions, HTTP framing, `update =` (core-only, host-tested) |
| `src/installconf.rs` | `install-config.yaml` (#79): the media slot, the chunk variables' names and the `v1:<len>:<N>:<sha256>` header (core-only, host-tested) |
| `src/clock.rs` | the RTC set from NTP before Linux (#77): option 42 / `ntp =` / `pool.ntp.org`, one bounded SNTP exchange, `SetTime` in UTC |
| `src/sntp.rs` | SNTP request and reply checks, NTP era → Unix, the UTC calendar (core-only, host-tested) |
| `src/blockio.rs` | publish the namespace as a block device, then chain-load its `BOOTX64.EFI` (the firmware's FAT, then `esp.rs`) |
| `src/intent.rs` | the boot intent (`install`/`local`/`auto`) read before the claim; every doubt is `auto` |
| `src/registry.rs` | read the intent; claim `boothost/<tag>`, or `boothost/default` by MAC, every claim naming the agent (#90); read the engine's version; also the old sbregistry `/v1/clones/claim` path, compiled out by `USE_REGISTRY = false` |
| `src/dnsname.rs` | the machine's DNS name (#23): DHCP options 12/15/6, PTR query and answer over DNS/TCP |
| `src/universal.rs` | universal boot (#15): is the engine new enough, which MAC is the machine's, what host the reply named |
| `src/esp.rs` | the attached ESP read without the firmware's FAT (#37): GPT, FAT12/16/32 at 512..4096-byte sectors |
| `src/espboot.rs` | read `BOOTX64.EFI` through `esp.rs` and `LoadImage` it from the buffer |
| `src/espprobe.rs` | third binary: who can read a 4K ESP; booted under OVMF by `tests/esp-ovmf.sh` |
| `src/sha256.rs` | the digest, because `EFI_HASH2` is optional |
| `src/config.rs` | the target, read from the media rather than compiled in; `local.conf` read before `stormboot.conf` (#83) |
| `src/shell.rs` | timed, never-forced failure console before the fall-through |
| `src/mlxfec.rs` | ConnectX FEC NV read; write only on a `fec =` recovery stick |
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
- **Chain-load; never leave the boot to the boot manager.** A disk that appears
  mid-boot-option is not in `BootOrder`; the machine drops to setup. Real EDK2
  also skips a bare BlockIO in `PartitionDxe`, so the handle carries a vendor
  device-path node, and the ESP is matched **strictly** by it — a loose match
  once booted a stale Windows install off a local SAS disk.
- **stormbootx's TCP/IP is its own (#56, owner 2026-09-30).** smoltcp on SNP,
  on every machine, and the firmware's TCP4 is never asked for: server3 (X9)
  had a NIC driver loaded and still no `EFI_TCP4`. EDK2's own stack is not
  an alternative: since edk2-stable202405 Ip4/Udp4/Dhcp4/TcpDxe refuse to
  start without `EFI_RNG` (`PseudoRandomU32` in their start), and TcpDxe
  without `EFI_HASH2`. SNP is opened `EXCLUSIVE` so a firmware MNP can't
  steal frames; `net::release` gives the NICs back on the fall-through.
  Never install an `EFI_RNG`: the Linux EFI stub seeds its RNG from it.
  Several facts below are about the firmware stack; they remain true of
  firmware and of `tcp4probe`, and each is kept where it still applies to
  `net.rs` (multiple NICs, the late-driver wait, never trusting one link
  sample).
- **Fedora's OVMF has no upper network stack.** SNP present, MNP/IP4/TCP4
  absent, and `ConnectController` over every handle does not change it. Since
  #56 that makes it exactly the emulator to test with: `tests/net-ovmf.sh`
  runs stormbootx's whole network path on it.
- **A server has more than one network stack, and the first is not yours.**
  Each NIC carries its own `EFI_TCP4` service binding, so `handles.first()` is a
  coin flip between the 1 GbE management port and the 25 GbE storage port. The
  wrong one gives `tcp4 : available` and then `NO_MAPPING` forever. Every
  interface is tried, ranked by link then descending MTU — this is the storage
  path, so the jumbo port goes first.
- **Never assume the platform ran DHCP.** `use_default_address` needs an address
  the platform already holds, and nothing guarantees one: the policy may be
  `STATIC`, or DHCP may only run as part of a PXE attempt nobody asked for.
  `dhcp4.rs` leases one directly and states it in `Tcp4ConfigData`. Fallback
  only — `EFI_DHCP4` is optional, like every other stack this refuses to need.
- **The first boot option may run before the network stack exists.** On the
  Dell (C2NR0Q2), same firmware and same boot session: the *first* boot option
  reported `EFI_TCP4 is not present` and the *second*, seconds later, found it
  **already bound**. `ConnectController` cannot bind a driver the platform has
  not dispatched, so more passes were never the answer — time was.
  `ensure_available` now retries for up to 5 s and reports the delay. Do not
  "optimise" that wait away: without it, whichever stick happens to be first in
  the boot order is the one that silently decides the machine has no network.
- **A real server may not carry the stack either, and that is a setup switch.**
  2026-09-03, first hardware run: a Dell (service tag C2NR0Q2) booted
  stormbootx, read its own tag out of SMBIOS, and reported `EFI_TCP4 is not
  present` — *after* the full `ConnectController` pass, so the drivers were not
  in the loaded firmware at all rather than merely unbound. On Dell the switch
  is the UEFI network stack: Integrated NIC set to **Enabled with PXE**, or
  **UEFI Network Stack** under Network Settings. Enabling PXE is not because
  anything wants PXE — it is what makes firmware load MNP/IP4/TCP4.
- **The stack can be there with no driver under it** (#26, Supermicro X9,
  2026-09-27). SnpDxe…TcpDxe all loaded, Network stack = Enabled, and the
  only UEFI NIC driver in the firmware (`PRO/1000`) manages no device: the
  Intel 10G and the ConnectX-3 carry legacy option ROMs only. No SNP, so no
  TCP4, and no setup switch fixes it. The media carries the driver
  (`\stormboot\drivers` on the rustnic medium: the Rust stormnic drivers;
  iPXE's was the first, and no medium carries it since #91). The platform's own drivers
  bind first, so a media driver never displaces a native one.
- **Never close an event the TCP driver may still signal** (#26, server1,
  2026-09-28). A token that timed out is still queued; closing its event and
  then aborting the connection (`Drop`'s `Configure(NULL)`) signals freed
  pool. EDK2 checks the signature and shrugs; AMI Aptio 4 did not, and the
  next `CreateEvent` failed with `INVALID_PARAMETER`. `Tcp4Socket::retire`
  aborts first. And `CheckEvent` *clears* what it reports, so a token's state
  is `pump`'s record, never a second `CheckEvent`.
- **microdns sends no DHCP option 12** (#26). It registers the reservation's
  name in DNS instead, so on a microdns network the name comes from the PTR,
  and the reservation names the NIC (`server1a`), not the machine (`server1`).
- **Serials repeat within a chassis** (#26). Seven X9 blades report the
  enclosure's serial as Type 1. Placeholders are not the only non-identity.
- **Proxmox's OVMF does, and it is the emulator to use.** Verified 2026-09-03
  on pve.g8.lo (`pve-edk2-firmware`, Nov 2025) with `tcp4probe` on VM 2062:
  every protocol reads *absent* as found and all nine appear after a
  `ConnectController` pass — the `BoundAfterFullPass` case #5 added. The boot
  menu lists `UEFI HTTPv4/v6`, which is why: HTTP boot pulls TCP4 in. So the
  network path *can* be exercised in a VM, on Proxmox rather than on Fedora's
  OVMF.
- **A 4K image's ESP is a 4096-byte-sector FAT, and old firmware misreads
  it** (#37, #33). Linux won't mount a FAT whose sector is smaller than the
  device's block, and volumes stay 4096 (owner: "512 will kill our
  performance"). AMI Aptio 4 mounts such an ESP and then answers `NOT_FOUND`
  for `BOOTX64.EFI`. Fedora's OVMF (EDK2) loads it fine (`tests/esp-ovmf.sh`). When the firmware loads
  nothing, `esp.rs` reads the file and `LoadImage` takes the buffer. A 512e
  view would not have fixed Aptio: the boot sector still says 4096.
- **The transfer size comes from MDTS, never from the MTU.** Sizing a command
  to fit one frame inverts: a 9000 path lands on 8 KiB and a 1500 path on
  64 KiB. TCP segments to the MSS and never IP-fragments, so frames are not
  the constraint; round trips are, because `read` keeps one command
  outstanding.
- **There is no DNS *discovery* in this binary.** The portal is a fixed
  appliance address named on the media or compiled in; *which image* is a
  `boothost/<name>` synonym claimed from the engine. Discovery was removed
  rather than switched off — it was a second place for the answer to live
  and a timeout on every boot in a zone nobody published. Don't reintroduce
  it. The one DNS query here since #23 is the machine asking the PTR of **its
  own address** for **its own name**, over TCP to the DHCP-given server,
  and only when DHCP option 12 gave none. It never finds a portal, and a
  network with no names boots exactly as before.
- **A machine is its DNS name, and the name is the first label**
  (#23, stormblock#199: hosts are `server3`, `stormblock1`). It comes from
  the lease of **the interface that reached the engine**, not any NIC's, because
  a management port and a storage port may hold different reservations.
  DHCP and PTR can disagree (g10's `192.168.10.31`: reservation `gb10b`,
  PTR `gb10.g10.lo`); DHCP wins, as the owner ordered. A 404 on the name
  moves on to the MAC. The name claim carries `{mac, serial}` so the engine
  can tie a new name to a host it knows (stormblock#204).
- **A machine nothing has named claims `boothost/default` by its MAC — and
  only from an engine with stormblock#200** (#15, universal boot). There
  the engine keys the claim on the MAC and gives each machine its own
  clone (`mac-<hex>`). On an engine from v17 to 19.3.0 the same claim is
  tag `default`: one boot clone name for every machine, each claim
  releasing the clone the last one is running on. So `universal.rs` gates
  it on the public `/api/v1/health` version being **strictly after 19.3.0**,
  and an unreadable version is "no". Don't loosen that gate to make a test
  pass. Not the SMBIOS serial either: serials are not unique (seven
  MicroCloud nodes share one), and a serial claim pins them all to one host.
- **The MAC is the lowest usable one of every NIC, not `handles.first()`.**
  SNP handles appear in driver-bind order, which `ensure_available`'s
  on-demand binding can change between boots. Same lesson as the multi-NIC
  fact below, in the identity instead of the socket.
- **A boot path must never need the network in order to boot without it.**
  Every failure in discovery or attach falls through to the local disk. One
  provisioning outage must not become a fleet outage.
- **Link-down early in UEFI is not evidence of anything.** A 25G RS link needs
  seconds to come up after a reset, and `snp.media_present` sampled once at
  that moment reads a perfectly healthy card as dark. This is the same fact as
  the `ensure_available` retry above, and forgetting it in a second place cost
  the R230 its 25G ports on 2026-09-07: the FEC self-heal believed a single
  all-down sample, wrote NV config to the card and warm-reset. **Never let a
  one-shot link read authorise a write.** If a decision depends on link state,
  wait for it the way `ensure_available` waits for the stack.
- **The 25G links dropping is usually the switch, not this binary.** dsw1
  latches an SFP28 port every time the peer powers off: light present, no PCS
  lock, immune to `shutdown`/`no shutdown` and to every host-side reset. Only a
  FEC *transition* clears it — both ports came up on the final stage of the
  walk, on CL108-RS, the value they had held for twelve hours. It is automated
  in the **`dswfecfix`** repo, running on the switch under cron; do not
  re-implement the recovery here, and do not read a dark 25G link on this
  fabric as evidence about stormbootx until that log has been checked.
- **Persistent config on the NIC is not stormbootx's to change at boot.** The
  card negotiates what it needs; a wrong fabric is fixed on the fabric, where
  the change is visible, reversible and applies to every host at once. A
  boot-time NV write is invisible, per-machine, survives reinstall, and can
  only be undone from the thing it just broke the network on.

- **The self-update believes the signature and nothing else** (#83). The
  manifest is fetched over plain HTTP from a name DHCP's DNS resolves, so
  everything about where it came from is forgeable; the Ed25519 check against
  the compiled-in `RELEASE_KEYS` comes before the manifest is parsed, and
  each file's size and SHA-256 before it is written. Never add a path that
  writes the medium on a hash alone, and never compile a test key into a
  golden: `STORMBOOTX_UPDATE_TEST_KEY` is for `tests/update-ovmf.sh` only.
- **A serial never goes down on a medium, and a failed one is never retried
  there.** A stick that falls back after a trial would otherwise take the same
  release again on its next boot and loop. The fix for a bad release is a
  new serial.
- **The trial is counted at start, before anything that could hang.** A new
  set that hangs in the NIC driver or the network never reaches the code that
  would notice; counting first means the third power cycle puts the old set
  back. The swap renames the bootloader last, and the state is written
  before the swap, so an interrupted swap is put back by the same count.

## Asking the machine before booting it

On a Dell with iDRAC, the hardware questions can be answered without a boot,
which is worth remembering after a day spent inferring them from console output
that turned out to come from three different stale sticks.

```bash
# Every component the Lifecycle Controller knows about.
curl -sk -u root:<pw> -X POST -H 'Content-Type: application/json' \
  https://<idrac>/redfish/v1/Managers/iDRAC.Embedded.1/Actions/Oem/EID_674_Manager.ExportSystemConfiguration \
  -d '{"ExportFormat":"XML","ShareParameters":{"Target":"ALL"}}'
# -> 202 + Location: a task; poll it, then: grep -oE 'FQDD="[^"]+"'
```

**Check `CollectSystemInventoryOnRestart` in that export before trusting it.**
Disabled means the inventory is stale and proves nothing.

That is how C2NR0Q2's real problem was found on 2026-09-04: the export listed
`NIC.Embedded.1-1-1`, `NIC.Embedded.2-1-1` and nothing else — no `NIC.Slot.*`,
no `RAID.Slot.*` — with CSIOR *enabled*, `Slot1`/`Slot2` enabled and
`BootMode=Uefi`. Both add-in cards were absent from the bus, which on an R230
means the riser they share. Every network symptom above it — no DHCP answer, no
jumbo interface, `NO_MAPPING` on both NICs — was the machine honestly reporting
that the port with the cable in it did not exist.

iDRAC8's Redfish (firmware 2.50) is v1.0.2 and has **no** `PCIeDevices`,
`Storage`, or manager `Attributes` endpoints. `EthernetInterfaces`,
`Storage/Controllers`, `Logs/Sel` and the SCP export are what it does have.

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

### Done (recent)

- [x] **#7 — the 25G FEC fix was switch-side, and mlxfec reads it
      (2026-09-06).** The flaky SR links (1/1/5 + 1/1/7 line-protocol-down for
      days, port 5 dead) were a FEC *mismatch*: the Dell S5148F fabric was
      pinned to `fec off`, while the ConnectX-4 Lx device-default already
      **negotiates** cl108-rs. Setting the switch to RS matched both ends and
      every link came up stable — including the dead port — with no
      shut/no-shut. The card needed no change; the fix was rolled to all 48
      SFP28 ports. The gotcha that cost the day: on OS10 10.4.3.6 the keyword is
      a **standalone, uppercase** interface command — `fec CL108-RS` (also
      `CL74-FC`, `CL91-RS`, `off`). Lowercase, `no fec off`, and putting it
      under `speed` are all rejected as "Illegal parameter". `fec ?` lists them
      but only renders over an interactive TTY (`ssh -tt`, stdin held open).

      `mlxfec` (built along the way) is a working UEFI ConnectX FEC read/write:
      it finds the card over `PciRootBridgeIo` config space (GetProtocol, never
      exclusive — exclusive tears down PciBusDxe and the NIC drivers), clears
      the cap9 semaphore the Mellanox UEFI driver parks for its whole lifetime
      (held value 0x3/0x7), runs an ICMD MNVDA access-register, and decodes the
      NV FEC override. `apply(None)` prints each port's current/next-boot FEC at
      boot as a diagnostic. Every wall (GetProtocol vs exclusive; the parked
      cap9 lock; the OperationTlv dword-1 bit layout) is in the git history.

      **Self-heal (0.3.4) — switched off in 0.3.6, 2026-09-07.** It fired when
      every recognised ConnectX 25G port read link-down, pinned the NV FEC
      override to RS and warm-reset once. Its first live firing is the only one
      it got: the R230's two 25G ports (dsw1 `1/1/5` and `1/1/7`) dropped
      together at 16:22 UTC and never came back, after **16 h 51 m of
      continuous link** on a fabric already correctly on `fec CL108-RS`. dsw1
      was not involved — up 4 days, no login since the previous evening, FEC
      `CL108-RS` configured *and* operational on all 48, uplinks forwarding
      throughout; the card stayed powered with one port lasing at −2.1 dBm
      without PCS lock and the other dark, which is a card-configuration state.
      **The defect is the trigger, not the write:** `matched_all_down` samples
      `snp.media_present` once with no settle wait, so a healthy card probed
      early enough in UEFI reads all-down — the same trap `ensure_available`
      documents and already retries around, because a 25G RS link needs seconds
      after a reset and *time was the answer*. The write self-limits (skipped
      once FEC is already RS), which is why this cost one bad write and not a
      boot loop. And there was nothing to fix: the CX4 Lx device-default
      *negotiates* cl108-rs, so a correct fabric matches it with no card-side
      change. Reviving it needs a settle wait before it believes "all down" and
      a reason to prefer pinning over the device default. PAOS (live port
      status) is *not* ICMD-reachable on CX4 Lx (only the MNVDA-family NV
      registers are), which is why the trigger read link from SNP via the
      device-path match rather than from the card — and why a wait, not a
      better source, is the fix.

**Issue numbering:** the FEC work above and its commits say `#7`, but GitHub
#7 is *boot identity* (placeholder serials, ODM boards). No issue was ever
filed for the FEC work. #7's substance is largely done — Type 2/3 fallthrough,
placeholder rejection, and `tag =` (#9, closed) — and it is still open.

- [x] #8 — closed as mis-framed: the tag already reaches the appliance as the
      host NQN on every connect; per-host discovery is stormblock's.
- [x] #9 — `tag = <id>` in `stormboot.conf` overrides SMBIOS.
- [x] #12 — docs re-audited against the code (2026-09-27, f41d587, sc-build
      passing). README build → sc-build, stamp format, sockets, a *Ports,
      health and shipping* section (no listener; ships as media, no golden),
      stormblock v17 token rule; sbregistry/DNS leftovers in module docs; the
      unwired MAC identity floor stated (#7). Cross-refs checked against
      stormblock, stormipmi and stormuefi source.

### Open, no external blocker

- [x] **#94 — the media names its stormbootx version (P2, storminstall#10).
      Closed 2026-10-06.** `build-boot-agent.sh` appends `version =
      <Cargo.toml version>` and `commit = <the binary's STORMBOOTX_BUILD, or
      unstamped>` to `stormboot.conf` (ESP, ISO9660 tree, golden `media/`),
      for every kind of medium; it dies if the `.efi` laid down does not
      carry that version. stormbootx ignores both keys; a self-update
      replaces the file with the release's. `tests/iso-layout.sh` checks
      them. dd4fe58, released **v0.19.0** (649dd07). sc-build of dd4fe58: no
      warnings, the full suite, a fake `.efi` refused (`does not carry
      version 0.18.0`), both golden trees built in the job, `iso-layout` PASS
      on all four ISOs (goldens `commit = dd4fe58`, the hand-built test ISO
      `unstamped`), and `media/stormboot/stormboot.conf` carrying both
      lines. The tag's golden tree: `version = 0.19.0`, `commit = 649dd07`.
      Goldens `golden-stormbootx-069eee57e7f1255b` and
      `golden-stormbootx-rustnic-61df2f5eed14f7ba`. Media before v0.19.0
      carry no `version =`; storminstall treats that as too old.

- [x] **#88 — a progress line while no NIC has a lease, and a named hung
      SNP call (P2, from #78). Closed 2026-10-06.** `connect_within`
      prints `waiting for a lease (N s): nic 0 link UP, DHCP n out n in, n
      frames in` once a second while no NIC holds a lease (counts kept in
      `SnpDevice`/`TxQueue`; the shell shows them too). `open_nic` names the
      NIC's driver before its first SNP call (`snpwatch::driver_of`: the
      agent that opened the parent `BY_CHILD_CONTROLLER` for a MAC child, or
      PCI I/O / NII `BY_DRIVER` on the handle; 07cdd22's "any `BY_DRIVER`"
      named OVMF's VLAN Configuration Driver, a consumer, fixed in dac1712).
      `src/snpwatch.rs` marks every SNP call; a 1 s periodic timer at
      `TPL_NOTIFY` prints `SNP.<call> (<driver>) has not returned after N s`
      (5 s, then every 30 s); `net::release` closes it. Released **v0.18.0**
      (1dfec6d). sc-build of dac1712: no warnings, suites 9/7/7/8/12/8/11/7,
      espprobe, both shells, update-ovmf, and net-ovmf's four boots: every
      boot `nic 0: driver Virtio Network Driver`, and `nolease` (its only NIC
      on a dead QEMU hub) printed 29 progress lines (`DHCP 1..15 out 0 in, 0
      frames in`) then `engine : version unknown (no address after 30 s …)`.
      blockio still 121–123 MiB/s with every call marked. The tag built
      `--locked` and passed net-ovmf. Goldens
      `golden-stormbootx-5659a6d6ba69d143` and
      `golden-stormbootx-rustnic-fe9974d052fd822f`. **Not run:** the hang
      watch firing (no driver here hangs), and metal.

- [x] **#52 — the firmware-drivers medium (`media : fw`), the third of
      #45's (owner, 2026-09-29). Closed 2026-10-06.** The owner's answer on
      #81 (2026-10-02, "I dont want the ipxe code ... we can keep the code to
      use built in nic firmware") leaves two media: rustnic, and one with no
      `\stormboot\drivers`. As #91 proposed, the `stormbootx` golden
      *became* that medium, so the name every BMC and recipe points at stays.
      `build-golden.sh stormbootx` lays no drivers and `--media fw`, ignores
      `--drivers`, and dies if a driver reaches `media/`; the component's
      `nic-drivers` input was dropped (`component edit stormbootx --set
      inputs=[]`). `run()` prints `drivers : none on the media; the
      firmware's own NIC drivers`. 04a58c4, released **v0.16.0** (e51ed45).
      sc-build of 04a58c4: no warnings, suites 9/7/7/8/12/8/11/7, espprobe,
      net-ovmf, both shells, update-ovmf, and the golden tree built in the
      job (`--drivers /nonexistent` ignored, `BUILD` `drivers = none`, no
      `media/stormboot/drivers`), `iso-layout` PASS and `media-ovmf` found
      `media : fw` and the `drivers : none` line. The tag built `--locked`
      and its golden ISO booted the same. Goldens
      `golden-stormbootx-1fdb5a8eb0710bc1` and
      `golden-stormbootx-rustnic-c2c2774b3fd29c72`. **Left for the master:**
      the X9 blades' BMCs move to `stormbootx-rustnic` before they boot the
      new `stormbootx` (it carries no driver for their NICs). **Not run:**
      the R230 on the fw medium. iPXE leaving `nic-drivers` and
      `build-nic-drivers.sh` is #91.

- [x] **#91 — retire iPXE from every medium and golden (P1, owner on #81,
      2026-10-02). Closed 2026-10-06.** #52 did the media; 39bb98b did the
      rest. `build-nic-drivers.sh` builds only the Rust drivers, as loadable
      `.efi` (no `IPXE_REF`, no iPXE fetch or make, no `IPXE-SOURCE.txt`, no
      `.efi.off`); `STORMNIC_DRIVERS` (default `ixgbe mlx4`) replaces
      `STORMNIC_ON_MEDIA`/`STORMNIC_CARRY`. The `nic-drivers` golden is
      `bin/stormnic-ixgbe.efi`, `bin/stormnic-mlx4.efi`, `STORMNIC-SOURCE.txt`;
      its registry `binaries` were changed to match. Released **v0.17.0**
      (b974528). sc-build of 39bb98b: no warnings, suites 9/7/7/8/12/8/11/7,
      espprobe, net-ovmf, both shells, update-ovmf, and all three golden
      trees in the job: nic-drivers with no iPXE and no `.off`, rustnic's ISO
      `iso-layout` PASS and under OVMF `media : rustnic ixgbe@563ea8d
      mlx4@cf37f8b` and both drivers' bindings, stormbootx with no drivers.
      The tag built `--locked`. Goldens `golden-nic-drivers-3f9b1ee49d47`,
      `golden-stormbootx-08e929b639635401`,
      `golden-stormbootx-rustnic-4207d13a085449f2`. #30 and #78 closed as
      moot, #81 and #27 closed. **Left for the master:** the X9 blades' BMCs
      on `stormbootx-rustnic`.

- [x] **#54 — the fall-through after an attach crashed OVMF (#UD at
      0x47FFFFFCA, pvetest1). Closed 2026-10-06.** An attach that booted
      nothing left `blockio::publish`'s BlockIO and device path installed
      when `main` returned `ABORTED`; the firmware unloaded the image, and
      BDS probed a disk whose functions were freed. `local_disks()` counted
      it too ("2 found" with one disk). `blockio::withdraw()` (disconnect,
      uninstall both, drop the namespace) is `fall_through`'s first step; a
      refused uninstall resets rather than return. `blockio::stale()` names
      a disk an earlier start left behind, at the start of `run()`. 935758e,
      released **v0.15.1** (782a074). `tests/net-ovmf.sh`'s third boot,
      `noesp` (blank namespace from a second NVMe stub, blank local
      virtio-scsi disk, 25 s of BDS after): on the old code (b2182c2) BDS's
      second start of stormbootx printed `1 handle(s) an earlier start
      published are still installed` and `(2 found)`; on the fix both starts
      printed `blockio : withdrawn` and `(1 found)`, no stale disk, no
      exception. Fedora's OVMF never crashed on the old code (the freed pages
      weren't reused), so the stale handle is the test's evidence, not the
      #UD. Full sc-build passing; the tag `--locked`. Goldens
      `golden-stormbootx-232ee4743e6d2f82` and
      `golden-stormbootx-rustnic-4e1013413b71399c`. **Not run:** pvetest1
      itself (the master's: a v0.15.1 ISO over its blank disk should print
      `blockio : withdrawn` and reach BDS's `No bootable option` without
      the exception).

- [x] **#79 — carry install-config.yaml from the media to the node (P1,
      stormcos#82). Closed 2026-10-03.** The slot is storminstall's
      (`docs/config-slot.md`): `\stormboot\install-config.yaml` on the
      media's ESP, at most 256 KiB. The carrier, ours to choose, is the
      owner's offer on #79: volatile EFI variables under #76's GUID, chunked
      because a firmware variable may be capped at 1 KiB.
      `StormBootInstallConfig0..N-1` hold 768 bytes each and
      `StormBootInstallConfig` = `v1:<len>:<N>:<sha256>` is set **last**; a
      refused chunk deletes the ones set, so a header means the whole file.
      Read in `run()` with the other media reads, before the network; the
      console names size and digest, never content (pull secret, API token).
      `src/installconf.rs` (core-only, 7 host tests), `config::read_bytes`,
      `handoff::set_install_config`; tcp4probe reassembles and checks it.
      b5784f9 + f0dba90 (warnings), released **v0.15.0** (c74872e). sc-build
      at the tag with `--locked`: no warnings, suites 9/7/7/8/12/8/11/7,
      espprobe, both shells, update-ovmf, and `tests/net-ovmf.sh`: a 2053-byte
      file written into the built ISO's ESP as storminstall does (MBR 0xEF
      volume, mtools) was handed down as `v1:2053:3:984c…b92b` and tcp4probe,
      started as the attached image's `BOOTX64.EFI`, reassembled 2053 bytes
      from 3 chunks with the digest matching; the boot without one handed
      down nothing. Goldens `golden-stormbootx-5e996dc349a7ec30` and
      `golden-stormbootx-rustnic-3fce00aa442b9783`. **Left elsewhere:**
      stormblock#275 (the initramfs copies it to `/state/config/` on a first
      boot and deletes the variables), stormpump#78 (applies it). **Not run:**
      a real firmware's variable store (X9 Aptio 4, the R230), a file near
      256 KiB, storminstall's own binary writing the ISO.

- [x] **#77 — set the hardware clock from NTP before Linux starts (P1,
      owner 2026-10-01). Closed 2026-10-01.** The X9 blades have no RTC
      battery, and neither BIOS nor BMC sets the clock. After the claim (or
      on the fall-through, before `net::release`) `clock::sync` sends one
      SNTP request over smoltcp UDP: DHCP option 42 of the NIC that reached
      the engine, else `ntp =` (`host[:port]`, `off`), else `pool.ntp.org`
      (one UDP A query to option 6 / `dns =`). Two tries of 1 s. If the RTC
      is out by more than 1 s, `SetTime` writes UTC (the firmware's time zone
      and daylight kept, so EDK2 writes no NV variable). `StormBootClock` =
      `synced:<addr>` / `unsynced`, volatile, #76's GUID, for stormcos#213.
      `src/sntp.rs` (core-only, 8 host tests), `src/clock.rs`,
      `dnsname::{a_query, a_answer}`, `net::udp_exchange`. Done in c35822f,
      released **v0.12.0** (35ce03b). sc-build: no warnings, suites
      9/7/7/8/12/8, espprobe, both shells, and `tests/net-ovmf.sh` with a stub
      SNTP server. Boot 1 printed `clock : was 2026-10-01 21:22:21, set to
      2031-05-04 03:02:01 UTC from 10.0.2.2:… (step +144740380 s)`, and
      tcp4probe, started as the attached image's `BOOTX64.EFI`, read `rtc :
      2031-05-04 03:02:02` and `StormBootClock = synced:10.0.2.2` (0x6). Boot
      2's LI-3 answer set nothing (`unsynced`, RTC still 2026). The tag passed
      the same `--locked`. Goldens `golden-stormbootx-dcc29fd07763a23f` and
      `golden-stormbootx-rustnic-757a90efc807d0f1`. **Not run:** option 42
      and the `pool.ntp.org` lookup (host tests only; the stub is named by
      address), and metal. Left for the master: a battery-less X9 blade booted
      on it, after a power cut, should print a `clock : was …, set to …` line.

- [x] **#76 — hand the claimed name and host NQN down to Linux (P0,
      stormblock#249). Closed 2026-10-01.** server8 claimed
      `boothost/server8` here, then its initramfs claimed the X9 chassis
      serial and laid server1's image. stormblock 4c02258 reads two volatile
      EFI variables first. `src/handoff.rs` sets `StormBootTag` (reply host,
      else name claimed; with no claim only a stated tag or DNS name) once
      the claim is decided, and `StormBootHostNqn` after the attach.
      Attributes 0x6, never NV; `universal::handoff_value_ok` gates
      `[A-Za-z0-9._:-]`. Done in 1f81c9e, released **v0.11.0** (b68ff90).
      sc-build: no warnings, suites 9/7/7/7/12, espprobe, both shells, and
      `tests/net-ovmf.sh`: boot 1 (claim 404) set only the NQN; boot 2 claimed
      `default` → `stubhost`, and tcp4probe, started as the attached image's
      `BOOTX64.EFI`, read `StormBootTag = stubhost` and `StormBootHostNqn =
      nqn.2026-09.lo.storm:host-stubhost`, both at attributes 0x6. Goldens
      `golden-stormbootx-e315b41c96aa8fd2` and
      `golden-stormbootx-rustnic-2780838739e86be8`. Left for the master:
      server8 on it (`xxd …/efivars/StormBootTag-ab361f54-…` shows `06 00 00
      00 server8`; initramfs `Machine name from the firmware: server8`).

- [x] **#60 — `startup.nsh` on the media, so an EFI Shell fallback boots
      stormbootx unattended (P0, owner 2026-09-30). Closed 2026-09-30.**
      X9 blades with no UEFI CD boot option drop to Aptio 4's built-in EFI
      Shell (EDK shell, EFI 1.10 mode). `media/startup.nsh` (CRLF on the
      media) sits at the root of the ESP, the ISO9660 tree and the `.img`. It
      loops fs0..fs7 for the volume with `\EFI\BOOT\BOOTX64.EFI` **and**
      `\stormboot\stormboot.conf`, because a local ESP (stormuefi on an
      installed disk, a stale Windows) has the first and never the second.
      `tests/shell-ovmf.sh` boots it under OVMF behind a decoy ESP, in the
      sc-build command. Verified in sc-build: the old EDK shell (`Shell_Full.efi`,
      edk2-stable201811, "Current running mode 1.1.2") and OVMF's Shell 2.2
      each skipped the decoy and started stormbootx from fs1 (and from fs0
      with the CD first). Released **v0.10.0** (2889359); both golden ISOs
      at the tag started from the old shell. Goldens
      `golden-stormbootx-916fd02a32c20f2f` and
      `golden-stormbootx-rustnic-2ba951f3cfd6b9eb`. The metal check (a blade
      that falls to the shell) is the master's. **2026-09-30:** v0.10.0 is
      on forge's minismbd share as `stormbootx-v0.10.0.iso`. The master's
      hot-patch (v0.9.0 plus a simpler startup.nsh) already booted server5
      unattended from the shell fallback; server6–8 are going through on the
      hot-patch, and server1 and server2 get v0.10.0. A shell that stops at
      `Shell>` without running the script will be reported on #60.

- [x] **#65 — rustnic media pins stormnic-ixgbe 563ea8d (P0, owner
      2026-10-01). Closed 2026-10-01.** stormnic-ixgbe#21: on server3 the
      82599 reset's EEMNGCTL.CFG_DONE0 wait timed out (`EEMNGCTL
      0x80000196`) and Start failed. 563ea8d logs it and continues. Pinned
      in 329edfd, built `--locked`. sc-build: suites 9/7/6/7/12, espprobe
      and net-ovmf PASS, rustnic and nic-drivers trees built, and the
      rustnic ISO under OVMF (KVM) printed `media : rustnic ixgbe@563ea8d
      mlx4@cf37f8b` and both drivers' bindings. Goldens
      `golden-stormbootx-rustnic-416b7237c78a29a3` (supersedes #64's) and
      `golden-nic-drivers-4bd5817b92cc`. Left for the master: boot server3
      on it; pass is `EEMNGCTL CFG_DONE0 not set after 1 s …; continuing`,
      then link setup, the DMA check and `Start: bound, SNP on a child
      handle`.

- [x] **#64 — rustnic media pins stormnic-mlx4 v0.2.1 (P0, owner
      2026-10-01). Closed 2026-10-01.** stormnic-mlx4#15: Aptio 4's PCI I/O
      numbers BARs, not BAR registers, so the hard-coded UAR `BarIndex` 2
      was refused and every doorbell failed UNSUPPORTED on server3. v0.2.1
      (cf37f8b) finds the BarIndex through `GetBarAttributes`. Pinned in
      3fe420a, built `--locked`. sc-build: suites 9/7/6/7/12, espprobe and
      net-ovmf PASS, rustnic and nic-drivers trees built, and the rustnic ISO
      under OVMF (KVM) printed `media : rustnic ixgbe@8ea722a mlx4@cf37f8b`,
      `stormnic-mlx4 0.2.1: driver binding installed` and the ixgbe binding.
      Goldens `golden-stormbootx-rustnic-bde9ae3a566c4d7d` (supersedes #63's)
      and `golden-nic-drivers-8cb3943d42f9`. Left for the master: boot
      server3 on it; pass is `UAR BAR (register 2): BarIndex 1, …`, `port 1:
      type Ethernet`, no `doorbell write failed`, `port 1 SNP: initialized`.

- [x] **#63 — rustnic media pins stormnic-ixgbe 8ea722a (P0, owner
      2026-10-01). Closed 2026-10-01.** stormnic-ixgbe#19: on server3
      (X9SRD-F, Aptio 4, 8086:1557) Start failed `could not enable memory
      decode and bus mastering: UNSUPPORTED`. 8ea722a tolerates PciIo
      attribute refusals and sets MSE/BME in the command register directly.
      Pinned in c6bc382, built `--locked`. sc-build: suites 9/7/6/7/12,
      espprobe and net-ovmf PASS, the rustnic and nic-drivers
      trees built, and the rustnic ISO under OVMF (KVM) printed `media :
      rustnic ixgbe@8ea722a mlx4@4c2d318` and `stormnic-ixgbe 0.1.0: driver
      binding installed`. Goldens `golden-stormbootx-rustnic-f32f6e50a16edd5e`
      and `golden-nic-drivers-fcb89ab03ecb`. Left for the master: boot
      server3 on the rustnic golden for the `PCI attributes Get …; command
      0x…` line, then bring-up and `Start: bound, SNP on a child handle`.

- [x] **#56 — stormbootx's own TCP/IP: smoltcp on SNP (P0, owner
      2026-09-30). Verified on metal 2026-09-30; the rest was #68 (closed).** server3 (X9, Aptio 4) loaded
      `ipxe-intelx.efi` and then reported `EFI_TCP4 is not present`. The
      EDK2-stack plan was dropped because since edk2-stable202405 its IPv4
      drivers refuse to start without `EFI_RNG` (and TcpDxe without
      `EFI_HASH2`). The owner's decision replaces it: **no firmware TCP4 on
      any machine**. `src/net.rs` runs smoltcp 0.14 directly on each NIC's SNP,
      opened exclusively. `src/entropy.rs` supplies randomness from the
      firmware `EFI_RNG`, then RDSEED/RDRAND/RNDR, then jitter, and never
      fails (owner's addition).
      **Done and verified in sc-build (5f10680, 2026-09-30):** build with no
      warnings (stormbootx.efi 231 KB), host suites 9/7/6/7/12, espprobe PASS,
      and `tests/net-ovmf.sh` PASS on both boots. Under Fedora's OVMF (no
      MNP/IP4/TCP4, plus the IPv4/IPv6 fw_cfg switches off) each boot leased
      10.0.2.15 from slirp, read the stub engine's 120 KB health reply, did the
      PTR over DNS/TCP, made both claims, attached the stub NVMe/TCP target at
      4096-byte blocks and read the 96 MiB `BOOTX64.EFI` through BlockIO
      (118–121 MiB/s), then started it. Boot 1 printed `rng : firmware`, and
      boot 2 (`rng = cpu`, CPU `-rdrand,-rdseed`) printed `rng : jitter`.
      **Released v0.9.0 (0d063b6)**, sc-build at the tag passing the same
      suite. Goldens: `golden-stormbootx-5fef07c934c75c04` (normal, iPXE
      intelx) and `golden-stormbootx-rustnic-9da8489f09aa7e99`. **Handed
      off:** the master puts the normal golden on forge's minismbd share and
      boots server3. It should print `tcp4 : smoltcp over SNP (…)`, `rng :
      rdrand` (or `firmware`) and a lease, then claim.
      Not seen yet: a multi-NIC machine, iPXE's or stormnic's SNP under
      smoltcp, a 25G link, and `release` handing NICs back to firmware.
      **2026-09-30 ~23:11Z, server3 on v0.9.0:** `tcp4 : smoltcp over SNP
      (nic 0 ac:1f:6b:8a:a4:5c)`, `rng : rdrand`, a lease, the claim, the
      attach, stormuefi and stormcos Ready (iPXE `intelx`'s SNP, one NIC).
      The owner says #56 can close; the four checks above are tracked in #68.

- [ ] **#55 — X9 (AMI Aptio 4) hangs at POST A2 reading the ISO's
      esp.img (P1, 2026-09-30). Handed off.** server1's capture: the
      firmware reads the PVD, the boot catalog and the first 12 KB of
      `/esp.img` (FAT16, 1 sector/cluster, pure El Torito, no partition
      table), then nothing. Debian's netinst boots on the same path: isohybrid
      (MBR + GPT) and a FAT12 efi.img at 4 sectors/cluster. Plan: build the
      ESP at `mkfs.fat`'s default geometry (4 MiB → FAT12, 2 KiB clusters,
      the same as Debian's), and the ISO isohybrid like Debian's
      (`-isohybrid-mbr` + `-isohybrid-gpt-basdat`; dev has no syslinux, so
      the MBR template is 432 zero bytes, since this media has no BIOS boot
      code). `tests/iso-layout.sh` checks both in sc-build; then goldens, and
      the master boots one on server1.

      **Done (2026-09-30):** 978563d, released v0.8.2 (7975b25). sc-build:
      host suites 9/7/6/7/12, espprobe PASS, `iso-layout: PASS` on all three
      ISOs (MBR `0xef` + GPT over `/esp.img`, FAT12, 2 KiB clusters), and
      normal, rustnic and tcp4probe ISOs each booted under OVMF (KVM).
      Goldens `golden-stormbootx-8b33e595281b0079` and
      `golden-stormbootx-rustnic-d741218c58e4ec82`. **Not verified:** Aptio
      4 itself. That is the master's boot on server1 (which reads the ESP
      past 12 KB, then DHCP). A hang there again would rule out the ESP
      geometry and the partition table, and leave what the ESP carries.

- [x] **#47 — rustnic media pins stormnic-ixgbe 9476135. Closed
      2026-09-29.** 9476135 is stormnic-ixgbe#13: PHY and link code matched
      to its `docs/spec/phy.md`, and 25 device IDs. Pinned in 463631c and
      built `--locked`. sc-build built the rustnic golden tree and booted its
      ISO under OVMF (KVM): `media : rustnic ixgbe@9476135` and
      `stormnic-ixgbe 0.1.0: driver binding installed (25 Intel 10G device
      IDs)`. Golden `golden-stormbootx-rustnic-f82a05f6013ea469`. The normal
      media carries no stormnic-ixgbe, so it didn't change. The metal SOL
      checks (stormnic-ixgbe `docs/bring-up.md`) are the master's, on
      server1/server2.

- [x] **#51 — rustnic media pins stormnic-ixgbe 0dd4267 (SNP). Closed
      2026-09-29.** stormnic-ixgbe#4: after bring-up and the DMA check,
      Start installs SNP and a MAC device path on a child handle. Pinned in
      36b22f4, built `--locked`. sc-build built all three trees, passed the
      host suites (8/7/6/7/12) and booted both ISOs under OVMF (KVM): `media
      : rustnic ixgbe@0dd4267 mlx4@4c2d318`, `stormnic-ixgbe 0.1.0: driver
      binding installed (25 Intel 10G device IDs)`, the mlx4 0.2.0 binding;
      the normal media unchanged (`ipxe-intelx.efi` only). Goldens
      `golden-stormbootx-rustnic-6d88515819338a0e` and
      `golden-nic-drivers-56ea4782ef2a`. Left for the master: boot server1
      on the rustnic golden; the SOL should show #3's DMA lines, `SNP
      initialized`, and stormbootx's `tcp4 : available` and claim.

- [x] **#50 — rustnic media pins stormnic-mlx4 v0.2.0 (4c2d318, SNP).
      Closed 2026-09-29.** stormnic-mlx4#4: SNP on a child handle per
      Ethernet port with a MAC device path; `Start` keeps the ConnectX-3
      (~1.5 s per NIC + up to 5 s for link, was ~20 s per port); the #3
      broadcast self-test no longer runs; ExitBootServices stops DMA. Pinned
      in 11469bd, built `--locked`. sc-build built all three trees and booted
      both ISOs under OVMF (KVM): `media : rustnic ixgbe@2afd319
      mlx4@4c2d318`, `stormnic-mlx4 0.2.0: driver binding installed`, the
      ixgbe binding; the normal media unchanged (`ipxe-intelx.efi` only).
      Goldens `golden-stormbootx-rustnic-ab4e848a4dcfcaf3` and
      `golden-nic-drivers-10a25ce0a5a3`. Left for the master: boot server1
      on the rustnic golden for `port 1 SNP: initialized` and `tcp4 :
      available` (and one boot at cef8dc5 if stormnic-mlx4#1–#3's SOL checks
      have not run yet).

- [x] **#48 — rustnic media pins stormnic-ixgbe 2afd319. Closed
      2026-09-29.** 2afd319 is stormnic-ixgbe#3: RX/TX descriptor rings
      with DMA, a broadcast check frame in Start, still no SNP. Pinned in
      c2cbb2a, built `--locked`. sc-build built all three trees and booted
      both ISOs under OVMF (KVM): `media : rustnic ixgbe@2afd319
      mlx4@cef8dc5`, both drivers' `driver binding installed`; the normal
      media unchanged (`ipxe-intelx.efi` only). Goldens
      `golden-stormbootx-rustnic-7f260c307c5ee784` and
      `golden-nic-drivers-60d62cc3aee9`. Left for the master: boot server2
      (or server1) on the rustnic golden for the DMA lines (stormnic-ixgbe
      `docs/rings.md`, "Hardware checks").

- [x] **#34 — stormnic-mlx4 on the rustnic media, carried in nic-drivers.
      Closed 2026-09-29.** Pinned at cef8dc5 (stormnic-mlx4#2 firmware
      bring-up, #3 Ethernet data path), built `--locked`, PE subsystem
      checked = 11 for every Rust driver (302dd25). `STORMNIC_ON_MEDIA` is a
      list; the rustnic media uses `ixgbe mlx4`, the nic-drivers golden
      carries both as `.efi.off`, the normal media carries neither (mlx4's
      Start holds each port ~20 s). sc-build of 29348a5 built all three
      trees and booted both ISOs under OVMF (KVM): `media : rustnic
      ixgbe@9476135 mlx4@cef8dc5`, both `driver binding installed`,
      `stormnic-mlx4.efi started`; normal media `media : normal` with
      `ipxe-intelx.efi` only. That run also found #49 (media-ovmf.sh died on
      a console over 20 lines), fixed. Goldens
      `golden-stormbootx-rustnic-b4f33d9566d6127e` and
      `golden-nic-drivers-5b867545c91e`. Left for the master: boot server1
      on the rustnic golden for stormnic-mlx4#1/#2/#3's SOL lines.

- [ ] **#46 — v0.5.1 media hangs on the R230 inside stormuefi's initramfs
      read (P0, 2026-09-29). In progress.** C2NR0Q2 on golden 206d57c3
      (b693698) stopped at stormuefi's `initrd … raw bytes (no filesystem)`
      for 16+ min; 74a242a6 (a709f9f) booted the same release. That line is
      printed in stormuefi's `handoff::load` *before* the initramfs is read,
      so the stall is the initramfs read through our `read_blocks` over
      NVMe/TCP, not `ExitBootServices`. Bisect by code: the only src commit
      in a709f9f..b693698 on the boot path is 1dbb761 (#37). On the firmware
      path, its one runtime change is `config::esp_reader()`, read after the
      attach (exclusive opens of LoadedImage and the virtual CD's SFS).
      `nvme.rs`, `tcp4.rs` and `read_blocks` are unchanged. `pump` times out,
      so a lost completion fails rather than hangs: the stall looks like reads
      that crawl. One boot per ISO, so the fabric (dsw1, see dswfecfix) is
      not ruled out. Plan:
      1. read `esp =` with the rest of the config, before the network, so
         the firmware path runs a709f9f's sequence exactly;
      2. `read_blocks` reports: a failed read's error text (was a bare
         `DEVICE_ERROR`), a read that took over 2 s, and progress every
         64 MiB with MiB/s, so the next R230 boot shows where the time goes;
      3. sc-build, golden, and the master boots it on the R230.

      **Done (2026-09-29):** 1–2 in 7ee138d, released as v0.7.0 (b36ece9).
      sc-build passes: build with no warnings, host suites 8/7/6/7/12, and
      `espprobe: PASS` under OVMF; `--locked` builds at the tag. Goldens:
      `golden-stormbootx-58415a7fcaed9ac0` (normal) and
      `golden-stormbootx-rustnic-8999c7a5555d95a5`. **Not verified:** the
      `blockio :` lines need an attached namespace (no TCP4 in Fedora's
      OVMF), and the hang itself needs the R230. **Next:** the master boots
      58415a7f on C2NR0Q2. If it boots, 1 was the cause. If it stalls, the
      `blockio :` lines say whether reads crawl (MiB/s, slow reads), fail
      (the NVMe/TCP error), or stopped; check dsw1's dswfecfix log too.

- [ ] **#37 — boot a 4096-byte ESP on firmware whose FAT can't (P0, owner
      2026-09-29). In progress.** Volumes stay 4K. server1's console (#33)
      shows Aptio 4's FAT *mounting* the attached 4K ESP (an SFS was found)
      and then `LoadImage` NOT_FOUND, so the firmware's FAT misreads a
      4096-byte-sector FAT. A 512e shim would still give that driver a BPB
      that says 4096, so it would work only by luck. Chosen instead (the issue's option 2):
      stormbootx reads the ESP itself. Plan:
      1. `src/esp.rs`, core-only like `dnsname.rs`: GPT (CRC-checked) → ESP →
         FAT12/16/32 at any sector size 512..4096, 8.3 and LFN lookup, file
         read. Host tests on `mkfs.fat -S 4096`/`-S 512` images built in the
         sc-build job (`tests/esp-images.sh`).
      2. `blockio::boot_attached`: firmware first (the R230 path), and if
         that load fails, read `\EFI\BOOT\BOOTX64.EFI` with `esp.rs` and
         `LoadImage` it from the buffer. `esp = stormbootx` in
         `stormboot.conf` forces the bridge (the OVMF test), `esp = firmware`
         turns it off. stormuefi needs nothing: it reads pallets through
         whole-disk BlockIO and parses the GPT itself.
      3. `espprobe.efi` (third binary) runs the same bridge on a local 4K
         virtio disk under OVMF in the sc-build job, to prove LoadImage
         from the buffer starts an image.
      4. Metal: server1 and a pve VM boot a 4K clone (the master runs them).

      **Done and verified in sc-build (2026-09-29):** 1–3. `esp.rs`'s 12
      host tests pass: FAT12/16/32, 512 and 4096 sectors on 512 and 4096
      disks, long names, a fragmented file, CRC failures. Under OVMF (KVM),
      espprobe read the payload off a FAT16 at 4096-byte sectors on a
      4096-byte virtio disk, and LoadImage started it from the buffer. OVMF's
      own FAT loads that ESP too, so pve's `No bootable option` (stormblock#228)
      is not EDK2's FAT. **Left:** 4, on metal. `blockio.rs`'s own half (the
      firmware-first order and the NVMe namespace adapter `NsDisk`) has not
      run anywhere yet: it needs an attached namespace, and Fedora's OVMF
      has no TCP4.

      **Golden:** `golden-stormbootx-206d57c3601dcd17` (v0.5.1, b693698). The
      first request failed: `build-nic-drivers.sh` fetched the private
      stormnic-ixgbe, and the golden builder has no GitHub credentials. The
      media golden never carried that driver, so it no longer builds it
      (2fdd994). The nic-drivers golden still does, which is stormcentral#189.
      **Next:** the master boots that golden on server1 and a pve VM with a
      4K clone; expect `boot : the firmware did not load it (…); reading the
      ESP here` on server1, then stormuefi. **Handed off 2026-09-29**
      (`stormcentral shipped`): nothing is left in this repo, and the golden
      is current (only docs since b693698). The metal run closes it; a
      failure there comes back as a new issue or a reopen.
      **2026-09-30 ~21:37Z:** server1 (X9SRD-F, Aptio 4) booted release
      11.56 from a 4096-byte namespace on v0.7.0 media, once 11.56's ESP was
      FAT16 (stormcos#188; 11.50's FAT32-labelled ESP of 16,384 clusters was
      what Aptio misread, and #67 is `esp.rs` reading one as Linux does).
      Which reader loaded `BOOTX64.EFI` waits on the SOL capture
      (stormcentral#220, #33).

- [x] **#45 — two media goldens, served side by side (P0, owner
      2026-09-29). Closed 2026-09-29.** `stormbootx` (normal: iPXE `intelx`) and
      `stormbootx-rustnic` (the Rust `stormnic-ixgbe.efi`, no iPXE NIC driver),
      both from `deploy/build-golden.sh` at one commit. Folds in #44 and #43
      (ixgbe pin → 884cf18, owner's choice on #44, built `--locked`). Plan:
      1. `build-nic-drivers.sh`: pin 884cf18; skip iPXE entirely when no iPXE
         driver is wanted (no `IPXE-SOURCE.txt` then).
      2. `media = <label>` in `stormboot.conf` (`build-boot-agent.sh --media`),
         printed as `media : normal` / `media : rustnic ixgbe@884cf18`.
      3. `build-golden.sh stormbootx-rustnic OUT`: `bin/stormbootx.efi`,
         `boot/stormbootx-rustnic.iso` (the path stormcentral's media recipe
         reads), built its own drivers (no `nic-drivers` input).
      4. `stormcentral component add stormbootx-rustnic --kind media`, then
         both goldens. mlx4 joins rustnic once it binds (owner); until then
         it stays off (#34).
      Serving from minismbd and pointing BMCs is the master's (step 3 of the
      issue).

      **Done and verified in sc-build (e0c79d5, 2026-09-29):** 1–3. All three
      trees build at one commit; `bin/stormbootx.efi` is byte-identical in
      both media; the rustnic ESP carries `stormnic-ixgbe.efi` +
      `STORMNIC-SOURCE.txt` (884cf18, `locked`) and no iPXE; the normal ESP
      `ipxe-intelx.efi`. `tests/media-ovmf.sh` booted each ISO under OVMF
      (KVM) and found `media : rustnic ixgbe@884cf18` / `media : normal`
      under the banner, and `stormnic-ixgbe 0.1.0: driver binding installed`
      on the rustnic boot. `stormbootx-rustnic` is registered (kind `media`,
      no inputs). **Goldens (v0.6.0, fdeb0b7):**
      `golden-stormbootx-8732577aa6bda0ce` (normal, on
      `golden-nic-drivers-72fe4f89174d`) and
      `golden-stormbootx-rustnic-0e449102ceb5882d`. Their `stormbootx.efi`
      digests differ: separate build dirs, and the binary is not yet
      reproducible across them (same dir → identical, as above). Left for
      the master: serve both from minismbd, point BMCs (server2 on rustnic
      for stormnic-ixgbe#2, #44). #43 and #44 are folded in.

- [ ] **Everything is a golden (owner, 2026-09-28; #21 decided,
      stormcentral#126). In progress.** stormbootx ships as a golden, not as
      a file on dev: the boot media (`.efi`, ESP image, ISO) as one golden,
      the NIC drivers as their own bin golden. The owner has asked
      stormcentral to make stormbootx a component; its generic `binaries`
      build is musl-only, so stormcentral needs a build path for it. This
      repo's side: `deploy/build-golden.sh <golden> OUT` compiles and writes
      that golden's tree into OUT and nothing else (no platform work,
      stormcentral#128), for stormcentral to call into the output it mounts.
      The media scripts default to `tmp/` in the checkout, never
      `/build/images`. **Nothing is left on dev**: every build gets a fresh
      drive that is deleted, and anything outside it is deleted too. The
      7fd80dd ISO that was put on dev for server1 was removed (2026-09-28).
      **This repo's side is done (d10f734, sc-build 119 s):** both trees
      build; the ISO's `BOOTX64.EFI` is byte-identical to `bin/stormbootx.efi`
      and its ESP carries the driver and `stormboot.conf`. **Registered:**
      `stormbootx` and `stormbootx-rustnic` are stormcentral components of
      kind `media` (stormcentral#153's `media` kind); `stormcentral component
      build <name>` builds a drive golden whose bytes are the ISO, and every
      release since v0.5.1 has had one. `nic-drivers` is built the same way
      (stormcentral#189 for its private-repo fetch). Left: #41 (a USB `.img`
      golden), #35 (whether the media golden keeps the `.img`, `bin/` and
      `tcp4probe.iso` it builds), #52 (a third, firmware-drivers-only media).

- [x] #26 — **NIC UEFI drivers from the boot media. Closed 2026-09-28**
      on metal: server1 (Supermicro X9, AMI Aptio 4) booted
      `golden-stormbootx-74a242a6f0e89f75` (a709f9f): `drivers : 1 of 1
      started` (iPXE `intelx`, the owner-approved interim; hermon is opt-in,
      #30), `tcp4 : available`, name `server1a.g16.lo` → `server1` from the
      PTR, claim `boothost/server1` (the master created it on forge), and an
      NVMe/TCP attach (nsid 77, 32 GiB at 4096-byte blocks) with no
      `CreateEvent` failure. What stops the boot after that is #33:
      `BOOTX64.EFI` `NOT_FOUND` on the 4096-byte namespace, fixed on the
      engine side (stormblock#228). The Rust drivers replacing iPXE are
      #27/#29.

- [x] #13 — presentation at `docs/presentation.md` (Marp, 13 slides). Closed
      2026-09-28: re-checked against HEAD (cb326b0) and rewritten where it
      drifted; sc-build built the binary (152,064 bytes, as the title says),
      passed all four host suites and rendered the deck with marp-cli.
      Slide 2 matches `stormcentral check`. Keep it current with the code.

- [ ] #10 — extract `nvme.rs` (and the claim) into a transport-generic
      `no_std` crate. Prerequisite for stormboot4bios.

- [x] **#83 — self-update of the boot media (USB stick / local ESP) from
      the current golden (P2, owner 2026-10-02). Closed 2026-10-02.** The
      owner's design: stormcentral serves `…/boothelpers/<golden>/current`
      (a signed text manifest), `current.sig` and `files/<path>`
      (stormcentral#279; the contract is posted there); Ed25519 keys compiled
      in; serial never down; A/B with a trial that must reach an attach.
      `src/manifest.rs` (core-only, 10 host tests), `src/selfupdate.rs`,
      `local.conf`, `build-boot-agent.sh --update/--tree`, goldens carry
      `media/` + `media.files`. Released **v0.13.0** (6a74b55). sc-build: no
      warnings, suites 9/7/7/8/12/8/10, espprobe, net-ovmf (the ISO skips its
      `update =` as read-only), both shells, and `tests/update-ovmf.sh`'s six
      boots (bad signature refused; serial 5 written, restarted, trial
      passed on the attach, `StormBootUpdate = good:5`; current; serial 6
      with a dead NVMe port fails two starts and the third puts serial 5
      back and chain-loads it, `failed:6`; disk checked with mtools). Goldens
      `golden-stormbootx-190ef2877df1330d` and
      `golden-stormbootx-rustnic-3cc3ee69c819e68d`. **Left: #86**,
      `RELEASE_KEYS` is empty until stormcentral#279 makes its key, so no
      medium updates yet (proposed after stormcentral#279). Not run: a real
      stick on metal, canary lines, a driver retired, a medium short of room.

- [x] **#86 — compile stormcentral's release key into `RELEASE_KEYS` (P1,
      2026-10-02). Closed 2026-10-02.** stormcentral#279 made the key
      (stormcentral 7b42262); public half `4fe28c02…0a171bce`, as posted on
      #86 and served open at `/api/v1/stormbootx/keys` (checked to match).
      `manifest::RELEASE_KEYS` (moved from `selfupdate.rs` so the host test
      `the_release_key_is_stormcentrals` holds it to that hex). e973cfb, the
      test's key count 559197b (#87), released **v0.14.0** (7e9810f).
      sc-build at the tag with `--locked`: no warnings, suites
      9/7/7/8/12/8/11, espprobe, net-ovmf, both shells, and update-ovmf's six
      boots, the bad-signature one checking against `2 compiled-in key(s)`
      (release + test). Goldens `golden-stormbootx-fe6117d741ddab41` and
      `golden-stormbootx-rustnic-75128a00457dab51`. **Not run:** a manifest
      signed by stormcentral's own key: nothing is promoted yet
      (`…/boothelpers/*/current` 404). The first `stormcentral stormbootx
      promote` and a writable stick on the v0.14.0 medium are the master's;
      pass is `update : -> v… (golden-…, serial 1)`. Only media at v0.14.0 or
      later can take an update: older sticks must be rewritten once.

- [x] **#66 — rustnic media pins stormnic-mlx4 v0.2.3 (0e50017) (P1).
      Closed 2026-10-06.** The issue asked for v0.2.2 (link diagnostics,
      stormnic-mlx4#15); its later comment v0.2.3, which adds the loop-back
      log line (stormnic-mlx4#21). Pinned in 77b9d3b. sc-build: no warnings,
      suites 9/7/7/8/12/8/11/7, espprobe, net-ovmf, both shells, update-ovmf,
      the nic-drivers and rustnic trees (`STORMNIC-SOURCE.txt` mlx4 0e50017
      `locked`, 81920 bytes, subsystem 11), rustnic ISO `iso-layout` PASS, and
      under OVMF `media : rustnic ixgbe@563ea8d mlx4@0e50017`, `stormnic-mlx4
      0.2.3: driver binding installed` and the ixgbe binding. Goldens
      `golden-stormbootx-rustnic-0b0aba5038360c60` and
      `golden-nic-drivers-9f614d980f62`; the fw medium carries no drivers and
      is unchanged. **Left for the master:** an X9 blade (server3) on the
      rustnic golden: per port, the speed/autoneg/module lines at the link
      wait (match against dellsw#14) and the loop-back line
      (stormnic-mlx4#21).

- [ ] **#89 — the self-update's unseen paths (P2, from #83/#86). OVMF
      half done 2026-10-06; the metal half waits on #96.** stormcentral serves
      a real promotion: `stormbootx-rustnic` serial 1 = v0.14.0
      (`golden-stormbootx-rustnic-75128a00457dab51`, hex signature);
      `stormbootx` (fw) still 404. 46048c0, `tests/update-boots.sh`, ten
      boots with the medium on a USB stick (`qemu-xhci` + `usb-storage`):
      stormcentral's captured manifest (`tests/fixtures/`) verified in the
      binary against the release key and went on to the file fetch (`…/files/
      EFI/BOOT/BOOTX64.EFI answered HTTP 404`, nothing written); serial 6
      refused as another MAC's canary (`is for its 1 canaries only`), then
      taken naming this MAC, `1 driver(s) retired` with `retire.efi.prev` on
      the disk; serial 7 `needs 4467 KB and the medium has 3727 KB free;
      nothing written`; serial 8's failed trial put serial 6 back. A non-PE
      `.efi` in `\stormboot\drivers` is `LoadImage: UNSUPPORTED` and the
      boot goes on. sc-build of 46048c0: no warnings, suites
      9/7/7/8/12/8/11/7, espprobe, net-ovmf, both shells, update-ovmf PASS.
      **Left:** a real stick on metal taking a real promotion end to end.
      Blocked on #96: today's serial 1 (v0.14.0) would take any v0.15+
      writable stick backwards. There is no writable golden either (#41/#97).
      Pre-v0.14.0 media seen in the latest consoles: server8 rustnic v0.10.0
      and server3 rustnic v0.12.0. server4 is on v0.14.0. Whether those are
      sticks or BMC media is not in the console.

- [x] **#90 — the claim names the boot agent (P2, stormcentral#286).
      Closed 2026-10-06.** Every claim body carries `"agent":{"name":
      "stormbootx","version","commit","media","update_serial","update"}`
      beside `{mac, serial}`. That is the channel the owner chose on #20 for
      firmware data ("with the claim to the boothost record"). The engine's
      `ClaimRequest` ignores unknown fields (v13.7.0 on forge and v20.0.0
      both), so it costs nothing until stormblock#177 records it (shape posted
      there). `selfupdate::medium_serial()`, `handoff::update_value()`,
      `registry::agent()`. bc1c83b, released **v0.20.0** (8f51155). sc-build
      of bc1c83b: no warnings, suites 9/7/7/8/12/8/11/7, espprobe, both shells,
      net-ovmf (`{"agent":{"name":"stormbootx","version":"0.19.0",…,"media":
      "agent test"}` on every claim), update-ovmf (`"commit":"update-test"}`
      on a fresh medium, `"update_serial":5,"update":"trial:5:1"}` on the
      trial, `serial:5` when current, `"update_serial":6,"update":"failed:8"}`
      after the revert). The tag built `--locked` and net-ovmf showed
      `0.20.0`. Goldens `golden-stormbootx-feb820e885a03a63` and
      `golden-stormbootx-rustnic-b01e86a2fbfca2ba`. **Left elsewhere:**
      stormblock#177 keeps it; stormcentral#286 reads it; stormcos#219 is the
      Linux half.

### Blocked on other repos

- [ ] **#75 — stormnic-virtio, a Rust virtio-net UEFI driver (P2, owner).
      Waiting on the owner (2026-10-06, `needs-owner`).** The repo doesn't
      exist, and this session can't make it a project: `project add` is
      admin-only, and `project spawn` needs a family with spawn on (only
      `nana`; from here it would make `stormbootx-virtio`). Asked on #75:
      (a, recommended) the owner/master creates `glennswest/stormnic-virtio`
      and registers it like stormnic-ixgbe/mlx4, and its own session writes
      the driver; (b) a `stormnic` family with spawn; (c) this session
      creates the repo itself. The PCI IDs are confirmed in pci.ids (1af4:1000
      transitional, 1af4:1041 modern). This repo's side, once the driver
      binds: pin it in `build-nic-drivers.sh` (the nic-drivers golden and
      the rustnic media), then OVMF and pvetest1/2. #69–#74 have the same
      question.

- [ ] **#69 — stormnic-igb, a Rust i350/i210/i211/82576/82580 UEFI driver
      (P2, owner). Waiting on the owner (2026-10-06, `needs-owner`)** for one
      thing: the master creating `glennswest/stormnic-igb` (as for
      stormnic-virtio, #75). Verification needs no test machine. dev's QEMU
      10.1.5 has `-device igb` (82576, `8086:10c9`), and OVMF has no igb
      driver of its own, so an sc-build OVMF boot is a clean check. (It also
      has `-device e1000e`, for #70.) IDs: the issue's are right except
      `1534`/`1535`, which are not in pci.ids; the family adds 82575EB, more
      82576/82580 variants, I354 and DH8900CC. This repo's side, once it
      binds: pin it in `build-nic-drivers.sh` and add an OVMF boot on
      `-device igb`.

- [ ] **#71 — stormnic-i40e, a Rust X710/XL710/XXV710/X722 UEFI driver
      (P2, owner). Waiting on the owner (2026-10-06, `needs-owner`).** Same
      pair of questions as #74: the master creates `glennswest/stormnic-i40e`
      (as for stormnic-virtio, #75), and which machine to verify on. QEMU
      emulates no i40e part, and no registered test machine has one (the
      consoles show only ixgbe, mlx4, mlx5 and tg3). The issue's IDs are
      right in pci.ids; the PF family adds 1580/1581/1585–1588/158a,
      104e/104f, 101f and 37ce/37cf/37d4. This repo's side, once the driver
      binds: pin it in `build-nic-drivers.sh`.

- [ ] **#74 — stormnic-realtek, a Rust RTL8111/8168, 8125, 8126 UEFI
      driver (P2, owner). Waiting on the owner (2026-10-06, `needs-owner`).**
      The repo doesn't exist. On #75 the owner agreed that the master creates
      each `stormnic-*` repo and registers it with its own session. Asked on
      #74: do the same for `glennswest/stormnic-realtek`, and which machine
      with a Realtek port to verify on. QEMU emulates only rtl8139, so no VM
      (OVMF, pvetest1/2) can bind it, and no registered test machine is known
      to have one. IDs checked in pci.ids: 8168, 8125, 8126 right; 8161, 8127
      and Killer 2600/3000/5000 are the same family. This repo's side, once
      the driver binds: pin it in `build-nic-drivers.sh` (nic-drivers golden,
      rustnic media).

- [x] **#68 — smoltcp (#56) on metal: the rest (P2). Closed 2026-10-06.**
      From the consoles stormcentral keeps
      (`/var/lib/stormcentral/console/`): **25G and two live links**: the
      R230 (C2NR0Q2), on every boot since 2026-10-03 (rustnic media, v0.14.0),
      opens four SNPs. nic 0/3 are the ConnectX-4 Lx 25G ports (`5c:06:22`,
      mlx5 in Linux), both `link UP`; nic 6/9 are the tg3 1G ports, down.
      nic 0 ranked first, `reached 192.168.31.202:9090`, claimed and attached,
      and nic 3 leased too. **stormnic SNP** (ixgbe, mlx4) and **a down NIC
      skipped** were seen on server3/server1 earlier. **`net::release`** now
      prints `net : N of M NIC(s) given back …` (033e85f),
      and `tests/net-ovmf.sh`'s fifth boot, `release`, falls through with the
      firmware's IPv4 stack on (plus a virtio-rng). The firmware's `UEFI
      PXEv4` on the same NIC then leased and started tcp4probe over TFTP.
      Released **v0.21.0** (96d72d0, built `--locked`). Goldens
      `golden-stormbootx-125a23abbe7b5a47` and
      `golden-stormbootx-rustnic-16d7fcd957f5c8ed`. **Not seen:** the MTU tiebreak between two live
      links (every firmware SNP here reports 1500), the 25G link-settle wait
      (the links were up at the first sample), and the release line on a
      metal fall-through.

- [ ] **#92 — promote the install-config-capable media (P1). Blocked on
      stormcentral#459 (2026-10-06).** Promotion (`stormcentral stormbootx
      promote … --booted <pve> --booted <metal>`) needs a passed install on
      each, booted *with* the golden. pvetest1/2 boot a v0.15.0 rustnic ISO
      (pvetest1 passed 11.88-flowsdn), but no hardware test machine has ever
      booted ≥ 0.15.0: the console banners in stormcentral's install logs
      read `0.14.0 (7e9810f)` on C2NR0Q2 (iDRAC) and `0.10.0` on server8.
      `check_booted` compares only times, so C2NR0Q2's 11.88 pass would be
      accepted while proving nothing (stormcentral#286). Asked on
      stormcentral#459: mount the **v0.17.0** rustnic golden
      (`golden-stormbootx-rustnic-4207d13a085449f2`; v0.15.0 lacks #54's
      fix) on a pve VM and C2NR0Q2, install, check the banner, promote as
      serial 2. That also ends #96's downgrade for v0.15–v0.17 sticks.
      Nothing in this repo changes for it.

- [ ] #14 — test containers (short/medium/long). **Waiting on an owner
      decision, 2026-09-27.** stormbootx only runs in firmware, so a real test
      boots the `.efi` in a UEFI VM. What exists: the runner
      (stormcentral `testruns.rs`) gives a Job no token, no privileged mode,
      no `/dev/kvm` and reads no `requires:`; the node's stormblock needs a
      token for everything but the claim (stormcos#89); stormvm boots UEFI
      only from a golden/volume and its serial console is unproven live, and
      its OVMF is not known to carry TCP4. stormblock's own test runs a
      private engine on loopback, which is the pattern a self-contained test
      would copy. Decisions needed: where the engine comes from, where a
      TCP4-capable OVMF comes from, and whether TCG (no KVM) is acceptable.

- [ ] #15 — **universal boot (P0, owner 2026-09-27). The stormbootx side is
      done (2026-09-27, sc-build passing).** One ISO boots any machine with
      no tag: with no `tag =`, it claims `boothost/default` carrying
      `{"mac", "serial"}` and the engine (stormblock#200, on stormblock main
      after v19.3.0) gives it its own CoW clone as host `mac-<hex>`. The
      console says `booting the default image as mac-<hex>`, or `booting
      <name>'s image` once the MAC is an alias. Pieces: `src/universal.rs`
      (version gate strictly after 19.3.0; lowest usable MAC; the reply's
      `host`, tested against the engine's sorted-key reply),
      `tcp4::machine_mac`, `registry::{engine_version, claim_default}`, and
      step 2a/3 in `run()`. Older engines or an unreadable version claim the
      serial as before; the MAC is now #7's floor for a board with no
      serial. A #200 engine's 404 (no default) retries by serial, which
      can then only find an existing host. Host NQN = the engine's host name.
      **Still open on (none in this repo):**
      - a stormblock **golden** containing #200, and forge on it. #200 is
        released in **v19.4.0** (6af6ad3); its claim reply and
        `ClaimRequest` were re-read at that tag and match `universal.rs`'s
        tests, and 19.4.0 passes the gate. But the newest stormblock golden
        is `stormblock@e00a1b0`, 16 commits before v19.4.0, and forge ran
        **13.7.0** (`/api/v1/health`, re-checked 2026-09-27);
      - stormcentral#29 / stormipmi: something sets `boothost/default`;
      - stormblock#202, the `serial` hint, so a machine the
        engine already knows by serial (C2NR0Q2) keeps its host instead of
        becoming `mac-<hex>` — until then, alias its MAC to it (#199);
      - a metal check: two machines, one ISO, no tag, two clones.

- [x] #23 — **identity from DNS (P1, owner 2026-09-27). The stormbootx side
      is done (2026-09-27, sc-build passing).** With no `name =`/`tag =`, a
      machine claims `boothost/<first label of its DNS name>`: DHCP option
      12 (+15 for the console) from the lease of **the interface that reached
      the engine**, else the PTR of that address over **DNS/TCP** to the
      option-6 server (microdns answers over TCP; checked on g8 and g10).
      Then #15's default-by-MAC, then the serial; a 404 moves down the list.
      Pieces: `src/dnsname.rs` (core-only, 6 host tests on captured microdns
      replies), `dhcp4::reply_for`, `Tcp4Socket::interface`,
      `registry::claim_boothost` with `{mac, serial}` and a status on error,
      `network_name`/`ptr_lookup` and step 3b/3c in `run()`. **Still open on:**
      - stormblock#204: an unknown name claimed with a known MAC/serial must
        reach (and name) that host, not mint a new one from the default.
        Until then, rename on the engine before the reservation's hostname
        reaches the machine (C2NR0Q2 → `stormblock1`, #199).
      - the issue's test on real machines: a microdns reservation `serverN`
        claims `boothost/serverN`, and two shared-serial MicroCloud nodes get
        different images. Not run: no VM or metal harness here (#14/#22).
        That `EFI_DHCP4.GetModeData` on a fresh child shows the platform's
        own lease is EDK2 behaviour; on AMI Aptio 4 a fresh child returned
        no reply (#23's comments), which is why 648e366 keeps its own lease's
        reply. Since #56 the reply is smoltcp's, kept per NIC. **#23 is
        closed**; stormblock#204 is the engine's.
      **2026-09-28:** server1's first boot (#26, ISO 97045e0) printed `name :
      none` and claimed the shared chassis serial. 648e366 is the fix: the
      reply of a lease stormbootx ran is kept, microdns gives no option 12 so
      the PTR is the path (`dns =` when the reply names no server), the NIC
      name maps to the machine (`server1a` → `server1`), and the chassis
      serial is rejected. sc-build of 648e366 passes (build, 7+7+6+7 host
      tests). **Next evidence is server1 booting an ISO of ≥ 648e366**
      (master's build, #26; its first attempt died on the iPXE step, #28),
      which should print `name : server1a names this NIC; the machine is
      server1` and claim `boothost/server1`. Forge still reports 13.7.0.

- [ ] #11 — per-machine boot intent. **The stormbootx half landed on
      2026-09-27** (`src/intent.rs`, `registry::boot_intent`, step 3a in
      `run()`). It reads `GET /api/v1/synonyms/boothost/<tag>/intent` before
      the claim. `local` falls through with no claim and no clone. `install`
      and `auto` claim as before. Every doubt (404, non-2xx, unreachable, no
      intent or an unknown one) reads as `auto`. It's verified by sc-build: the
      binary builds and `intent.rs`'s 7 host tests pass. **Still open on:**
      - **stormblock#148**: the engine route, the one-shot `install` → `local`
        reset, and carrying `install` in the claim reply for the initramfs.
        **Released in stormblock v20.0.0** (0e3c47b, tagged 2026-09-28);
        forge still on 13.7.0 (re-checked 2026-10-02). GET is open and resolves names and
        aliases through `host_of`; the reply is `{host, intent, updated_at}`
        (+`resolved_from`); PUT is admin; `POST …/installed {volume}` resets
        `install` → `local` (the node's OS sends it, not this binary); the
        claim reply carries `intent`. Until forge runs it every read is a
        404 and every boot is `auto`, exactly as before.
      - **Done 2026-09-28 (sc-build passing, intent 8/8):** the intent is
        read down the same name list the claim uses (DNS name → MAC →
        serial/tag; a stated tag alone), moving on only on a 404. A `local`
        set on a host the engine knows by its MAC was missed whenever the
        DNS name was not yet a host or alias (stormblock#204).
        `the_engines_own_reply` tests 0e3c47b's `intent_body` shape.
        **Left for the close:** forge on stormblock v20 (#148),
        then one metal boot with `local` set that prints `nothing claimed`.
      - ~~**#3**~~: done in v0.8.0, then overridden in v0.8.1 (owner,
        2026-09-30): `auto` claims, and the local-ESP rule runs only with
        `local_when_bootable = true`. stormcos#30's marker is not needed.
      - Not yet seen on metal: nothing can serve `local` until #148 lands.

- [x] #3 — **owner override, 2026-09-30 (later the same day): until
      intents work on forge v20 (stormblock#148), always claim.** `auto` and
      every doubt claim, as before v0.8.0; only `local` stays local. The
      local-ESP rule stays in the code, off by default:
      `local_when_bootable = true` in `stormboot.conf` turns it on (no media
      script writes it). Done in 2a32afb, released v0.8.1 (ab28481); sc-build
      clean, suites 9/7/6/7/12, espprobe PASS under OVMF. Goldens
      `golden-stormbootx-7d8b3de0e246002a` and
      `golden-stormbootx-rustnic-bc483f74f381eaad` supersede v0.8.0's.
- [x] #3 (the rest) — **answered 2026-09-30 (owner, via the master): built
      in 086c046, released v0.8.0 (d65157a). Closed 2026-09-30.** sc-build:
      no warnings, host suites 9/7/6/7/12 (`the_owners_rule`,
      `doubt_boots_a_bootable_disk_and_claims_on_a_bare_one`), and under
      OVMF espprobe's `local : ESP partition 1, FAT16, …` (`espboot::find`,
      what `auto` asks) agreed with the full read. Goldens
      `golden-stormbootx-4dad2bc3ecdd68bb` and
      `golden-stormbootx-rustnic-6cd745d27f0d0fbd`. **Not run:** `run()`'s
      step 3a itself (needs TCP4, which Fedora's OVMF lacks); the master's
      next metal boot shows `local : …`. Note C2NR0Q2's stale Windows SAS
      disk, if GPT, likely has a BOOTX64.EFI, and `auto` would boot it there: set `install`,
      or wipe that disk. Was: `install` claims; `local` boots the
      local disk; `auto`, a 404 or any other doubt boots the local disk only if
      a local, non-removable disk other than the boot media carries an ESP with
      `\EFI\BOOT\BOOTX64.EFI` (read with `esp.rs`, no network), else claims
      as today. Unreachable engine: local, as now. Also answers #31. Plan:
      1. `intent::decide(intent, local_bootable) -> Action` (core-only, host
         tests);
      2. `blockio::local_bootloader()`: every whole, present, non-removable
         disk except the one this image was loaded from, `espboot::find` on
         it (GPT → ESP → FAT → the file, not read);
      3. step 3a in `run()` probes only when the intent is `auto`;
      4. docs (README, intent.rs, main.rs), CHANGELOG, sc-build, golden.
      Earlier: **re-scoped by the owner on #19 (2026-09-29), then
      `needs-owner`.** No golden comparison, and a new golden never installs
      by itself. At every boot one question: was an install requested (the
      intent record, stormblock#148, in stormblock v20.0.0)? Yes → install;
      no → the local disk. stormcos#30's marker is no longer needed. **Open
      question (posted on #3, 2026-09-29):** what "no request" means for a
      machine with nothing bootable locally, and whether a 404/unknown
      MAC/forge on 13.7.0 counts as "no". Recommended: `auto` and every
      doubt boot local only if a local ESP carries `\EFI\BOOT\BOOTX64.EFI`
      (checked with `esp.rs`, no network), else claim as today. No code
      until answered. That answer also settles #31. The history below
      predates the re-scope.
      Previously: **blocked, re-checked 2026-09-24 (now P0).** Three
      things, none of them in this repo:
      1. ~~**stormblock#123**~~, closed 2026-09-24 in stormblock v16.2.0: the
         flow-over now lays an ESP (stormuefi) and kernel pallets, so an
         installed disk boots on its own. It has been verified under OVMF but
         not yet on metal.
      2. **stormcos#30** — nothing writes an installed marker yet, and its
         natural home is the ESP that #123 adds.
      3. **The compare key** — an owner decision, #19, see below.
         **Superseded: #19 was decided 2026-09-29 (no golden comparison;
         install only on request) and is closed.** Was, 2026-09-27: still
         unanswered, so no code. New
         since: on stormblock ≥ 17 the `GET boothost/<tag>` below needs a
         token (only the claim and `/api/v1/health` are open), so option 2
         also needs an engine change: e.g. the intended golden's key in the
         `…/intent` reply (stormblock#148). Recorded on #19. With #15 the
         key is read under the MAC-resolved name, not the serial.

      The owner's rule on #11 (2026-09-24) supersedes the issue's "different →
      reinstall": a node boots **local** unless there is a new golden **and**
      an install was requested. So #3 is the "is there a new golden?" half and
      #11 the "was it requested?" half; neither reinstalls on its own.
      **Superseded (history only):** #19's decision dropped the golden
      comparison, and the owner's 2026-09-28 reply on stormblock#148 wants a
      fresh clone on **every** network boot (#31, #32), so ending the
      per-boot clone is not a goal of #3 or #11.

      Found while re-checking: the intended golden is answerable **without a
      claim**. `GET /api/v1/synonyms/boothost/<tag>` returns the synonym's
      target volume, `version` and free-form `label` and mints nothing, where
      the issue's plan (attach, read `version_label` via
      `stormblock-pallet-format`) needs a claim — a fresh clone every boot,
      the churn #11 and stormblock#119 want gone. Using the GET means the
      marker has to carry the same key (e.g. the initramfs's
      `claimed_from.volume`), which is the contract stormcos#30 has to agree.
- [ ] #4 (the registration half) — reporting this machine's inventory back.
      Everything wanted is reachable before any OS: MACs from
      `EFI_SIMPLE_NETWORK`, memory/CPU/chassis from SMBIOS types 17/16/4/3,
      storage from `EFI_BLOCK_IO`, controllers from `EFI_PCI_IO` class `0x01`.
      Two constraints: collect it **before** `blockio::publish`, or the machine
      reports the namespace it just attached as its own hardware; and `BLOCK_IO`
      only shows what firmware bound a driver for, so the PCI scan is needed as
      well as, not instead of. Blocked on the payload shape. **P3
      (2026-09-27 validation):** stormipmi now reads CPU and memory over
      Redfish for machines with a BMC, and the report's home is stormipmi's
      machine record or stormblock#177's claim record, not a `BootHost`
      (stormnetboot#8 is no longer on the path). Whether firmware
      registration is still wanted beyond machines without a BMC is an
      owner call.
- [ ] #2 — self-update of the boot media (P3). **Superseded by #83
      (v0.13.0).** stormbootx has no golden and
      nothing publishes `stormbootx.efi`, so there is not yet an artifact for a
      controlled digest to name. SHA-256 is done; what is left is
      gated on #4, because updating to "whatever was on the last image
      attached" is exactly the uncontrolled update this must not become. The
      next piece that needs no one else is hashing a file through
      `EFI_FILE_PROTOCOL` a buffer at a time — the streaming API is already
      shaped for it.

## Status

v0.21.0. **First complete NVMe/TCP attach on real hardware: 2026-09-05**
(and the same day, the full chain: chain-load into stormuefi and a running
stormcos kernel), on a
Dell PowerEdge R230 (service tag C2NR0Q2) booting the agent over iDRAC virtual
media, attaching a 32 GiB clone from forge over a 25 GbE Mellanox port:

```
service tag : C2NR0Q2
tcp4        : available
claim       : boothost/C2NR0Q2 at 192.168.31.202:9090
    interface 0 answered (MTU 1500, link up)
  claimed a clone of this machine's image
  portal    : 192.168.31.202:4420  nsid 7
  namespace : 8388608 blocks x 4096 bytes  (32 GiB)
  transfer  : 128 KiB per command  (controller MDTS 5; path MTU 1500)
blockio     : published on handle 0x8301ae98
RESULT: remote image is a local disk. Firmware can boot it.
```

Every fix in v0.3.0 confirmed on metal in that one boot: the 4096-byte LBA read
from FLBAS (not assumed 512), the MDTS-derived 128 KiB transfer (not the
inverted 8 KiB), multi-NIC selection taking the live 25G port over a
`handles.first()` guess, and the service-tag claim end to end.

### The bring-up, and what each wall was

Three days, and every failure was real hardware or infrastructure, not the
binary — but each one first looked like a stormbootx bug. One of the fixes
was itself wrong: item 4's `fec off` on the switch got the link up that day
and broke the fabric later (see the `#7` correction). The rest held.

1. `EFI_TCP4 is not present` — the UEFI network stack was disabled in setup.
   Enabling it fixed the onboard ports; the console `tcp4` line and #5's
   ConnectController-with-wait were what made it legible.
2. `NO_MAPPING` after 20 s — assumed missing DHCP, was really `handles.first()`
   landing on a NIC with no cable. Fixed by trying every interface, fastest by
   MTU first.
3. Only 2 of 4 NICs present — `GlobalSlotDriverDisable = Enabled` in BIOS
   disabled every slot's option ROM, so both add-in cards were invisible to
   firmware. Found via the iDRAC SCP export, not a boot.
4. All 4 NICs present but 25G ports `link down` — a FEC mismatch, and the
   conclusion drawn here was **wrong and is the breaking change**. The A/B at
   the time (`fec off` linked, `cl108` stayed dark) was read as "the Mellanox
   wants no FEC", and `fec off` was set on all 48 SFP28 ports of dsw1. That
   is what later left ports line-protocol-down for days with light present and
   produced other faults on the links that did come up: no FEC on 25GBASE-SR is
   out of spec, and the ConnectX-4 Lx device-default negotiates cl108-rs.
   Corrected 2026-09-06 (#7): the switch ports run `fec CL108-RS` — the
   standalone uppercase interface command; lowercase and `no fec off` are
   rejected — and the card is left alone. **Never set `fec off` on the 25G
   ports again.** See the `#7` entry in the work plan.
5. Link up but no lease — a red herring; the g16 DHCP server was on the L2 and
   answered directly once a port carried traffic.

### Known follow-ups

- The Mellanox presents MTU 1500 to firmware, so the path is not jumbo
  end-to-end even though the switch ports are 9216. Transfer size is unaffected
  (MDTS drives it), but raising the card's UEFI MTU would let a 9000 path show.
- Three `boothost-C2NR0Q2` clones accumulated from repeated claims during
  bring-up; harmless, but the claim mints a fresh clone each call.
