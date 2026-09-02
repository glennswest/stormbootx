# stormbootx

**A UEFI NVMe/TCP boot extension. No kernel, no initramfs, no PXE.**

A 45 KB UEFI application on a USB stick. It reads the machine's service tag out
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
| `stormbootx.efi` | **45 KB** |
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
| `registry.rs` | claim an image from sbregistry, keyed on the service tag |
| `config.rs` | the target, read from the media rather than compiled in |

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

## Configuration lives on the media

`\stormboot\stormboot.conf` on the ESP, found through
`EFI_LOADED_IMAGE_PROTOCOL` — the exact volume this image was loaded from, so
there is no probing for "something that looks like our ESP" and no risk of
writing to a partition that belongs to somebody else.

```ini
portal = 192.168.31.202
port   = 4420
nqn    = nqn.2026-09.lo.g16:stormcos
nsid   = 2
```

Compiled values are only a floor. This matters more than it sounds: over one
afternoon the target moved host (`dev.g8.lo` → `forge.g16.lo`) and then changed
NQN (`…lo.g8` → `…lo.g16`), and each compiled-in value meant a machine that
could not boot until someone rebuilt and rewrote a stick.

DNS-based discovery belongs above this and is not built yet — see the issues.

## Building

Builds on `dev.g8.lo`, never a workstation.

```bash
cargo build --release --target x86_64-unknown-uefi
./scripts/build-boot-agent.sh --portal 192.168.31.202 --nqn nqn.2026-09.lo.g16:stormcos --nsid 2
dd if=/build/images/stormbootx.img of=/dev/sdX bs=4M conv=fsync
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
`ConnectController` pass over every handle does not change that.

## Status

Built and verified as an artifact; **not yet run on hardware**. The first line
to watch is `tcp4 : available`.

Related: [stormnetboot](https://github.com/glennswest/stormnetboot) (the boot
server and the post-`switch_root` agent), and
[stormuefi](https://github.com/glennswest/stormuefi) (the local boot chain an
assimilated node ends up on).
