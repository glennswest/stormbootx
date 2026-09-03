# stormbootx

**A UEFI NVMe/TCP boot extension. No kernel, no initramfs, no PXE.**

A 57 KB UEFI application on a USB stick. It reads the machine's service tag out
of SMBIOS, attaches a remote image over `nvme-tcp://`, and publishes it as
`EFI_BLOCK_IO_PROTOCOL` — after which the firmware's own partition and FAT
drivers find the GPT and the ESP, and the boot manager loads a bootloader from
a disk that is not in the chassis.

```
service tag (SMBIOS)  →  attach nvme-tcp://  →  publish EFI_BLOCK_IO_PROTOCOL
                                              →  firmware boots it
```

Nothing in that path is a file transfer. There is no PXE, no TFTP, no DHCP boot
option, and no HTTP first hop — the only transport is NVMe/TCP.

## Why it is this small

| | Size |
|---|---|
| `stormbootx.efi` | **57 KB** |
| `tcp4probe.efi`, the firmware diagnostic | 24 KB |
| the media image | 6 MB (1 MB GPT alignment + a 4 MB FAT16 ESP) |
| a kernel + initramfs UKI, for comparison | 64 MB |

**The firmware already has the network stack.** `EFI_TCP4_PROTOCOL` means the
platform's own TCP/IP and NIC driver do the networking, so this carries no
network stack, no NIC driver and no libc. What is added on top is only the NVMe
layer: PDU framing, the ICReq/ICResp handshake, the Fabrics Connect capsule,
admin and I/O queues, and the R2T/H2CData write flow.

## Identity is the service tag, not a MAC

SMBIOS type 1 "Serial Number" — the Dell service tag — read from the table the
firmware already published in the EFI configuration table. It costs no network,
no DHCP, no BMC and no configuration.

A NIC can be swapped or added to, and then a MAC-keyed boot server believes it
is looking at a different machine. The service tag is the chassis, and it is
what is printed on the pull-out tab when someone has to find the box.

## Modules

| File | Job |
|---|---|
| `smbios.rs` | the service tag, before any network exists |
| `tcp4.rs` | a blocking socket over the firmware's own TCP stack |
| `nvme.rs` | the NVMe/TCP initiator |
| `blockio.rs` | publish the namespace as a block device, then `ConnectController` |
| `registry.rs` | claim this machine's image, keyed on the service tag |
| `config.rs` | where to attach: the file, then the compiled floor |
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

Worth knowing before reaching for the obvious emulator: **Fedora's OVMF has no
upper network stack at all** — SNP appears, MNP/IP4/TCP4 do not, and a
`ConnectController` pass over every handle does not change that. `tcp4probe`
will say so plainly rather than leaving you to conclude the code is broken.

## Status

Built and verified as an artifact; **not yet run on hardware**. The first line
to watch is `tcp4 : available`.

Related: [stormnetboot](https://github.com/glennswest/stormnetboot) (the boot
server and the post-`switch_root` agent), and
[stormuefi](https://github.com/glennswest/stormuefi) (the local boot chain an
assimilated node ends up on).
