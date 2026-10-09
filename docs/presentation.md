---
marp: true
title: stormbootx
paginate: true
---

<!--
  Render: npx @marp-team/marp-cli docs/presentation.md   (HTML)
          npx @marp-team/marp-cli docs/presentation.md --pdf
  Every claim here is checkable against the code at the version on the
  title slide. If the code changes, change this with it (CLAUDE.md, rule 6).
-->

# stormbootx

**A UEFI boot agent that attaches a machine's image over NVMe/TCP and boots it.**

v0.24.0 (2026-10-08) · `x86_64-unknown-uefi` · `no_std` · ~320 KB

No kernel, no initramfs, no PXE, no TFTP. It carries its own TCP/IP (smoltcp
on the NIC driver's SNP), so it needs no network stack from the firmware.

---

## The problem it solves

- A machine should boot **the image the fleet assigned to it**, with nothing
  but a small binary on a USB stick or virtual-media ISO.
- *Which* image is a fleet decision kept next to the images: a `boothost`
  synonym on the storage engine. **One ISO boots every machine**: one nothing
  has named claims `boothost/default` by its MAC and gets a clone of its own
  (#15). Moving a machine is a `PUT` on its name, not a visit to it.
- It has to work **before any OS exists**, and it must **never** make the
  network a reason a machine can't boot: every failure falls through to the
  local disk.

---

## Where it sits in stormcos

```
stormcentral relationships (stormcentral check: the project's group):
  stormbootx   group: boot   → stormblock        nothing depends on it
  stormuefi    group: boot   → stormblock        stormcos → stormuefi
```

As components (`stormcentral component list`), stormbootx ships as
`stormbootx` and `stormbootx-rustnic` of kind **media** (and the `-disk`
images, #41), and the NIC drivers as `nic-drivers` of kind **tree**.
stormuefi has no component entry of its own; its ESP golden is
stormcentral#215.

Boot is **two stages**. stormbootx picks and attaches the image; the
`BOOTX64.EFI` it starts on a stormcos image is **stormuefi**, which verifies
and boots a pallet from that disk.

| Machine boots | stormbootx | stormuefi |
|---|---|---|
| over the network, from forge | attaches the image | boots its kernel |
| from its own drive | not involved | boots its kernel |

---

## How it works

```
 \stormboot\drivers ──►  NIC drivers the firmware lacks (after its own bind)
 name= / DHCP / PTR / NIC / SMBIOS ──►  names: stated | DNS name, MAC, serial
                        │
 engine :9090   ──►  GET  /api/v1/health               version: default claim safe?
                ──►  GET  …/boothost/<name>/intent   each name until not 404;
                                                      local → fall through
                ──►  POST …/boothost/<name>/claim, …/default/claim {mac}, …
                                                      → addr, port, NQN, NSID
                        │
 portal :4420   ──►  NVMe/TCP: ICReq · Connect · CC.EN · Identify
                        │
 firmware       ◄──  EFI_BLOCK_IO + vendor device path · ConnectController
                        │   (firmware's own GPT + FAT drivers mount the ESP)
                        ▼
                 LoadImage/StartImage \EFI\BOOT\BOOTX64.EFI  (stormuefi)
```

Any failure on any arrow → **fall through to the local disk**.

---

## What it does today (1/3): choosing and attaching

- **Identity** (`config.rs`, `dnsname.rs`, `net::machine_mac`, `smbios.rs`):
  `name =` wins and is the only name. Else the **DNS name** (DHCP option 12,
  else the PTR over DNS/TCP; a NIC's `server1a` is machine `server1`) claims
  `boothost/server1` (#23); a 404 moves on to the default by the lowest usable
  MAC (#15), then the SMBIOS Type 1 → 2 → 3 serial, then the MAC as a tag.
  Placeholders and a serial the whole chassis shares are rejected (#26).
- **Boot intent** (`intent.rs`): `install` / `local` / `auto`, read under the
  same names in the same order. Only an explicit `local` skips the claim;
  **any doubt reads as `auto`**.
- **Claim** (`registry.rs`, `universal.rs`): plain HTTP/1.1 over its own TCP. No
  `tag =` → `boothost/default` by MAC, a clone per machine (`mac-<hex>`),
  only from an engine after v19.3.0 (stormblock#200); else by serial.
- **Inventory** (`inventory.rs`, `hardware.rs`, #4): every claim carries
  the NICs and their drivers, storage controllers and disks the firmware
  sees, and CPU and memory when there is no BMC; under the engine's 16 KiB.
- **Attach** (`crates/nvme-tcp-initiator`, `nvme.rs`, #10): host NQN `nqn.2026-09.lo.storm:host-<name>`;
  PSDT = SGL on every command; `CC.EN` before admin commands; transfer size
  from the controller's **MDTS** (128 KiB against stormblock), never the MTU.

---

## What it does today (2/3): booting it

- **Publish** (`blockio.rs`): BlockIO with the namespace's real block size
  (FLBAS, so 4096 on a 4K namespace), plus a vendor device-path node, then
  `ConnectController`.
- **Chain-load**: loads `BOOTX64.EFI` only from an ESP whose device path
  starts with that vendor node — never a local disk. It does not hand back to
  the boot manager: a disk that appears mid-boot is not in `BootOrder`.
- **Reads the ESP itself when the firmware can't** (`esp.rs`, #37): volumes
  stay 4K, and old firmware (AMI Aptio 4) misreads a 4096-byte-sector FAT.
  GPT → ESP → FAT12/16/32 at 512..4096-byte sectors → `LoadImage` from the
  buffer, with a read-only `EFI_SIMPLE_FILE_SYSTEM` (`espfs.rs`, #42) on
  the ESP so the bootloader can open its other files. `esp =` picks one
  reader alone. On the X9 blades the firmware's own FAT loads the FAT16 4K
  ESP; the bridge is the fallback.
- **NIC drivers from the media** (`drivers.rs`, #26): every `*.efi` in
  `\stormboot\drivers` is started after the platform's own drivers bind, so
  it only takes NICs nothing else drives, except `prefer_media_drivers =
  virtio` (#108), which takes VMs' virtio-net from OVMF's driver. Each step
  is printed, and a hung SNP call is named (`snpwatch.rs`, #88).
- **Network** (`net.rs`, `entropy.rs`, #56): its own TCP/IP, smoltcp on each
  NIC's SNP (opened exclusively), with no firmware TCP4 anywhere. DHCP on
  every NIC at once; a connection goes out on the NIC that worked last, else
  link-up, then the largest MTU. The ISN and ports are seeded from the
  firmware RNG, then RDRAND, then jitter.
---

## What it does today (3/3): around the boot

- **Clock** (`clock.rs`, `sntp.rs`, #77): one SNTP exchange (DHCP option
  42, else `ntp =`, else `pool.ntp.org`); `SetTime` in UTC when the RTC is
  more than 1 s off. The X9 blades have no RTC battery.
- **Hand-off to Linux** (`handoff.rs`, #76): volatile EFI variables
  `StormBootTag`, `StormBootHostNqn`, `StormBootClock`, `StormBootUpdate`,
  and the media's `install-config.yaml` in chunks (#79), so the initramfs
  claims as the machine stormbootx claimed as.
- **Self-update** (`selfupdate.rs`, `manifest.rs`, #83): a writable medium
  takes a newer Ed25519-signed release from stormcentral, on trial.
- **NIC FEC report** (`mlxfec.rs`, `connectx.rs`): prints each ConnectX
  port's FEC, read-only; a ConnectX-3 says it has none (#25).

---

## When it fails: the fall-through

A boot path must never need the network in order to boot without it.

1. Prints `no network boot: <reason>`.
2. Offers a console for **5 s** (`press c`); silence continues.
3. Sets the clock from NTP if the boot has not (#77), then gives the NICs
   back to the firmware (`net::release`, #56).
4. Local disk present → falls through at once. None → says so, waits 30 s.
5. Returns `EFI_ABORTED`; the boot manager tries the next option.

Console (`shell.rs`): `nics` · `state` · `dhcp [secs]` · `connect IP PORT` ·
`pci [all]` · `fec [MODE]` · `reset` · `boot`

---

## Interfaces

**No listener, no health or metrics endpoint.** It is a UEFI app; it reports
on the console. Outbound only:

| To | Default | What |
|---|---|---|
| DNS server | option 6 (else `dns =`), TCP 53 / UDP 53 | the PTR of its own address (#23); the NTP server's A record (#77) |
| NTP server | option 42, else `ntp =`, else `pool.ntp.org`, UDP 123 | one SNTP exchange (#77) |
| engine API | `<portal>:9090` | `GET /api/v1/health`, `GET …/boothost/<name>/intent`, `POST …/boothost/{<name>,default}/claim` |
| NVMe/TCP portal | `<portal>:4420` or the claim's | the attach |
| stormcentral | the `update =` URL, HTTP | the self-update's manifest, signature and files (#83) |

It sends no token; firmware has nowhere to keep one. The engine leaves the
claim, health and (from stormblock#148) the intent read open.

---

## Configuration: `\stormboot\stormboot.conf`

Read from the volume it booted from. `key = value`, each key independent.

| Key | Default | |
|---|---|---|
| `portal` | `192.168.31.202` | portal and engine host |
| `port` / `api_port` | `4420` / `9090` | NVMe/TCP / engine API |
| `nqn` / `nsid` | `…lo.g16:stormcos` / `2` | used only if the claim fails or is off |
| `fallback` | on | `none`: no `nqn`/`nsid` attach; every golden (#36) |
| `claim` | `yes` | `no` pins the stick to `nqn`/`nsid` |
| `name` (or `tag`) | DNS name, MAC, SMBIOS | states the identity |
| `dns` | — | PTR server when the lease names none (#26) |
| `ntp` | option 42, `pool.ntp.org` | `host[:port]`, or `off` (#77) |
| `local_when_bootable` | `false` | `true`: `auto` boots a bootable local disk (#3) |
| `rng` | `firmware` | first entropy source: `firmware`, `cpu`, `jitter` |
| `media` | — | the label printed under the banner |
| `esp` | `auto` | who reads the ESP: firmware then stormbootx, or one alone (#37) |
| `fec` | — | recovery sticks only: write FEC, warm-reset |
| `nic_verbose` | `false` | stormnic drivers print their full trace (#80) |
| `prefer_media_drivers` | — | `virtio`: take virtio-net from OVMF (#108) |
| `update` | — | the self-update's URL, or `off` (#83) |
| `version`, `commit` | — | written on the media for storminstall; not read (#94) |

---

## How it ships and is operated

- **Built** with `sc-build` on the build box: three `.efi`s plus ten host
  test suites (`intent` 9, `sha256` 7, `universal` 9, `dnsname` 8, `esp` 13,
  `sntp` 8, `manifest` 13, `installconf` 7, `inventory` 6, `connectx` 4)
  and the initiator crate's 7; then `espprobe` boots under OVMF against a 4K
  disk (#37), stormbootx boots against a stub engine and NVMe/TCP target
  with no firmware network stack (`net-ovmf.sh`, #56), an ISO's
  `startup.nsh` starts it from the old EDK shell and OVMF's Shell 2.x
  (`shell-ovmf.sh`, #60), two builds from two paths must match byte for
  byte (`repro.sh`, #53), and the self-update boots off a USB stick
  (`update-ovmf.sh`, #83).
- **Ships as goldens** (#21, decided 2026-09-28), written by
  `deploy/build-golden.sh`: `stormbootx` (no NIC drivers: the firmware's own, #52),
  `stormbootx-rustnic` (the Rust stormnic-ixgbe, stormnic-mlx4 and
  stormnic-virtio drivers, #45, #108) and `nic-drivers` (the three Rust
  drivers; no iPXE anywhere, #91). Every golden names no fallback
  namespace (#36). Both media are stormcentral components of kind
  `media`: `stormcentral component build stormbootx` (or
  `stormbootx-rustnic`) builds a drive golden whose bytes are the ISO.
  Nothing is kept on the build box.
- **Packaging** is `scripts/build-boot-agent.sh`; hand-built variants:
  `--pin`, `--probe`, `--fec` (a recovery stick).
- **No boot option?** The media's `\startup.nsh` (#60) finds the volume
  with `BOOTX64.EFI` and `stormboot.conf` on `fs0`..`fs7` and starts it from
  the firmware's EFI Shell.
- **Updated** by itself (#83): a writable medium with `update =` fetches the current release from stormcentral, checks its Ed25519-signed manifest and every file's SHA-256, swaps the files (old kept as `*.prev`) and restarts; the new set must reach an attach within two starts or the old one comes back. stormcentral's key is compiled in (#86). stormcentral serves `stormbootx-rustnic` serial 1 (v0.14.0), which any newer medium declines as older (#96); a current promotion is #92.
- **Firmware needs only a UEFI driver for its NIC** (SNP), or one on the
  media in `\stormboot\drivers` (#26: the Supermicro X9 blades have
  legacy-only NICs and boot the rustnic medium). No `EFI_TCP4` since #56.

---

## Proven on hardware

2026-09-05, Dell PowerEdge R230 (C2NR0Q2), ISO over iDRAC virtual media,
25 GbE ConnectX-4 Lx. The first attach, verbatim and trimmed (the build
before chain-loading; the same day it went on through stormuefi to a
running stormcos):

```
service tag : C2NR0Q2
claim       : boothost/C2NR0Q2 at 192.168.31.202:9090
  portal    : 192.168.31.202:4420  nsid 7
  namespace : 8388608 blocks x 4096 bytes  (32 GiB)
  transfer  : 128 KiB per command  (controller MDTS 5; path MTU 1500)
RESULT: remote image is a local disk. Firmware can boot it.
```

2026-09-28, Supermicro X9 blade (server1), no UEFI NIC driver in firmware:
`drivers : 1 of 1 started` → `tcp4 : available` → name `server1` from the
PTR → `boothost/server1` claimed → NVMe/TCP attach (#26, from the golden).
Its release disk's `BOOTX64.EFI` was not found at 4096-byte blocks (#33).
2026-09-30: server1 boots release 11.56 from a 4096-byte namespace, once
that release's ESP was FAT16 (stormcos#188); and server3 (X9) booted on
smoltcp, through stormuefi to a running stormcos (#56, #68).

---

## Planned — not in the code yet

| | What | Waiting on |
|---|---|---|
| stormblock#204 | a DNS name the engine hasn't seen reaches its host (#23 is closed) | stormblock |
| #69–#74 | more Rust NIC drivers (iPXE retired from every medium, #91) | paused by the owner at P3 |
| #92, #89 | a current release promoted, and a stick taking it on metal | stormcentral#459 |
| #100 | more than one NVMe/TCP command in flight (~46 MiB/s on metal) | — |
| #84 | arm64 media (`BOOTAA64.EFI`) | the owner; stormcentral#604 |
| #110 | the claim over HTTPS against a fleet CA | stormcos#35 |
| #14 | test containers | a test's engine token (stormcentral#133) |

---

## Status

- **v0.24.0**, running on metal since 2026-09-05; #4, #10, #17, #25, #36,
  #42, #53 and #67 are on `main` for v0.25.0 (#128).
- Intents (#11) and universal boot (#15) are in the binary and tested
  under OVMF against a stub engine, and inert until the engine serves
  them: forge runs stormblock 13.7.0 (2026-10-09), so every
  intent read is a 404 (→ `auto`, which claims, #3) and there is no
  `boothost/default`.
- **P1:** #92 (promote the install-config-capable media), #14.
- **Decisions open:** #84 (arm64 media), #35 (what the fw golden keeps).

Source: https://github.com/glennswest/stormbootx
