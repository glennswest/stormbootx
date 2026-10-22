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

## Phase 1: the API, commands and the hold (built, v0.33.0)

The owner (2026-10-21): "more important to have api control". So the
commands do not wait for a poll of the engine: **stormbootx answers HTTP on
port 9180 of every leased NIC** from the moment its stack is up until the
image's bootloader starts (`src/api.rs`; README, *The API, commands and the
hold*):

```
GET  /api/v1/status      phase, detail, names, addresses, the floor, pending/last command
GET  /api/v1/inventory   phase 0's inventory
GET  /api/v1/log         every console line so far
POST /api/v1/command     a signed command → 202 {"accepted":<id>,…} | 400 | 403 | 409
```

The engine's host record carries the same signed object, for when the API
can't be reached (stormblock#494):

```
GET  /api/v1/synonyms/boothost/<name>/command
  → 200 {"host":"server8","id":1792000000123,"command":"reboot","args":"",
         "issued":"2026-10-21T22:01:00Z","by":"stormcentral","sig":"<hex>"}
  → 404 (none)
POST /api/v1/synonyms/boothost/<name>/command/done  {"id":…,"result":"…"}
```

stormbootx reads it under the name the intent answered for, once before
the intent, and every 5 s while holding. Every doubt (404, unreachable,
unsigned, a bad signature, an `id` it has taken, another machine's name) is
**no command**, and the boot goes on exactly as without one.

| command | what stormbootx does |
|---|---|
| `reboot` | `ResetSystem(EfiResetCold)` |
| `poweroff` | `ResetSystem(EfiResetShutdown)` (ACPI S5) |
| `boot-next <NNNN>` | set `BootNext` to `Boot<NNNN>` (it must exist), reset |
| `install`, `local` | as the intent of that name, this boot only |
| `retry` | the boot again from the engine |
| `inventory` | collect again, served by `/api/v1/inventory` |
| `hold` | stay in stormbootx: panel up, log streaming, API answering, until another command |
| `format <serial> [fast]` | zero the first and last MiB of that one drive; `full` and `secure` are baremetalservices' |
| `maint` | claim the maintenance image and boot it: the usual claim with `"image":"maint"` (stormblock#497), down the same names, answering this host's own `maint-<host>` clone of `boothost/maint`'s volume. Never the media's fallback; a reply without `"image":"maint"` (an engine before #497, `stormboot-serve`) is not attached; no `boothost/maint` (a 404) or either of those is a failed boot, so it holds. Before the chain-load it boots again for the maintenance image |

**Holding.** A failed network boot with a lease holds for `hold =` seconds
(the goldens: 120; the binary's default: off), then falls through as every
failure did; one with nothing local that boots holds until a command.
`hold_retry =` boots again every N minutes. install-config.yaml's
`stormbootx_hold:` / `stormbootx_hold_retry:` when the media says neither.

Power-**on** cannot come from inside the machine: Wake-on-LAN from
stormcentral or the laptop app (WoL switched on in setup by
baremetalservices' `/bios/configure`), the BMC where it still works.

### Authentication (decided: Ed25519)

The master's decision (2026-10-22) is option 1, verified with the
self-update's key: commands signed by stormcentral's key compiled in
(`manifest::RELEASE_KEYS`), or by the laptop app's key named on the media
as `command_key =`. The signed text's first line,
`stormbootx-command v1`, keeps a signed manifest from ever reading as a
command. Commands must never be open on the network. The manifest's lesson
holds: the reply comes over plain HTTP from a name DHCP gave, so **only a
signature counts**. The signed text is
`stormbootx-command v1\n<host>\n<id>\n<command>\n<args>\n<issued>\n`: the
host name ties it to one machine, and the `id` is a replay floor kept in a
non-volatile variable (`StormBootCommandId`), as `StormBootMinSerial` is
for updates. Nothing secret is on the media; whoever can write the media
already decides what boots.

Option 2, HMAC with install-config's token, was declined: a medium without
an install-config (the BMC virtual-media ISO every blade boots) could take
no command, and the token would be a secret on a readable stick.

## Phase 2: console input — dropped

**Master decision (2026-10-22, on #167): no console input in stormbootx.**
The API already shows the log and status and takes every command, which is
everything stormbootx's own failure shell does. The prompts a person must
answer come from the installer and the maintenance image, after stormbootx
has handed off and its network is gone, so their console input is theirs
to carry (stormblock#477, baremetalservices). The design that was here (a
signed `console <session key>` command, MAC'd keystrokes into stormbootx's
`ConIn`) would have reached nothing after the hand-off.

## Phase 3: the controls

stormcentral's Test machines page and the laptop app set commands, show
the inventory and the log, and send Wake-on-LAN. Those are stormcentral's
and the laptop app's (stormblock's `stormboot-serve`); stormbootx's part
ends at the command record and the inventory.

## Who does what

| piece | repo |
|---|---|
| Inventory v2, the API, signatures, reset, `BootNext`, `hold`, the fast wipe, `maint`'s claim | stormbootx |
| Console input for the installer and the maintenance image | stormblock#477, baremetalservices |
| `…/command` and `…/command/done` on the host record, the inventory kept | stormblock (forge and `stormboot-serve`) |
| The command key, signing, the Test machines controls, WoL | stormcentral |
| `boothost/maint` and its claim (`"image":"maint"`) | stormblock#497 (v22.3.0); forge's `boothost/maint` set to the image |
| The maintenance image itself (ESP + UKI, sealed) | baremetalservices#2 |
| A BMC cold reset without touching users | baremetalservices `/ipmi` |
