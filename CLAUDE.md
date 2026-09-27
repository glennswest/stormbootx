# CLAUDE.md — stormbootx

A UEFI application that attaches a remote image over NVMe/TCP, publishes it
as `EFI_BLOCK_IO_PROTOCOL` so the firmware's partition and FAT drivers see its
GPT and ESP, and chain-loads the image's `\EFI\BOOT\BOOTX64.EFI`. ~115 KB,
`no_std`, one required firmware protocol (`EFI_TCP4`).

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
crate, so name the command. This builds the binary and runs both host suites:

```bash
sc-build 'cargo build --release --target x86_64-unknown-uefi && mkdir -p t && \
  rustc --edition 2021 --test src/intent.rs -o t/intent-test && ./t/intent-test && \
  rustc --edition 2021 --test src/sha256.rs -o t/sha256-test && ./t/sha256-test && \
  rustc --edition 2021 --test src/universal.rs -o t/universal-test && ./t/universal-test && \
  rustc --edition 2021 --test src/dnsname.rs -o t/dnsname-test && ./t/dnsname-test'
```

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
replies captured on 2026-09-27.

`Cargo.lock` is tracked, as it should be for anything that produces a binary.
Without it every build resolved fresh, and this is a firmware binary whose
whole dependency surface is two crates that move: a `cargo build` on 2026-09-02
offered `uefi` 0.40 against the 0.39 the code was written for. Nothing here
should change under a build nobody asked to change it. Bump the pin
deliberately, in its own commit, and rebuild.

`./scripts/build-boot-agent.sh` is packaging only: it puts the built `.efi` and a
`stormboot.conf` onto a GPT `.img` (or, with `--iso`, an `.iso` instead) in
`/build/images`. Nothing in the agent creates media.

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
| `src/tcp4.rs` | a blocking socket over the firmware's own TCP stack; ranks every NIC |
| `src/dhcp4.rs` | lease an address when the platform has not |
| `src/nvme.rs` | the NVMe/TCP initiator |
| `src/blockio.rs` | publish the namespace as a block device, then chain-load its `BOOTX64.EFI` |
| `src/intent.rs` | the boot intent (`install`/`local`/`auto`) read before the claim; every doubt is `auto` |
| `src/registry.rs` | read the intent; claim `boothost/<tag>`, or `boothost/default` by MAC; read the engine's version; also the old sbregistry `/v1/clones/claim` path, compiled out by `USE_REGISTRY = false` |
| `src/dnsname.rs` | the machine's DNS name (#23): DHCP options 12/15/6, PTR query and answer over DNS/TCP |
| `src/universal.rs` | universal boot (#15): is the engine new enough, which MAC is the machine's, what host the reply named |
| `src/sha256.rs` | the digest, because `EFI_HASH2` is optional |
| `src/config.rs` | the target, read from the media rather than compiled in |
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
- **Fedora's OVMF has no upper network stack.** SNP present, MNP/IP4/TCP4
  absent, and `ConnectController` over every handle does not change it. The
  obvious emulator cannot test the network path.
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

- [ ] #26 — **NIC UEFI drivers from the boot media (P0, 2026-09-27). In
      progress.** Supermicro X9 blades (server1–8) carry SnpDxe…TcpDxe but no
      UEFI driver for their NICs (Intel 10G `IBA XE`, ConnectX-3 `FlexBoot`,
      both legacy-only), so there is no SNP handle and no TCP4. Plan:
      `src/drivers.rs` loads every `\stormboot\drivers\*.efi` on the boot
      volume (after a platform `ConnectController` pass, so native drivers
      claim their NICs first), then connects everything and reports each
      driver; `scripts/build-nic-drivers.sh` builds iPXE's `intelx` and
      `hermon` `.efidrv` at a pinned commit; `build-boot-agent.sh --drivers
      DIR` lays them on the media. Done when server1 prints `tcp4 : available`.

- [ ] #13 — presentation at `docs/presentation.md` (Marp). **In progress
      2026-09-27.** Drawn from the #12-audited README and the code; slide 2
      matches `stormcentral check` (stormbootx: kind `boot`, → stormblock;
      nothing depends on it). "Golden kind" is stated as none (media only),
      with #21 as the open decision.
      **State at session restart (2026-09-27):** deck written and pushed
      (e173e48, linked from README, changelog entry). Verification sc-build
      (build + host tests + optional Marp render if the build box has npx)
      was running and its result is unknown — re-run it, then close #13 with
      what it showed.

- [ ] #10 — extract `nvme.rs` (and the claim) into a transport-generic
      `no_std` crate. Prerequisite for stormboot4bios.

### Blocked on other repos

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

- [ ] #23 — **identity from DNS (P1, owner 2026-09-27). The stormbootx side
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
        own lease is EDK2 behaviour, read from its source, not yet observed.

- [ ] #11 — per-machine boot intent. **The stormbootx half landed on
      2026-09-27** (`src/intent.rs`, `registry::boot_intent`, step 3a in
      `run()`). It reads `GET /api/v1/synonyms/boothost/<tag>/intent` before
      the claim. `local` falls through with no claim and no clone. `install`
      and `auto` claim as before. Every doubt (404, non-2xx, unreachable, no
      intent or an unknown one) reads as `auto`. It's verified by sc-build: the
      binary builds and `intent.rs`'s 7 host tests pass. **Still open on:**
      - **stormblock#148**: the engine route, the one-shot `install` → `local`
        reset, and carrying `install` in the claim reply for the initramfs.
        **Re-checked 2026-09-27:** not on stormblock main; forge on 13.7.0.
        Added to #148: resolve the name through `canonical()`/`host_of`,
        because since #15 a machine booted from the default reads its intent
        under its MAC (12 hex), and leave the GET open without a token.
        Until it lands every read is a 404 and every boot is `auto`, exactly
        as before.
      - **#3**: `auto` booting an installed, current disk locally (the owner's
        "new golden **and** requested" rule). This needs stormcos#30.
      - Not yet seen on metal: nothing can serve `local` until #148 lands.

