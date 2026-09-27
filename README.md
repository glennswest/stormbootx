# stormbootx

**A UEFI boot agent that attaches a remote disk over NVMe/TCP and boots it.**
No kernel, no initramfs, no PXE, no TFTP. It uses the firmware's own TCP stack.

It runs from a USB stick or a virtual-media ISO. It reads the machine's
identity from SMBIOS, claims that machine's image from the storage engine,
attaches it over NVMe/TCP, publishes it as `EFI_BLOCK_IO_PROTOCOL`, and
chain-loads the `\EFI\BOOT\BOOTX64.EFI` on the attached disk. If any step
fails, or the machine's boot intent is `local`, it falls through to the local
disk.

```
identity (conf or SMBIOS) → boot intent → claim boothost/<tag> → NVMe/TCP attach
    → publish BlockIO + device path → ConnectController
    → load \EFI\BOOT\BOOTX64.EFI from the attached ESP → StartImage
```

## Where it sits in the boot

stormbootx is **stage one of two**. It decides *which image* and attaches it,
and knows nothing about what is inside. On a stormcos image the `BOOTX64.EFI`
it starts is [stormuefi](https://github.com/glennswest/stormuefi), which finds
the pallets, verifies them and starts the kernel.

```
USB stick / ISO   stormbootx   tag → claim → attach → publish → chain-load ↓
attached clone    stormuefi    pallets → select → verify → kernel + initramfs → Linux
```

| How the machine boots | stormbootx | stormuefi |
|---|---|---|
| over the network, from forge | attaches the image | boots the kernel out of it |
| from its own drive | not involved | boots the kernel out of it |

Legacy-BIOS machines are served by neither. That is the planned
**stormboot4bios**, and #10 (extracting the initiator) is its prerequisite
here.

## What it touches at boot

At boot it **reads** only `\stormboot\stormboot.conf` from the volume it was
loaded from, and **loads** any `*.efi` in `\stormboot\drivers\` on that
volume as a NIC driver (#26; absent on ordinary media). It writes to three things:

| What | When |
|---|---|
| the attached clone on the engine | the booted OS writes to its disk, and the BlockIO handle is read-write |
| the platform's IP4 policy (`EFI_IP4_CONFIG2`) | any interface set to `STATIC` is switched to `DHCP` when a socket is opened; on EDK2-based firmware this setting is kept in NVRAM |
| the ConnectX NV FEC setting | only with `fec =` on a recovery stick, or `fec MODE` typed at the failure console; followed by a warm reset |

## What it does, step by step

`src/main.rs`, `run()`:

1. **Banner**: version and build stamp (`STORMBOOTX_BUILD`: the short commit,
   with `-dirty` if the tree had changes, set by `build-boot-agent.sh`;
   `unstamped build` otherwise), so the console always says which binary is
   talking.
2. **Identity.** A machine is known to the engine by its **DNS name**
   (#23, stormblock#199); its serial and MAC are aliases.
   - `name = <id>` (or the older `tag = <id>`) in `stormboot.conf` wins over
     everything, and is the only name tried.
   - Otherwise, once the engine has been reached (step 5), the **DNS name**:
     DHCP option 12 from the lease on the interface that reached the engine
     (option 15 adds the domain for the console), else the **PTR** of that
     interface's address, asked of the option 6 DNS server over **DNS/TCP**
     (`src/dnsname.rs`; microdns answers over TCP, and TCP4 is the one stack
     this binary needs anyway). The engine knows hosts by the first label, so
     `server3.g10.lo` claims `boothost/server3`. The console prints
     `name : server3.g10.lo (from DHCP)`. A name that is not a valid DNS name
     is ignored, not guessed at.
   - Otherwise SMBIOS (`src/smbios.rs`, via the `_SM3_` or `_SM_` entry in the
     EFI configuration table): the serial of Type 1 (System), then Type 2
     (Baseboard), then Type 3 (Chassis). The first one that is not a
     placeholder is used.
   - **Placeholders are rejected**: empty, `none`, `unknown`,
     `default string`, `system serial number`, `not applicable`,
     `not specified`, `n/a`, `invalid`, anything containing `to be filled` or
     `o.e.m.`, and all-zero strings.
   - No usable serial is no longer a failure: once the network stack exists
     (step 4) the machine's **MAC** identifies it, the lowest usable unicast
     permanent address across every NIC (`tcp4::machine_mac`), so the answer
     does not depend on driver bind order. Only no serial *and* no MAC falls
     through.
   - The console prints the source, plus the SMBIOS model when there is one,
     and a `mac` line after step 4.
3. **NIC FEC report** (`src/mlxfec.rs`). It prints every ConnectX port's
   current and next-boot FEC, read only. If `fec =` is set in the conf, it
   writes that value and warm-resets once. The next boot then finds nothing to
   change.
4. **TCP4** (`tcp4::ensure_available`).
   1. It checks whether TCP4 is present.
   2. If not, it runs `ConnectController` on the NIC (SNP) handles, then on
      every handle.
   3. Then it waits up to 5 s, retrying every 250 ms.
   4. The console says which of those worked. If none did, the boot falls
      through with advice on the firmware setting.
5. **Where and which.** `config::resolve` gives the portal from the conf,
   falling back to compiled defaults. Unless the conf says `claim = no`, it
   first reads the machine's **boot intent** (`src/intent.rs`):

   ```
   GET http://<portal>:<api_port>/api/v1/synonyms/boothost/<tag>/intent   → {"intent":"…"}

   (It is read under the stated name, else the DNS name, else — for a machine
   claiming the default — its MAC's twelve hex digits, which the engine
   resolves as an alias, else the serial.)
   ```

   | intent | what stormbootx does |
   |---|---|
   | `install` | claims and boots the image, as below |
   | `local` | falls through to the local disk at once: no claim, no clone |
   | `auto` | claims and boots, as every boot did before intents existed |

   Set a state on the engine and power-cycle the machine to get it. **Any
   doubt reads as `auto`**: a 404, a non-2xx, an unreachable engine, or a body
   with no intent or an unknown one. Only an explicit `local` skips the
   network image, so a failed read can't keep a machine off an install it was
   asked for. The console prints the intent and, when it defaulted, why. The
   contract is the one proposed on stormblock#148, and the engine doesn't serve
   it yet, so today every read is a 404 and every boot is `auto`. Resetting
   `install` back to `local` after the install is the engine's job.
   `auto` does not yet boot an installed, current disk locally. That needs #3.

   First it reads the engine's version from `GET /api/v1/health`. That is the
   boot's first request, so it also brings the network up and tells which
   interface to read the DNS name from. Then it claims the machine's image,
   best name first:

   ```
   POST …/api/v1/synonyms/boothost/<stated name>/claim   body {}
   POST …/api/v1/synonyms/boothost/<DNS name>/claim      body {"mac":"…","serial":"…"}
   POST …/api/v1/synonyms/boothost/default/claim         body {"mac":"…","serial":"…"}
   POST …/api/v1/synonyms/boothost/<serial>/claim        body {}
   ```

   A stated name is the only one tried. Otherwise a 404 moves on to the next
   line, and any other failure falls back to the resolved target. The DNS
   claim carries the MAC and serial so the engine can tie a name it has not
   seen to a host it already knows (stormblock#204).

   The reply supplies the address, port, NQN and NSID. Both `address`/`port`
   and `traddr`/`trsvcid` spellings are accepted, with port defaulting to
   4420 and NSID to 1. Any claim failure falls back to the conf's own
   `nqn`/`nsid` rather than failing.

   **Universal boot: one medium for every machine** (#15, stormblock#200).
   A machine whose media states no `tag =` claims `boothost/default` and says
   which machine it is with its MAC. The engine gives it a copy-on-write
   clone of the default release **of its own**, recorded as host
   `mac-<hex>` until it is named, and the same MAC gets the same host back on
   every boot. The console says `booting the default image as mac-<hex>`, or,
   once the machine has a name the MAC is an alias of, `booting <name>'s
   image`. Not the SMBIOS serial: serials are not unique (seven Supermicro
   MicroCloud nodes report one chassis serial), and a serial claim would boot
   them all as one machine. The serial rides along in the body so the engine
   can resolve a machine it already knows by serial (C2NR0Q2) to that host
   (stormblock#202). stormbootx cannot check that itself, because every read
   but the claim needs a token.

   **Only against an engine that has #200**, told by the public
   `/api/v1/health` version being after 19.3.0. An engine from v17 to 19.3.0
   reads `default` as one shared tag, and each machine's claim would release
   the clone another is running from. Against those, or when the version
   can't be read, it claims `boothost/<serial>` as before, and the engine
   (v17 and later) pins an unknown serial to `boothost/default`. If a #200
   engine has no `boothost/default` to give (404) and the machine has a
   serial, it tries the serial claim too; with no default the engine cannot
   mint a serial host, so that only finds one that already exists. A stated
   `tag =` is always claimed as that tag. The default is set through
   stormipmi's `/api/v1/machines/default` (stormcentral#29).
6. **Attach** (`src/nvme.rs`). The host NQN is
   `nqn.2026-09.lo.storm:host-<name>`, so the target knows which machine is
   connecting: the engine's name for the machine from the claim reply
   (`mac-<hex>` for one it booted as the default), else the DNS name, else the
   tag. The console prints the namespace geometry and the transfer
   size.
7. **Publish** (`src/blockio.rs`). BlockIO is installed with the namespace's
   own block size (read from FLBAS, so 4096 on a 4K namespace). A device path
   of one hardware vendor node (`6d7a1f2e-9c34-4b8a-b1d0-5e2f7a0c9b41`) goes
   on the same handle, then `ConnectController` recursively.
8. **Chain-load.** It looks for `\EFI\BOOT\BOOTX64.EFI` on a filesystem whose
   device path **starts with that vendor node**, which means an ESP on the
   attached disk only. It is never a local disk. `LoadImage` + `StartImage`.
   Success never returns.

### Why chain-load

Publishing a disk and returning to the boot manager does not boot it. A disk
that appears while a boot option is running is not in `BootOrder`, so the
manager moves on and drops to setup. Real EDK2's `PartitionDxe` also skips a
handle that has BlockIO but no device path. That is why step 7 installs one.
OVMF is lenient here and hid it. The ESP match is strict because an earlier,
looser version booted a stale Windows install off a local SAS disk.

## The network path

**NIC drivers from the media (#26, `src/drivers.rs`).** Before TCP4 is looked
for, every `*.efi` in `\stormboot\drivers\` on the boot volume is loaded
(`LoadImage` by device path) and started, then every handle is connected. One
`ConnectController` pass runs first, so the platform's own drivers claim every
NIC they will take and a media driver only gets the ones nothing else wanted.
The console prints `drivers : N of M started from \stormboot\drivers` and one
line per file; a driver that fails is reported and skipped. This is for
firmware that has the TCP/IP stack but no UEFI driver for its NICs, like the
Supermicro X9 blades (legacy-only Intel 10G and ConnectX-3). No directory, no
change.


`src/tcp4.rs` and `src/dhcp4.rs`. A socket is opened for the intent read,
the claim, and each NVMe queue (admin and one I/O), and each open works like
this:

- **Every TCP4 interface is tried**, ranked by link state and then by
  descending MTU, because each NIC carries its own stack. The ranking is
  printed.
- Any interface set to `STATIC` is switched to `DHCP` first (see the table
  above).
- **Three phases, cheapest first:**
  1. an interface that already has an address;
  2. waiting for the platform's DHCP, up to the socket budget;
  3. a DHCP client of its own over `EFI_DHCP4`, once per interface, matched
     to the TCP4 interface by MAC. The lease is stated explicitly in
     `Tcp4ConfigData`.
- Timeout: 30 s per operation (`Tcp4Socket::connect`), for the intent read,
  the claim and the attach. The console's `connect` uses 8 s.

## The NVMe/TCP initiator

`src/nvme.rs` was ported from sbregistry's host initiator.

- It speaks ICReq/ICResp, then Fabrics Connect on the admin and I/O queues,
  then Property Get/Set, `CC.EN`, and Identify for the controller and the
  namespace. Reads use C2HData. Writes use R2T/H2CData.
- **PSDT = 01b (SGL) on every command.** A zero FLAGS byte means PRPs, which
  do not exist over a fabric.
- **`CC.EN` before any admin command.** Identify on a disabled controller is a
  Command Sequence Error.
- **Synchronous, one command in flight.** There is no write pipelining.
- **No header or data digests.** A target that insists on them is refused.
- **Transfer size comes from the controller's MDTS**:
  `2^(12+MPSMIN) × 2^MDTS`, capped at 512 KiB, or 64 KiB if MDTS is 0. It is
  never derived from the MTU. Against stormblock (MDTS 5) that is 128 KiB per
  command.

## When it fails

Every error in `run()` ends in `fall_through`:

1. It prints `no network boot: <reason>`.
2. It offers a console: *press c within 5 s*. Silence continues, so an
   unattended machine never stops at a prompt.
3. It counts local disks (whole, non-removable BlockIO devices).
   - If there are any, it prints `falling through to the local disk (N found)`.
   - If there are none, it says so and waits 30 s.
4. It returns `EFI_ABORTED`, so the boot manager tries the next boot option.

The console (`src/shell.rs`):

| Command | Does |
|---|---|
| `nics` | every network interface the firmware knows |
| `state` | each interface's address and IP4 policy |
| `dhcp [n]` | run DHCP on interface n, or all |
| `connect IP PORT` | open a TCP connection the way the attach does |
| `pci [all]` | devices on the bus, with or without a driver |
| `fec [MODE]` | read the ConnectX FEC; with MODE (`default`/`rs`/`fc`/`off`/`autoneg`), write it |
| `reset` | warm reset, so firmware re-reads NV config |
| `boot` | leave the console and continue the fall-through |

## `stormboot.conf`

`\stormboot\stormboot.conf` on the volume the binary was loaded from, found
through `EFI_LOADED_IMAGE_PROTOCOL`. It uses `key = value` lines, and `#`
starts a comment. Each key stands alone, so a typo in one does not reset the
others.

| Key | Default (compiled) | Meaning |
|---|---|---|
| `portal` | `192.168.31.202` | NVMe/TCP portal, and the host the claim goes to |
| `port` | `4420` | NVMe/TCP port |
| `nqn` | `nqn.2026-09.lo.g16:stormcos` | subsystem NQN if the claim fails or is off |
| `nsid` | `2` | namespace if the claim fails or is off |
| `api_port` | `9090` | engine API port on the portal host |
| `claim` | `yes` | `no` / `false` / `0` / `off` skips the claim |
| `name` (or `tag`) | none (DNS name, then MAC, then SMBIOS) | states the identity; the only name claimed |
| `fec` | none | **recovery sticks only**: write this FEC and warm-reset |
| `stamp` | none | parsed, not yet used; for self-update (#2) |

## `tcp4probe`

A second binary, `src/tcp4probe.rs`, to run first on a new server model. It
reports which network-stack protocols are present layer by layer, runs a
`ConnectController` pass when TCP4 is missing, and then creates and
configures a TCP4 child. Presence alone does not prove the agent will work.
A stick that boots it is made with `--probe` (see *Getting it onto a stick*).

## Build

Never on a workstation, and never as root. Push, then run `sc-build` from the
checkout. It fetches the pushed commit onto the build box (`dev.g8.lo`, as the
unprivileged `stormbuild` user), builds it in a scratch directory and deletes
it. The default `cargo build && cargo test` does not suit a `no_std` UEFI
crate, so name the command:

```bash
sc-build 'cargo build --release --target x86_64-unknown-uefi && mkdir -p t && \
  rustc --edition 2021 --test src/intent.rs -o t/intent-test && ./t/intent-test && \
  rustc --edition 2021 --test src/sha256.rs -o t/sha256-test && ./t/sha256-test && \
  rustc --edition 2021 --test src/universal.rs -o t/universal-test && ./t/universal-test && \
  rustc --edition 2021 --test src/dnsname.rs -o t/dnsname-test && ./t/dnsname-test'
```

That builds `stormbootx.efi` and `tcp4probe.efi`, then runs the four host
test suites. There is no host target and no `cargo test`. `src/sha256.rs`,
`src/intent.rs`, `src/universal.rs` and `src/dnsname.rs` are the exceptions: each uses only
`core` and names no `crate::` item, so each compiles as its own crate with
`rustc --test`.
`--edition 2021` is required, because bare `rustc` defaults to 2015, where
`core` is not in scope.

`Cargo.lock` is tracked, and `uefi` is pinned at 0.39 / `uefi-raw` at 0.15.
Bump them deliberately, in their own commit.

### Getting it onto a stick

Packaging only; nothing here runs at boot. `scripts/build-boot-agent.sh`
(on dev) puts the built `.efi` and a `stormboot.conf` onto boot media in
`/build/images`: a GPT `.img` to `dd` onto a USB stick, or with `--iso` an
El Torito `.iso` for iDRAC virtual media instead.

```bash
./scripts/build-boot-agent.sh                    # one stick for every machine
./scripts/build-boot-agent.sh --iso              # same, as an ISO
./scripts/build-boot-agent.sh --pin --portal 192.168.31.202 \
    --nqn nqn.2026-09.lo.g16:stormcos --nsid 2   # one fixed namespace, no claim
./scripts/build-boot-agent.sh --probe            # boots tcp4probe instead
./scripts/build-boot-agent.sh --fec default      # FEC recovery stick

# NIC drivers for firmware without them (#26): iPXE as EFI drivers
./scripts/build-nic-drivers.sh /build/images/drivers
./scripts/build-boot-agent.sh --iso --drivers /build/images/drivers
```

**On hold (2026-09-27): which driver ships is an open owner decision on #26; don't use this script yet.** `build-nic-drivers.sh` builds iPXE's `intelx` (Intel 82599/X540/X552) and
`hermon` (ConnectX-3) as `bin-x86_64-efi/*.efidrv` at a pinned commit, named
`ipxe-*.efi`, beside an `IPXE-SOURCE.txt` naming that commit. iPXE is GPL-2
and ships as separate binaries on the media.

`--help` lists the rest (`--drivers`, `--api-port`, `--port`, `--size`, `--binary`,
`--output`).

## Ports, health and shipping

stormbootx is a UEFI application, so it has no daemon, listens on no port and
has no health or metrics endpoint. What it reports goes to the console. It
only makes outbound connections:

| To | Default | Set by |
|---|---|---|
| engine API (health, intent read, claim) | `<portal>:9090`, HTTP/1.1 | `api_port` |
| DNS server (PTR of its own address, #23) | option 6 of its DHCP lease, TCP 53 | DHCP |
| NVMe/TCP portal (attach) | `<portal>:4420`, or what the claim returns | `port`, or the claim reply |

The portal defaults to `192.168.31.202` (forge). Since stormblock v17.0.0 the
engine API requires a token for everything except `POST
…/boothost/<tag>/claim` and `GET /api/v1/health`. stormbootx sends none, because firmware has nowhere to
keep one. The intent route (stormblock#148) will need the same exemption, and
until it has one a read gets a 401, which reads as `auto`.

**How it ships:** as boot media, not as a stormcos component. There is no
golden for it. `scripts/build-boot-agent.sh` writes the `.efi` and a
`stormboot.conf` onto a `.img` or `.iso` (below), which goes onto a USB stick
or iDRAC virtual media.

## Firmware requirements

- **`EFI_TCP4`**, which having a NIC does not imply. On Dell, set Integrated
  NIC to **Enabled with PXE**, or enable **UEFI Network Stack** under Network
  Settings. `GlobalSlotDriverDisable` must be off, or add-in cards have no
  UEFI driver.
- **A UEFI driver for the NIC**, or one on the media (#26). On the
  Supermicro X9 blades the stack is there (Advanced → PCIe/PCI/PnP →
  **Network stack = Enabled**), but their NICs carry legacy option ROMs only,
  so the media has to bring `--drivers`.
- `EFI_DHCP4` is optional. It is only used when the platform produced no
  address.
- For emulation, use **Proxmox's OVMF**, which has the stack. Fedora's OVMF
  has no upper network stack at all.

## In the code, not active

- `registry::claim` / `registry::existing`: the older sbregistry
  `/v1/clones/claim` path at `sbregistry.gt.lo:5100`, behind
  `USE_REGISTRY = false` in `main.rs`.
- `config::render` / `config::write_file`, `sha256.rs`, and the `stamp` key:
  the self-update path (#2), not wired up.
- The FEC self-heal (automatic write on "all ports down") was switched off in
  0.3.6. It was triggered by a single link sample. The reasoning is in
  `main.rs` step 2b.

## Status

v0.4.0. Running on hardware since 2026-09-05. A Dell PowerEdge R230 (C2NR0Q2)
booted the ISO over iDRAC virtual media, claimed `boothost/C2NR0Q2`, attached
a 32 GiB 4K clone from forge over 25 GbE, and chain-loaded stormuefi, which
started stormcos:

```
service tag : C2NR0Q2
tcp4        : available
claim       : boothost/C2NR0Q2 at 192.168.31.202:9090
  claimed a clone of this machine's image
  portal    : 192.168.31.202:4420  nsid 7
attaching   : nqn.2026-09.lo.storm:host-C2NR0Q2
  namespace : 8388608 blocks x 4096 bytes  (32 GiB)
  transfer  : 128 KiB per command  (controller MDTS 5; path MTU 1500)
blockio     : published on handle 0x8301ae98
RESULT: image attached; starting its bootloader.
boot        : starting \EFI\BOOT\BOOTX64.EFI from the attached image
```

Open issues:

| Issue | What |
|---|---|
| #2 | self-update of the stick |
| #3, #11 | skipping to the disk when nothing changed; a per-machine boot intent |
| #4 | inventory registration |
| #7 | the identity follow-ups |
| #10 | extracting the initiator for stormboot4bios |
| #13, #14 | a presentation; test containers per the stormcos test standard |
| #15 | universal boot: client side done, waiting on an engine with stormblock#200 and a `boothost/default` |

A slide deck of the above is in [`docs/presentation.md`](docs/presentation.md)
(Marp: `npx @marp-team/marp-cli docs/presentation.md`).

Related: [stormuefi](https://github.com/glennswest/stormuefi) (stage two) and
[stormnetboot](https://github.com/glennswest/stormnetboot) (the earlier
network-boot project, whose PXE chain this USB/NVMe-TCP path retired).
