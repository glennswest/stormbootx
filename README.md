# stormbootx

**A UEFI NVMe/TCP boot extension. No kernel, no initramfs, no PXE.**

A ~110 KB UEFI application on a USB stick. It reads the machine's service tag
out of SMBIOS, attaches a remote image over `nvme-tcp://`, publishes it as
`EFI_BLOCK_IO_PROTOCOL` so the firmware's own partition and FAT drivers find
the GPT and the ESP, and then chain-loads `\EFI\BOOT\BOOTX64.EFI` off that
ESP — a bootloader on a disk that is not in the chassis.

```
service tag (SMBIOS)  →  claim boothost/<tag>  →  attach nvme-tcp://
    →  publish EFI_BLOCK_IO_PROTOCOL  →  chain-load the image's BOOTX64.EFI
```

## Where it sits in the boot

stormbootx is the **first of two stages**. It answers *which image* and gets
it attached; it knows nothing about what is inside. On a stormcos image the
`BOOTX64.EFI` it starts is [stormuefi](https://github.com/glennswest/stormuefi),
which finds the pallets on the attached disk, verifies them, selects one with
fallback and starts the kernel.

```
USB stick       stormbootx   tag → claim → NVMe/TCP attach → publish BlockIO
                                                           → chain-load ↓
attached clone  stormuefi    pallets → verify → select → kernel + initramfs → Linux
```

| How the machine boots | stormbootx | stormuefi |
|---|---|---|
| over the network, from forge | attaches the image | boots the kernel out of it |
| from its own drive, after flow-over | not involved | boots the kernel out of it |

stormuefi never talks to the network: every byte it reads from a network boot
goes through the BlockIO handle stormbootx published. Proven end to end on the
R230 (C2NR0Q2) on 2026-09-05.

Legacy-BIOS machines are served by neither. That is **stormboot4bios**
(planned, its own repo): one loader doing both stages, sharing this initiator
(#10) and stormuefi's `stormblock-pallet-format`.

### Why chain-load, not "the firmware boots it"

Publishing a block device and leaving it to the boot manager does not work on
real firmware: a disk that appears in the middle of a boot option is not in
`BootOrder`, and the machine drops to setup. Two further details are
load-bearing:

- **A vendor device-path node on the published handle.** EDK2's `PartitionDxe`
  on real firmware skips a bare BlockIO; OVMF was lenient and hid this.
- **The ESP is matched strictly by that node.** An early, looser match booted a
  stale Windows install off a local SAS disk instead of the image.

Nothing in that path is a file transfer. There is no PXE, no TFTP, no DHCP boot
option, and no HTTP first hop — the only transport is NVMe/TCP.

## Why it is this small

| | Size |
|---|---|
| `stormbootx.efi` | **~110 KB** (v0.3.8) |
| `tcp4probe.efi`, the firmware diagnostic | ~35 KB |
| the media image | 6 MB (1 MB GPT alignment + a 4 MB FAT16 ESP) |
| a kernel + initramfs UKI, for comparison | 64 MB |

**The firmware already has the network stack.** `EFI_TCP4_PROTOCOL` means the
platform's own TCP/IP and NIC driver do the networking, so this carries no
network stack, no NIC driver and no libc. What is added on top is only the NVMe
layer: PDU framing, the ICReq/ICResp handshake, the Fabrics Connect capsule,
admin and I/O queues, and the R2T/H2CData write flow.

## Identity is the service tag, not a MAC

The chassis serial — the Dell service tag — read from the SMBIOS table the
firmware already published in the EFI configuration table. It costs no network,
no DHCP, no BMC and no configuration.

- **Type 1 (System)** first: Dell, HPE, Lenovo and Cisco all carry it there.
- **Type 2 (Baseboard)**, then **Type 3 (Chassis)**: ODM boards often leave the
  system serial as a placeholder and burn the real number into the baseboard.
- **Placeholders are rejected**, not used — `Default string`, `To be filled by
  O.E.M.`, all-zero and the rest are shared by every board of a model, so a node
  claiming one boots as somebody else.
- **`tag = <id>` in `stormboot.conf` wins over all of it** — for a board with no
  usable serial, or to bench-test a box as another host.

The console names which source answered. The tag also goes out on every NVMe
connect as the host NQN (`nqn.2026-09.lo.storm:host-<tag>`), so the appliance
knows who is asking.

A NIC can be swapped or added to, and then a MAC-keyed boot server believes it
is looking at a different machine. The service tag is the chassis, and it is
what is printed on the pull-out tab when someone has to find the box.

## Modules

| File | Job |
|---|---|
| `smbios.rs` | the service tag, before any network exists |
| `tcp4.rs` | a blocking socket over the firmware's own TCP stack |
| `nvme.rs` | the NVMe/TCP initiator |
| `dhcp4.rs` | lease an address directly when the platform has not |
| `blockio.rs` | publish the namespace as a block device, then chain-load its `BOOTX64.EFI` |
| `registry.rs` | claim this machine's image, keyed on the service tag |
| `config.rs` | where to attach: the file, then the compiled floor |
| `shell.rs` | a timed, never-forced console on failure, before falling through |
| `mlxfec.rs` | reads (and on a recovery stick, writes) ConnectX FEC NV config |
| `sha256.rs` | the digest, for self-update (#2); unreferenced until then |
| `tcp4probe.rs` | a second binary — will this firmware run the agent at all? |

### The NVMe layer is ported, not rewritten

From sbregistry's `src/nvme.rs`, which is validated against real hardware. The
wire format is the part most likely to be subtly wrong, and two of its lessons
are load-bearing:

- **PSDT = 01b on every command.** There are no PRPs over a fabric. A zero FLAGS
  byte says "PRPs are used", and a controller that validates it rejects the
  command with Invalid Field before it looks at the SGL. stormblockmk does not
  check; the Linux target does.
- **The controller must be enabled before admin commands.** Fabrics Connect only
  establishes a queue; Identify before `CC.EN` is answered with Command Sequence
  Error on a conforming target.

## Which image, and where

Two questions, answered in different places on purpose.

**Which image** is a fleet decision — *this box runs 10.22* — so it lives next
to the images, as a `boothost/<service tag>` synonym on the storage engine. At
boot the agent claims it:

```
POST /api/v1/synonyms/boothost/C2NR0Q2/claim
  -> a copy-on-write clone of the golden that machine is assigned, costing
     nothing until it is written
  -> and the address, NQN and NSID that reach it
```

One request, and the answer is bootable. Moving a machine to a new version is a
`PUT` on its name — nothing on the media changes and nobody visits the machine.
The engine's API is the same host as the portal (`api_port`, default 9090): one
serves the bytes, the other says which bytes.

It is keyed on the service tag rather than a MAC because that names the chassis
and survives a network card being swapped. It is not in DHCP because a lease is
not a source of truth, it does not survive a change of boot method, and one
static string cannot answer the same name with different locations.

**A claim that fails is not a failed boot.** No synonym for this machine, a 404,
an engine that is down — the console says which and the boot continues on
whatever resolution below produced. An image nobody has assigned beats no image.
`claim = no` opts a stick out entirely.

## Finding the portal

Where to attach — the appliance address, not the image. Two sources, first hit
wins, and neither touches the network.

There was a third: DNS SRV/TXT discovery of the portal. It is **gone**, not
switched off. It made sense while the portal was the thing a machine had to be
told; once the portal became a fixed appliance address and the service tag
answered the interesting question, DNS was a second place for the answer to
live, a resolver that had to be right before a machine could boot, and a
timeout on every boot in a zone nobody published.

### 1. The media

`\stormboot\stormboot.conf` on the ESP, found through
`EFI_LOADED_IMAGE_PROTOCOL` — the exact volume this image was loaded from, so
there is no probing for "something that looks like our ESP" and no risk of
writing to a partition that belongs to somebody else.

```ini
# The appliance. nqn and nsid here are only the fallback, for a claim that
# cannot be reached — an image nobody assigned beats no image.
portal   = 192.168.31.202
port     = 4420
nqn      = nqn.2026-09.lo.g16:stormcos
nsid     = 2

# Which image is this machine's own, claimed by service tag.
api_port = 9090       # the engine API, on the same host as the portal
claim    = yes        # `no` leaves the machine on the nqn/nsid above

# Optional. States the identity instead of reading it from SMBIOS.
# tag    = C2NR0Q2

# Recovery sticks only (--fec). Writes this FEC to the ConnectX NV config once
# and warm-resets. Absent on every normal stick: nothing in the boot path
# decides on its own to rewrite a card.
# fec    = default
```

### 2. Compiled values

A floor, not a configuration — enough that a blank `dd`-written stick is useful
before anyone has edited anything.

## Will it run on this machine?

`tcp4probe.efi` is a second 24 KB binary that answers that before anyone writes
a stick, and it is the thing to run first on every new server model. It surveys
the nine protocols of the network stack layer by layer — firmware that stops at
MNP shows up as exactly that rather than as "no TCP4", which is a different
conversation with a vendor — then creates and configures a TCP4 child, because
presence is necessary and not sufficient.

`stormbootx` itself runs a `ConnectController` pass before giving up on TCP4:
UEFI binds drivers on demand, and an application that only calls
`LocateHandleBuffer` never creates the demand, so a stack that is built in but
unbound looks identical to one that is absent. The console says which of the
three ways TCP4 turned out to be reachable.

## Building

Builds on `dev.g8.lo`, never a workstation.

```bash
export CARGO_TARGET_DIR=/build/cargo/stormbootx
cargo build --release --target x86_64-unknown-uefi

# The normal stick: names the portal, claims its image by service tag.
./scripts/build-boot-agent.sh

# A stick pinned to one namespace, for a machine that must not move.
./scripts/build-boot-agent.sh --pin --portal 192.168.31.202 \
    --nqn nqn.2026-09.lo.g16:stormcos --nsid 2

# A diagnostic stick that boots tcp4probe instead of the agent.
./scripts/build-boot-agent.sh --probe --output /build/images/tcp4probe.img

# Also write an El Torito UEFI ISO, for iDRAC virtual media.
./scripts/build-boot-agent.sh --iso

# A FEC recovery stick (see `fec =` above).
./scripts/build-boot-agent.sh --fec default

dd if=/build/images/stormbootx.img of=/dev/sdX bs=4M conv=fsync
```

`src/sha256.rs` is the one part that can be exercised without a machine to
boot — it touches only `core` and names no `crate::` item, so it compiles
standalone:

```bash
rustc --edition 2021 --test src/sha256.rs -o $CARGO_TARGET_DIR/sha256-test && \
  $CARGO_TARGET_DIR/sha256-test
```

Output goes to `/build/images` — never `/tmp`, which on dev is a tmpfs sized at
half of RAM.

## What it needs from the firmware

`EFI_TCP4_PROTOCOL`, which is **not** implied by the machine having a NIC: the
platform's TCP/IP stack is a separate set of DXE drivers that firmware often
loads only when network boot is enabled in setup. The agent checks and says so
rather than failing obscurely.

On Dell that is **Integrated NIC → Enabled with PXE**, or **UEFI Network
Stack** under Network Settings — not because anything wants PXE, but because it
is what makes the firmware load MNP/IP4/TCP4. The stack may also arrive late:
on the R230 the first boot option found no TCP4 and the second, seconds later,
found it bound, so the agent waits up to 5 s for it.

Once TCP4 is there, the agent does not trust the platform's choices:

- **Every interface is tried**, ranked by link and then descending MTU, because
  a server has one TCP4 service per NIC and the first is often the 1 GbE
  management port with no route to the portal.
- **It leases its own address** over `EFI_DHCP4` when the platform has not run
  DHCP (which it often only does inside a PXE attempt).

Worth knowing before reaching for the obvious emulator: **Fedora's OVMF has no
upper network stack at all** — SNP appears, MNP/IP4/TCP4 do not, and a
`ConnectController` pass over every handle does not change that. **Proxmox's
OVMF does** (`pve-edk2-firmware`): its HTTP boot support pulls TCP4 in, so the
network path can be exercised in a VM there.

## When it fails

Every failure — no TCP4, no claim, no attach, no bootloader on the image —
**falls through to the local disk.** One provisioning outage must not become a
fleet outage. Before it does, it offers a timed console (`shell.rs`) that shows
the NICs, their addresses and whether a host is reachable; silence takes the
normal path, so an unattended machine never stops at a prompt.

## Status

**Running on hardware since 2026-09-05.** A PowerEdge R230 (C2NR0Q2) booted
over iDRAC virtual media, claimed `boothost/C2NR0Q2`, attached a 32 GiB clone
from forge over 25 GbE, and chain-loaded stormuefi off it, which started
stormcos's kernel:

```
service tag : C2NR0Q2
tcp4        : available
claim       : boothost/C2NR0Q2 at 192.168.31.202:9090
  claimed a clone of this machine's image
  portal    : 192.168.31.202:4420  nsid 7
  namespace : 8388608 blocks x 4096 bytes  (32 GiB)
  transfer  : 128 KiB per command  (controller MDTS 5; path MTU 1500)
blockio     : published on handle 0x8301ae98
RESULT: image attached; starting its bootloader.
```

Open work is tracked as issues: registration and the intended image (#4),
skipping to the disk when nothing changed (#3, #11), self-update of the stick
(#2), and extracting the initiator for stormboot4bios (#10).

Related: [stormuefi](https://github.com/glennswest/stormuefi) (the second
stage — pallet selection and kernel start, on the network clone and later on
the local disk), [stormnetboot](https://github.com/glennswest/stormnetboot)
(the boot server and the post-`switch_root` agent), and
[dswfecfix](https://github.com/glennswest/dswfecfix) (the switch-side recovery
for dsw1's 25G port latch).