- [ ] #3 (the rest) — **blocked, re-checked 2026-09-24 (now P0).** Three
      things, none of them in this repo:
      1. ~~**stormblock#123**~~, closed 2026-09-24 in stormblock v16.2.0: the
         flow-over now lays an ESP (stormuefi) and kernel pallets, so an
         installed disk boots on its own. It has been verified under OVMF but
         not yet on metal.
      2. **stormcos#30** — nothing writes an installed marker yet, and its
         natural home is the ESP that #123 adds.
      3. **The compare key** — an owner decision, #19, see below.
         **Re-checked 2026-09-27: still unanswered, so no code.** New
         since: on stormblock ≥ 17 the `GET boothost/<tag>` below needs a
         token (only the claim and `/api/v1/health` are open), so option 2
         also needs an engine change: e.g. the intended golden's key in the
         `…/intent` reply (stormblock#148). Recorded on #19. With #15 the
         key is read under the MAC-resolved name, not the serial.

      The owner's rule on #11 (2026-09-24) supersedes the issue's "different →
      reinstall": a node boots **local** unless there is a new golden **and**
      an install was requested. So #3 is the "is there a new golden?" half and
      #11 the "was it requested?" half; neither reinstalls on its own.

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
- [ ] #2 — self-update of the boot media (P3). stormbootx has no golden and
      nothing publishes `stormbootx.efi`, so there is not yet an artifact for a
      controlled digest to name. SHA-256 is done; what is left is
      gated on #4, because updating to "whatever was on the last image
      attached" is exactly the uncontrolled update this must not become. The
      next piece that needs no one else is hashing a file through
      `EFI_FILE_PROTOCOL` a buffer at a time — the streaming API is already
      shaped for it.

## Status

v0.4.0. **First complete NVMe/TCP attach on real hardware: 2026-09-05**
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

- Cosmetic: the per-NIC table reprints on every `connect_within` (the intent
  read and the claim open one socket each, the attach opens two), so the
  ranking prints four times a boot. Rank/print once and pass the socket down.
- The Mellanox presents MTU 1500 to firmware, so the path is not jumbo
  end-to-end even though the switch ports are 9216. Transfer size is unaffected
  (MDTS drives it), but raising the card's UEFI MTU would let a 9000 path show.
- Three `boothost-C2NR0Q2` clones accumulated from repeated claims during
  bring-up; harmless, but the claim mints a fresh clone each call.
