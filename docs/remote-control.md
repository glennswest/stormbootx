# stormbootx as the front end of a machine with no working BMC (#167)

The owner (2026-10-21): "enhance stormbootx to a fuller ipmi boot
replacement, with disk format, power control, etc. ie the server8
experience." On server8 the BMC's web login failed TLS, its serial console
died seconds into a boot, and its power button never took. Nothing could see
or manage the machine.

stormbootx runs first on every network boot and already has a network, a
signed-update path and an engine to talk to. This note is the plan for
making it the front end: what it shows, which commands it takes, how they
are authenticated, and in what order. The disk, BIOS and BMC tools are
**baremetalservices'** maintenance image (`/disks`, `/bios/*`,
`/boot/order`, `/firmware/update`, `/ipmi`). stormbootx starts it and does
not reimplement it in UEFI.

## What exists today

| | where |
|---|---|
| Every console line to the network (stormcast multicast and a unicast collector) | #162, `src/netlog.rs` |
| The boot intent (`install`, `local`, `auto`) read before the claim | #11, `src/intent.rs` |
| A firmware inventory with every claim, kept on the host by the engine | #4, `src/inventory.rs`, stormblock#177 |
| Ed25519 signature checks against keys compiled in | #83, `src/manifest.rs` |
| Falling through to the local disk on every failure | #3, #145 |

## Phase 0: the inventory (this repo, no one else's)

The owner's addition on #167: "disk info, number of drives, etc." The
inventory's `"v":2` sends, at every claim, everything the firmware can say
with no OS and no BMC:

- **SMBIOS**: system vendor, product, serial and UUID (Type 1); board
  (Type 2); firmware vendor, version and date (Type 0); CPU and memory
  (Type 4, 17), now always, not only with no BMC (this supersedes #20's
  option 2, because a BMC that answers nothing is the case this is for).
- **Drives**: how many, and for each the bus (`nvme`, `sata`, `scsi`,
  `usb`, `virtio`, `other`), its device path (controller, port or target),
  model, serial, firmware revision, WWN (ATA words 108–111, SCSI VPD page
  0x83 NAA, NVMe EUI-64/NGUID), size, logical and physical sector size,
  media type (`ssd`, `hdd`, `nvme` or none known) and the GPT's partitions
  by type, with the directories under `\EFI` of any ESP (`BOOT`,
  `Microsoft`, `ubuntu`: the old installs).
- **NICs**: MAC, link, the SNP's MTU, driver, PCI address and id. SNP has
  no link speed. The stormnic drivers print theirs on the console.

Sources: `EFI_DISK_INFO` (ATA IDENTIFY, SCSI INQUIRY, NVMe Identify
Namespace), `EFI_NVM_EXPRESS_PASS_THRU` (Identify Controller: model,
serial, firmware), `EFI_SCSI_IO` (VPD 0x80, 0x83, 0xB1) and the disk's own
GPT and ESP through BlockIO, all read-only. The console prints one line per
drive and NIC, so the same list goes to the collector over #162's stream.
The engine keeps it on the host's last claim (16 KiB at most; disks are cut
from the end and counted when a machine has more than fit). stormcentral's
Test machines page and the system-drive picker (stormcentral#1066) read it
from there.

## Phase 1: commands, next to the intent

The engine's host record carries one pending **command** beside the intent:

```
GET  /api/v1/synonyms/boothost/<name>/command
  → 200 {"host":"server8","id":17,"command":"reboot","args":"",
         "issued":"2026-10-21T22:01:00Z","by":"stormcentral","sig":"<hex>"}
  → 404 (none)
POST /api/v1/synonyms/boothost/<name>/command/done  {"id":17,"result":"…"}
```

stormbootx reads it at the intent step, down the same name list, before the
claim. Every doubt (404, unreachable, unsigned, bad signature, an `id` it
has done before, another machine's name) is **no command**, and the boot
goes on exactly as without one.

| command | what stormbootx does |
|---|---|
| `reboot` | `done`, flush the log, `ResetSystem(EfiResetCold)` |
| `poweroff` | `done`, flush, `ResetSystem(EfiResetShutdown)` (ACPI S5) |
| `boot-next <NNNN>` | set `BootNext` to `Boot<NNNN>` (it must exist), `done`, reset |
| `maint` | claim and attach the baremetalservices maintenance image as it does a host's image, and start its UKI |
| `install`, `local` | as the intent of that name, this boot only |
| `inventory` | collect again, print it and send it with `done` |
| `hold` | stay in stormbootx: panel up, log streaming, the command read again every 5 s, until another command |

The engine clears a command when its `done` arrives; a command nobody
collects stays pending until the next boot. Power-**on** cannot come from
inside the machine: Wake-on-LAN from stormcentral or the laptop app (WoL
switched on in setup by baremetalservices' `/bios/configure`), the BMC where
it still works.

### Authentication (the owner's choice)

Commands must never be open on the network. The manifest's lesson holds:
the reply comes over plain HTTP from a name DHCP gave, so **only a
signature counts**. The signed text is
`stormbootx-command v1\n<host>\n<id>\n<command>\n<args>\n<issued>\n`: the
host name ties it to one machine, and the `id` is a replay floor kept in a
non-volatile variable (`StormBootCommandId`), as `StormBootMinSerial` is
for updates.

Who can sign:

1. **Ed25519, keys compiled in and named on the media.** stormcentral gets a
   *command* key (not the release key: that one signs media, and should not
   sign at the rate commands come). Its public half is compiled in beside
   `RELEASE_KEYS`. The laptop app makes its own key pair and writes the
   public half as `command_key =` in the `\stormboot\local.conf` of the
   media it writes. Nothing secret is on the media; whoever can write the
   media already decides what boots.
2. **HMAC-SHA256 with the machine's token from `install-config.yaml`.**
   No new key anywhere, but a medium without an install-config (the BMC's
   virtual-media ISO every blade boots) can take no command, and the
   token is a secret on a stick anyone can read.

**Recommended: 1.** It works on every medium stormcentral serves, keeps no
secret on the media, and reuses the verify code the self-update already
runs.

## Phase 2: console input

#162 sends the log; nothing comes back. Input rides on phase 1's
authentication rather than a second scheme: a signed `console <collector>
<session key>` command makes stormbootx open **one outbound** TCP connection
to the collector (stormbootx still listens on nothing), take keystrokes
from it, each frame MAC'd with that session key, and feed them to its own
`ConIn` beside the keyboard. It lasts while stormbootx runs. A UKI it starts
(baremetalservices, the installer) carries its own console over the
network; that is stormblock#477's side. This comes after commands because
it needs their authentication. The issue lists it first; this note orders
it second.

## Phase 3: the controls

stormcentral's Test machines page and the laptop app set commands, show
the inventory and the log, and send Wake-on-LAN. Those are stormcentral's
and the laptop app's (stormblock's `stormboot-serve`); stormbootx's part
ends at the command record and the inventory.

## Who does what

| piece | repo |
|---|---|
| Inventory v2, command reader, signatures, reset, `BootNext`, `hold`, console input | stormbootx |
| `…/command` and `…/command/done` on the host record, the inventory kept | stormblock (forge and `stormboot-serve`) |
| The command key, signing, the Test machines controls, WoL | stormcentral |
| A maintenance image the engine can serve as a namespace (ESP + UKI) | baremetalservices |
| A BMC cold reset without touching users | baremetalservices `/ipmi` |
