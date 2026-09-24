# stormbootx

**A UEFI boot agent that attaches a remote disk over NVMe/TCP and boots it.**
No kernel, no initramfs, no PXE, no TFTP. It uses the firmware's own TCP stack.

It runs from a USB stick or a virtual-media ISO. It reads the machine's
identity from SMBIOS, claims that machine's image from the storage engine,
attaches it over NVMe/TCP, publishes it as `EFI_BLOCK_IO_PROTOCOL`, and
chain-loads the `\EFI\BOOT\BOOTX64.EFI` on the attached disk. If any step
fails, it falls through to the local disk.

```
identity (conf or SMBIOS) → claim boothost/<tag> → NVMe/TCP attach
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
loaded from. It writes to three things:

| What | When |
|---|---|
| the attached clone on the engine | the booted OS writes to its disk, and the BlockIO handle is read-write |
| the platform's IP4 policy (`EFI_IP4_CONFIG2`) | any interface set to `STATIC` is switched to `DHCP` when a socket is opened; on EDK2-based firmware this setting is kept in NVRAM |
| the ConnectX NV FEC setting | only with `fec =` on a recovery stick, or `fec MODE` typed at the failure console; followed by a warm reset |

## What it does, step by step

`src/main.rs`, `run()`:

1. **Banner**: version and build stamp (`b<n>-<sha>`, set by the build
   script), so the console always says which binary is talking.
2. **Identity.**
   - `tag = <id>` in `stormboot.conf` wins over everything.
   - Otherwise SMBIOS (`src/smbios.rs`, via the `_SM3_` or `_SM_` entry in the
     EFI configuration table): the serial of Type 1 (System), then Type 2
     (Baseboard), then Type 3 (Chassis). The first one that is not a
     placeholder is used.
   - **Placeholders are rejected**: empty, `none`, `unknown`,
     `default string`, `system serial number`, `not applicable`,
     `not specified`, `n/a`, `invalid`, anything containing `to be filled` or
     `o.e.m.`, and all-zero strings.
   - No usable identity is a failure, and the boot falls through.
   - The console prints the source, plus the SMBIOS model when there is one.
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
   then claims the machine's image:

   ```
   POST http://<portal>:<api_port>/api/v1/synonyms/boothost/<tag>/claim   body {}
   ```

   The reply supplies the address, port, NQN and NSID. Both `address`/`port`
   and `traddr`/`trsvcid` spellings are accepted, with port defaulting to
   4420 and NSID to 1. A 404 is reported as "no `boothost/<tag>` synonym". Any
   claim failure falls back to the conf's own `nqn`/`nsid` rather than
   failing.
6. **Attach** (`src/nvme.rs`). The host NQN is
   `nqn.2026-09.lo.storm:host-<tag>`, so the target knows which machine is
   connecting. The console prints the namespace geometry and the transfer
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

`src/tcp4.rs` and `src/dhcp4.rs`. A socket is opened for the claim and for
each NVMe queue, and each open works like this:

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
- Timeout: 30 s per operation (`Tcp4Socket::connect`), for both the claim and
  the attach.

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
| `tag` | none (SMBIOS) | states the identity |
| `fec` | none | **recovery sticks only**: write this FEC and warm-reset |
| `stamp` | none | parsed, not yet used; for self-update (#2) |

## `tcp4probe`

A second binary, `src/tcp4probe.rs`, to run first on a new server model. It
reports which network-stack protocols are present layer by layer, runs a
`ConnectController` pass when TCP4 is missing, and then creates and
configures a TCP4 child. Presence alone does not prove the agent will work.
A stick that boots it is made with `--probe` (see *Getting it onto a stick*).

## Build

On `dev.g8.lo`, never the workstation:

```bash
export CARGO_TARGET_DIR=/build/cargo/stormbootx
cargo build --release --target x86_64-unknown-uefi
```

That builds `stormbootx.efi` (~110 KB) and `tcp4probe.efi` (~35 KB). There is
no host target and no `cargo test`. `src/sha256.rs` is the exception: it
depends only on `core` and runs its FIPS vectors standalone:

```bash
rustc --edition 2021 --test src/sha256.rs -o $CARGO_TARGET_DIR/sha256-test && \
  $CARGO_TARGET_DIR/sha256-test
```

### Getting it onto a stick

Packaging only; nothing here runs at boot. `scripts/build-boot-agent.sh`
(on dev) puts the built `.efi` and a `stormboot.conf` onto boot media in
`/build/images`: a GPT `.img` to `dd` onto a USB stick, or with `--iso` an
El Torito `.iso` for iDRAC virtual media instead.

```bash
./scripts/build-boot-agent.sh                    # claims by service tag
./scripts/build-boot-agent.sh --iso              # same, as an ISO
./scripts/build-boot-agent.sh --pin --portal 192.168.31.202 \
    --nqn nqn.2026-09.lo.g16:stormcos --nsid 2   # one fixed namespace, no claim
./scripts/build-boot-agent.sh --probe            # boots tcp4probe instead
./scripts/build-boot-agent.sh --fec default      # FEC recovery stick
```

`--help` lists the rest (`--api-port`, `--port`, `--size`, `--binary`,
`--output`).

## Firmware requirements

- **`EFI_TCP4`**, which having a NIC does not imply. On Dell, set Integrated
  NIC to **Enabled with PXE**, or enable **UEFI Network Stack** under Network
  Settings. `GlobalSlotDriverDisable` must be off, or add-in cards have no
  UEFI driver.
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

v0.3.8. Running on hardware since 2026-09-05. A Dell PowerEdge R230 (C2NR0Q2)
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

Related: [stormuefi](https://github.com/glennswest/stormuefi) (stage two) and
[stormnetboot](https://github.com/glennswest/stormnetboot) (the boot server and
the post-`switch_root` agent).
