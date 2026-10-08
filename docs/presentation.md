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

v0.23.1 (2026-10-08) · `x86_64-unknown-uefi` · `no_std` · ~320 KB

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

## What it does today (1/2): choosing and attaching

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
- **Attach** (`nvme.rs`): host NQN `nqn.2026-09.lo.storm:host-<name>`;
  PSDT = SGL on every command; `CC.EN` before admin commands; transfer size
  from the controller's **MDTS** (128 KiB against stormblock), never the MTU.

---

## What it does today (2/2): booting it

- **Publish** (`blockio.rs`): BlockIO with the namespace's real block size
  (FLBAS, so 4096 on a 4K namespace), plus a vendor device-path node, then
  `ConnectController`.
- **Chain-load**: loads `BOOTX64.EFI` only from an ESP whose device path
  starts with that vendor node — never a local disk. It does not hand back to
  the boot manager: a disk that appears mid-boot is not in `BootOrder`.
- **Reads the ESP itself when the firmware can't** (`esp.rs`, #37): volumes
  stay 4K, and old firmware (AMI Aptio 4) misreads a 4096-byte-sector FAT.
  GPT → ESP → FAT12/16/32 at 512..4096-byte sectors → `LoadImage` from the
  buffer. `esp =` picks one reader alone.
- **NIC drivers from the media** (`drivers.rs`, #26): every `*.efi` in
  `\stormboot\drivers` is started after the platform's own drivers bind, so
  it only takes NICs nothing else drives. Each step is printed, so a hang
  names itself.
- **Network** (`net.rs`, `entropy.rs`, #56): its own TCP/IP, smoltcp on each
  NIC's SNP (opened exclusively), with no firmware TCP4 anywhere. DHCP on
  every NIC at once; a connection goes out on the NIC that worked last, else
  link-up, then the largest MTU. The ISN and ports are seeded from the
  firmware RNG, then RDRAND, then jitter.
- **Clock** (`clock.rs`, `sntp.rs`, #77): one SNTP exchange (DHCP option
  42, else `ntp =`, else `pool.ntp.org`); `SetTime` in UTC when the RTC is
  more than 1 s off. The X9 blades have no RTC battery.
- **Hand-off to Linux** (`handoff.rs`, #76): volatile EFI variables
  `StormBootTag`, `StormBootHostNqn`, `StormBootClock`, so the initramfs
  claims as the machine stormbootx claimed as.
- **NIC FEC report** (`mlxfec.rs`): prints each ConnectX port's FEC, read-only.

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
| `claim` | `yes` | `no` pins the stick to `nqn`/`nsid` |
| `name` (or `tag`) | DNS name, MAC, SMBIOS | states the identity |
| `dns` | — | PTR server when the lease names none (#26) |
| `ntp` | option 42, `pool.ntp.org` | `host[:port]`, or `off` (#77) |
| `local_when_bootable` | `false` | `true`: `auto` boots a bootable local disk (#3) |
| `rng` | `firmware` | first entropy source: `firmware`, `cpu`, `jitter` |
| `media` | — | the label printed under the banner |
| `esp` | `auto` | who reads the ESP: firmware then stormbootx, or one alone (#37) |
| `fec` | — | recovery sticks only: write FEC, warm-reset |

---

## How it ships and is operated

- **Built** with `sc-build` on the build box: three `.efi`s plus six host
  test suites: `intent` 9, `sha256` 7, `universal` 7, `dnsname` 8, `esp` 12,
  `sntp` 8;
  then `espprobe` boots under OVMF against a 4K disk (#37), and stormbootx
  boots against a stub engine and NVMe/TCP target with no firmware network
  stack (`net-ovmf.sh`, #56), and an ISO's `startup.nsh` starts it from
  the old EDK shell and OVMF's Shell 2.x (`shell-ovmf.sh`, #60).
- **Ships as goldens** (#21, decided 2026-09-28), written by
  `deploy/build-golden.sh`: `stormbootx` (no NIC drivers: the firmware's own, #52),
  `stormbootx-rustnic` (the Rust stormnic-ixgbe and stormnic-mlx4 drivers,
  #45) and `nic-drivers` (the two Rust drivers; no iPXE anywhere, #91). Both media are stormcentral components of kind
  `media`: `stormcentral component build stormbootx` (or
  `stormbootx-rustnic`) builds a drive golden whose bytes are the ISO.
  Nothing is kept on the build box.
- **Packaging** is `scripts/build-boot-agent.sh`; hand-built variants:
  `--pin`, `--probe`, `--fec` (a recovery stick).
- **No boot option?** The media's `\startup.nsh` (#60) finds the volume
  with `BOOTX64.EFI` and `stormboot.conf` on `fs0`..`fs7` and starts it from
  the firmware's EFI Shell.
- **Updated** by itself (#83): a writable medium with `update =` fetches the current release from stormcentral, checks its Ed25519-signed manifest and every file's SHA-256, swaps the files (old kept as `*.prev`) and restarts; the new set must reach an attach within two starts or the old one comes back. stormcentral's key is compiled in (#86); live once stormcentral promotes a golden.
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
| #37 | which reader loads the X9's 4K ESP; `esp.rs` hardening (#67) | the SOL capture (stormcentral#220) |
| stormblock#204 | a DNS name the engine hasn't seen reaches its host (#23 is closed) | stormblock |
| #69–#75 | more Rust NIC drivers (iPXE retired from every medium, #91) | each driver written and proven on metal |
| #4 | report inventory before any OS | decision #20 |
| #83 | self-update the media (in the binary, OVMF-tested, key compiled in) | stormcentral promoting a golden |
| #10 | NVMe/TCP initiator as a shared crate | stormboot4bios |
| #14 | test containers | a test's engine token (stormcentral#133) |

---

## Status

- **v0.23.1**, running on metal since 2026-09-05.
- Intents (#11) and universal boot (#15) are in the binary and tested
  under OVMF against a stub engine, and inert until the engine serves
  them: forge runs stormblock 13.7.0 (2026-10-06), so every
  intent read is a 404 (→ `auto`, which claims, #3) and there is no
  `boothost/default`.
- **P0:** #37 (the X9 blades' 4K ESP), #46 (the R230's initramfs read).
- **Decisions open:** #20 (inventory).

Source: https://github.com/glennswest/stormbootx
