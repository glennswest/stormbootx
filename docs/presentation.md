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

v0.4.0 (+ #15 universal boot) · `x86_64-unknown-uefi` · `no_std` · ~115 KB

No kernel, no initramfs, no PXE, no TFTP. It uses the firmware's own TCP stack.

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
stormcentral relationships (stormcentral check):
  stormbootx   kind: boot   → stormblock        nothing depends on it
  stormuefi    kind: boot   → stormblock        stormcos → stormuefi
```

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
 tag= / SMBIOS / NIC  ──►  identity (stated tag, serial, MAC)
                        │
 engine :9090   ──►  GET  /api/v1/health               version: default claim safe?
                ──►  GET  …/boothost/<tag>/intent    local → fall through
                ──►  POST …/boothost/default/claim {mac}   (or …/<tag>/claim)
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

- **Identity** (`smbios.rs`, `config.rs`, `tcp4::machine_mac`): `tag =` wins;
  else SMBIOS Type 1 → 2 → 3 serial; and the lowest usable NIC MAC.
  Placeholders (`Default string`, `To be filled by O.E.M.`, …) are rejected.
- **Boot intent** (`intent.rs`): `install` / `local` / `auto`. Only an explicit
  `local` skips the claim; **any doubt reads as `auto`**.
- **Claim** (`registry.rs`, `universal.rs`): plain HTTP/1.1 over TCP4. No
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
- **Network** (`tcp4.rs`, `dhcp4.rs`): every NIC tried, ranked link-up then
  largest MTU; waits up to 5 s for a late network stack; runs its own DHCP
  when the platform didn't.
- **NIC FEC report** (`mlxfec.rs`): prints each ConnectX port's FEC, read-only.

---

## When it fails: the fall-through

A boot path must never need the network in order to boot without it.

1. Prints `no network boot: <reason>`.
2. Offers a console for **5 s** (`press c`); silence continues.
3. Local disk present → falls through at once. None → says so, waits 30 s.
4. Returns `EFI_ABORTED`; the boot manager tries the next option.

Console (`shell.rs`): `nics` · `state` · `dhcp [n]` · `connect IP PORT` ·
`pci [all]` · `fec [MODE]` · `reset` · `boot`

---

## Interfaces

**No listener, no health or metrics endpoint.** It is a UEFI app; it reports
on the console. Outbound only:

| To | Default | What |
|---|---|---|
| engine API | `<portal>:9090` | `GET /api/v1/health`, `GET …/boothost/<tag>/intent`, `POST …/boothost/{default,<tag>}/claim` |
| NVMe/TCP portal | `<portal>:4420` or the claim's | the attach |

The claim and health are the only engine calls open without a token (stormblock v17).
Firmware has nowhere to keep one.

---

## Configuration: `\stormboot\stormboot.conf`

Read from the volume it booted from. `key = value`, each key independent.

| Key | Default | |
|---|---|---|
| `portal` | `192.168.31.202` | portal and engine host |
| `port` / `api_port` | `4420` / `9090` | NVMe/TCP / engine API |
| `nqn` / `nsid` | `…lo.g16:stormcos` / `2` | used only if the claim fails or is off |
| `claim` | `yes` | `no` pins the stick to `nqn`/`nsid` |
| `tag` | SMBIOS | states the identity |
| `fec` | — | recovery sticks only: write FEC, warm-reset |

---

## How it ships and is operated

- **Built** with `sc-build` on the build box: the `.efi` plus the two host
  test suites (`intent.rs`, `sha256.rs`, 7 tests each).
- **Packaged** by `scripts/build-boot-agent.sh` into a GPT `.img` (USB stick)
  or `--iso` (iDRAC virtual media). Variants: `--pin`, `--probe`
  (`tcp4probe`), `--fec`.
- **Golden kind: none.** It is not a stormcos component; the media is
  carried to the machine. How it should be published is open (#21).
- **Updated** by rewriting the stick. Self-update (#2) is planned.
- **Firmware needs `EFI_TCP4`** (on Dell: *UEFI Network Stack* on).
  `tcp4probe` checks a new server model first.

---

## Proven on hardware

2026-09-05, Dell PowerEdge R230 (C2NR0Q2), ISO over iDRAC virtual media,
25 GbE ConnectX-4 Lx, through to stormuefi and a running stormcos:

```
service tag : C2NR0Q2
claim       : boothost/C2NR0Q2 at 192.168.31.202:9090
  portal    : 192.168.31.202:4420  nsid 7
  namespace : 8388608 blocks x 4096 bytes  (32 GiB)
  transfer  : 128 KiB per command  (controller MDTS 5; path MTU 1500)
RESULT: image attached; starting its bootloader.
```

---

## Planned — not in the code yet

| | What | Waiting on |
|---|---|---|
| #3 | boot local unless a new golden **and** an install request | stormcos#30, decision #19 |
| #11 | intents take effect | stormblock#148 (engine route) |
| #15 | universal boot served | a stormblock release with #200 on forge (stormblock#194), `boothost/default` set, stormblock#202 |
| #7 | NIC MAC as the last-resort identity | a definition shared with stormipmi#14 |
| #4 | report inventory before any OS | decision #20 |
| #2 | self-update the stick, digest-verified | #4, decision #21 |
| #10 | NVMe/TCP initiator as a shared crate | stormboot4bios |
| #14 | test containers | decision #22 |

---

## Status

- **v0.4.0**, running on metal since 2026-09-05.
- The intent read and universal boot are in the binary, and inert until the
  engine serves them: today every intent read is a 404 (→ `auto`), and forge
  runs stormblock 13.7.0, before #200, with no `boothost/default`, so it is
  claimed by serial as before.
- **P0:** #3 and #11 — together they end the fresh clone on every boot.
- **Decisions open:** #19 (compare key), #20 (inventory), #21 (publishing),
  #22 (test approach).

Source: https://github.com/glennswest/stormbootx
