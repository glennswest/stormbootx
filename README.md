# stormbootx

**A UEFI boot agent that attaches a remote disk over NVMe/TCP and boots it.**
No kernel, no initramfs, no PXE, no TFTP. It carries its own TCP/IP stack
(smoltcp, on the NIC driver's `EFI_SIMPLE_NETWORK`) and needs nothing above
the NIC driver from the firmware.

It runs from a USB stick or a virtual-media ISO. It loads any NIC drivers the
media carries, works out which machine it is (a stated name, else its DNS
name, else its MAC or SMBIOS serial), claims that machine's image from the
storage engine, attaches it over NVMe/TCP, publishes it as `EFI_BLOCK_IO_PROTOCOL`, and
chain-loads the `\EFI\BOOT\BOOTX64.EFI` on the attached disk. If any step
fails, or the machine's boot intent is `local`, it falls through to the local
disk.

```
media NIC drivers → smoltcp on SNP (DHCP) → names (conf | DNS name, MAC, serial) → boot intent
    → claim boothost/<name> (or default by MAC) → NVMe/TCP attach
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

At boot it **reads** `\stormboot\local.conf` and `\stormboot\stormboot.conf`
from the volume it was loaded from (and `\stormboot\state`, the self-update's
record), and **loads** any `*.efi` in `\stormboot\drivers\` on that volume
as a NIC driver (#26; absent on ordinary media). It writes to these:

| What | When |
|---|---|
| the attached clone on the engine | the booted OS writes to its disk, and the BlockIO handle is read-write |
| the ConnectX NV FEC setting | only with `fec =` on a recovery stick, or `fec MODE` typed at the failure console; followed by a warm reset |
| the hardware clock (RTC), through UEFI `SetTime` | a boot whose NTP server answers while the RTC is more than a second off (#77); UTC, time zone and daylight left as the firmware had them |
| **volatile** EFI variables, `StormBootTag`, `StormBootHostNqn`, `StormBootClock`, `StormBootUpdate` and `StormBootInstallConfig` (+ its chunks) | every boot that names the machine, attaches an image, asks NTP, has a self-update state or finds an `install-config.yaml` on the media (#76, #77, #83, #79); gone at the next reset |
| its own boot medium: `\EFI\BOOT\BOOTX64.EFI`, `\startup.nsh`, `\stormboot\stormboot.conf`, `\stormboot\drivers\*` (with `*.new`/`*.prev` copies), `\stormboot\state`, and `\stormboot\local.conf` only to keep a `name =` | a writable medium (USB stick, local ESP) with `update =`, when a newer signed release verifies, and the starts of its trial (#83, *Self-update*) |
| one **non-volatile** EFI variable, `StormBootMinSerial` | when a self-updated medium passes its trial (#83): the highest release serial this machine has run, so an older signed release can't be replayed onto it |

Nothing is written to the platform's network configuration any more (#56;
until 0.9 a `STATIC` IP4 policy was switched to `DHCP` in NVRAM). The NICs are
taken from the firmware's own stack for the boot (SNP opened exclusively) and
given back on the fall-through.

## What it does, step by step

`src/main.rs`, `run()`:

0. **A self-update on trial** (#83, `src/selfupdate.rs`), before anything
   else: counts this start, or, after two starts that never attached, puts
   the previous files back and chain-loads them. See *Self-update*.
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
     (`src/dnsname.rs`; microdns answers over TCP, and TCP is the one
     transport this binary carries). **microdns sends no option 12**: it puts the
     reservation's name in DNS, so on microdns networks the PTR is the path.
     When the reply names no DNS server, or no reply was kept for that
     interface, `dns =` in `stormboot.conf` is asked instead; each way this comes
     up empty is printed. The engine knows hosts by the first label, so
     `server3.g10.lo` claims `boothost/server3`. The console prints
     `name : server3.g10.lo (from DHCP)`. A name that is not a valid DNS name
     is ignored, not guessed at.
   - **A reservation names the NIC, not the machine** (#26): a label ending
     in a digit and one lowercase letter drops the letter, so `server1a` and
     `server1b` are both the machine `server1` (`dnsname::machine_label`), and
     the console says so.
   - Otherwise SMBIOS (`src/smbios.rs`, via the `_SM3_` or `_SM_` entry in the
     EFI configuration table): the serial of Type 1 (System), then Type 2
     (Baseboard), then Type 3 (Chassis). The first one that is not a
     placeholder is used.
   - **A serial shared by several machines is rejected like a placeholder**
     (#26): a Type 1 serial equal to the Type 3 serial on a multi-node chassis
     (chassis type multi-system, blade or blade enclosure) is the enclosure's,
     so Type 1 and Type 3 are skipped and Type 2 is tried. `S11075924402016`,
     the chassis serial seven Supermicro X9 blades report, is listed outright.
   - **Placeholders are rejected** (`universal::serial_usable`, host-tested
     against stormipmi's vectors, so both derive the same `boothost/<tag>`,
     #7): empty or NULs, `none`, `unknown`, `na`, `n/a`, `empty`, `invalid`,
     `default string`, `not applicable`, `not specified`, `serial number`,
     `system serial number`, `chassis serial number`, `base board serial
     number`, `0123456789`, `123456789`, anything containing `to be filled`
     or `o.e.m.`, any string made only of `0`, `.`, `-` and spaces, and one
     filler repeated (`FFFFFFFF`, `XXXXXXXX`, `****`, 0xFF bytes).
   - No usable serial is no longer a failure: once the network stack exists
     (step 4) the machine's **MAC** identifies it, the lowest usable unicast
     permanent address across every NIC (`net::machine_mac`), so the answer
     does not depend on driver bind order. Only no serial *and* no MAC falls
     through.
   - The console prints the source, plus the SMBIOS model when there is one,
     and a `mac` line after step 4.
3. **NIC FEC report** (`src/mlxfec.rs`). It prints every ConnectX port's
   current and next-boot FEC, read only. If `fec =` is set in the conf, it
   writes that value and warm-resets once. The next boot then finds nothing to
   change.
4. **NIC drivers from the media, then the network** (`drivers::load_from_media`,
   `net::up`, #56). Any `*.efi` in `\stormboot\drivers` is started first (see
   *The network path*). Then every `EFI_SIMPLE_NETWORK` handle is opened and
   smoltcp starts DHCP on all of them. With no SNP at all it connects every
   handle and waits up to 5 s. Then it falls through, saying the NIC needs a
   UEFI driver. Each NIC's driver is named before its first SNP call
   (`nic 0: driver <name>`, #88). The console prints `tcp4 : smoltcp over
   SNP (nic 0 <mac>, …)` and `rng : firmware | rdrand | rndr | jitter`.
   Then **the self-update** (#83): with `update =` on a writable medium, ask
   stormcentral for the current release and, if it is newer and its signature
   verifies, write it and restart into it (*Self-update*). One `update :`
   line either way.
5. **Where and which.** `config::resolve` gives the portal from the conf,
   falling back to compiled defaults. Unless the conf says `claim = no`, it
   first reads the machine's **boot intent** (`src/intent.rs`):

   ```
   GET http://<portal>:<api_port>/api/v1/synonyms/boothost/<name>/intent
       → 200 {"host":"server1","intent":"local","updated_at":…}  or 404

   (A stated name is the only one asked. Otherwise it goes down the names the
   claim tries, in the same order, and a 404 moves on to the next: the DNS
   name, the MAC's twelve hex digits for a machine that claims the default,
   then the serial. The engine resolves each through the same host table, so
   a DNS name it does not know yet can't hide an intent set on the host it
   knows by MAC.)
   ```

   | intent | what stormbootx does |
   |---|---|
   | `install` | claims and boots the image, as below |
   | `local` | falls through to the local disk at once: no claim, no clone |
   | `auto` | claims and boots; with `local_when_bootable = true`, a bootable local disk first (below) |

   Set a state on the engine and power-cycle the machine to get it. **Any
   doubt reads as `auto`**: a 404, a non-2xx, an unreachable engine, or a body
   with no intent or an unknown one.

   **`auto` claims, as every doubt does** (owner, 2026-09-30): until intents
   work on forge (stormblock#148), every boot claims and boots or installs
   from the network. The rule below is in the code and **off by default**;
   `local_when_bootable = true` in `stormboot.conf` turns it on.

   **With it on, `auto` boots what the machine has** (#3). A local disk
   "can boot" when it is a whole, present, non-removable disk other than the
   media stormbootx was loaded from, and its GPT has an ESP carrying
   `\EFI\BOOT\BOOTX64.EFI`. `esp.rs` reads that, with no network and no
   firmware FAT, and the file is found, not read. The console prints
   `local : disk N (…): ESP partition P, FATn, …` and falls through, or
   `local : nothing to boot (…); claiming` with the reason for each disk.
   So a bare machine, or one the engine does not know yet, still claims and
   installs, and an installed one boots itself until someone sets `install`.
   Any bootloader counts, not only stormuefi: a disk with another OS on it
   boots that OS under `auto`. The console prints the intent and, when it defaulted, why. The
   contract is stormblock#148, released in stormblock v20.0.0 but not yet on
   forge (13.7.0 on 2026-10-02), so today every read there is a 404 and every
   boot is `auto`. Setting it is `PUT …/intent {"intent":"local"}` with
   the admin token. Resetting `install` back to `local` is the engine's job: the
   node's OS reports `POST …/installed {volume}` after the flow-over, and the
   claim reply carries `intent` for the initramfs. stormbootx never writes it.

   First it reads the engine's version from `GET /api/v1/health`. That is the
   boot's first request, so it also brings the network up and tells which
   interface to read the DNS name from. Then it claims the machine's image,
   best name first:

   ```
   POST …/api/v1/synonyms/boothost/<stated name>/claim   body {"agent":{…}}
   POST …/api/v1/synonyms/boothost/<DNS name>/claim      body {"agent":{…},"mac":"…","serial":"…"}
   POST …/api/v1/synonyms/boothost/default/claim         body {"agent":{…},"mac":"…","serial":"…"}
   POST …/api/v1/synonyms/boothost/<serial>/claim        body {"agent":{…}}
   ```

   Every claim names the boot agent (#90, stormcentral#286), so stormcentral
   can tell which stormbootx booted a machine, not only which release it
   installed:

   ```
   "agent": {"name":"stormbootx","version":"0.19.0","commit":"649dd07",
             "media":"rustnic ixgbe@728b328 mlx4@0e50017",
             "update_serial":5,"update":"serial:5"}
   ```

   `commit` is the build stamp (absent on an unstamped build), `media` the
   medium's `media =` label, `update_serial` the self-update serial of the
   files on the medium (absent when no update wrote them), and `update` the
   `StormBootUpdate` value at claim time (`serial:<n>`, `trial:<n>:<start>`,
   `failed:<n>`; absent when unset). The engine ignores fields it does not
   know; keeping it on the host's claim record is stormblock#177.

   A stated name is the only one tried. Otherwise a 404 moves on to the next
   line, and any other failure falls back to the resolved target. The DNS
   claim carries the MAC and serial so the engine can tie a name it has not
   seen to a host it already knows (stormblock#204).

   The reply supplies the address, port, NQN and NSID. Both `address`/`port`
   and `traddr`/`trsvcid` spellings are accepted, with port defaulting to
   4420 and NSID to 1. Any claim failure falls back to the conf's own
   `nqn`/`nsid` rather than failing.

   **Universal boot: one medium for every machine** (#15, stormblock#200).
   A machine whose media states no name, and whose DNS name the engine does
   not know (or that has none), claims `boothost/default` and says which
   machine it is with its MAC. The engine gives it a copy-on-write
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
5a. **Set the hardware clock from NTP (#77, `src/clock.rs`, `src/sntp.rs`).**
   The X9 blades have no RTC battery, and neither their BIOS nor their BMC
   sets the clock. Once the claim is decided the network is up and leased, so
   stormbootx sends one SNTP request (RFC 4330, UDP 123) to the server in
   DHCP option 42 of the lease on the NIC that reached the engine, else to
   `ntp =` in `stormboot.conf` (`host[:port]`), else to `pool.ntp.org`. A
   name is looked up with one A query over UDP to the lease's DNS server (or
   `dns =`). Two tries of a second each, for the lookup and for the request;
   a reply is believed only from a synchronised server (mode 4, LI ≠ 3,
   stratum 1..15) that echoes the request's random transmit timestamp. When
   NTP and `GetTime` differ by more than a second, `SetTime` writes UTC
   (Linux reads the RTC as UTC), keeping the firmware's time zone and
   daylight fields so EDK2 writes no NV variable. One line says what happened:
   `clock : was 2026-01-01 00:00:05, set to 2026-10-01 21:14:03 UTC from
   162.159.200.1 (step +23663638 s)`, `clock : … UTC from …; the RTC agrees
   (within 1 s)`, or `clock : NTP unreachable at … (…); left at …`. It never
   stops the boot. A boot that falls through instead (a `local` intent, a
   failed attach) does it on the way out, before the NICs are released.
   `ntp = off` leaves the clock alone. The outcome goes to Linux as
   `StormBootClock` (step 6a): `synced:<server address>` when the RTC is
   known to be right, else `unsynced`, so stormcos#213 can trust it or step
   the clock itself.
6. **Attach** (`src/nvme.rs`). The host NQN is
   `nqn.2026-09.lo.storm:host-<name>`, so the target knows which machine is
   connecting: the engine's name for the machine from the claim reply
   (`mac-<hex>` for one it booted as the default), else the DNS name, else the
   tag. The console prints the namespace geometry and the transfer
   size.
6a. **Hand the name down to Linux (#76, `src/handoff.rs`).** The initramfs
   claims `boothost/<name>` again, and its own guess is the SMBIOS serial,
   which on the X9 MicroCloud blades is the chassis's: server8 booted its own
   image and then laid its disk from server1's (stormblock#249). So before
   the loader starts, stormbootx sets two volatile EFI variables under vendor
   GUID `ab361f54-0166-44a4-a088-1ac22e98ab76`, attributes
   `BOOTSERVICE_ACCESS | RUNTIME_ACCESS` (0x6, never `NON_VOLATILE`), ASCII
   with no NUL:
   - `StormBootTag`: the name claimed on: the claim reply's host when it
     named one, else the name claimed (`server8`, `C2NR0Q2`, `mac-<hex>`).
     With nothing claimed (the engine down, a `local` intent), only a name
     stormbootx was given or found: the stated `tag =`, else the DNS name;
     never a serial or a MAC guess. Set as soon as the claim is decided, so a
     failed attach that falls through still names the machine.
   - `StormBootHostNqn`: the host NQN of step 6, once the attach worked.
   - `StormBootClock` (#77): `synced:<NTP server address>` or `unsynced`,
     from step 5a (or the fall-through).
   - `StormBootUpdate` (#83): the boot medium's self-update, `serial:<n>`,
     `trial:<n>:<start>`, `good:<n>` or `failed:<n>` (*Self-update*).
   - `StormBootInstallConfig` (#79): the media's `install-config.yaml`, as a
     header and chunks (*install-config.yaml*, below). Set before step 1.
   A value outside `[A-Za-z0-9._:-]` (Linux ignores those) is not set. Each
   is printed as `handoff : …`; a failure to set is printed and not fatal.
   Linux reads them from `/sys/firmware/efi/efivars/<Name>-<guid>` (four
   attribute bytes, `06 00 00 00`, then the value) ahead of
   `rd.stormblock.tag=` and SMBIOS (stormblock `docs/boot-hooks.md`,
   "Whose image").
7. **Publish** (`src/blockio.rs`). BlockIO is installed with the namespace's
   own block size (read from FLBAS, so 4096 on a 4K namespace). A device path
   of one hardware vendor node (`6d7a1f2e-9c34-4b8a-b1d0-5e2f7a0c9b41`) goes
   on the same handle, then `ConnectController` recursively.
8. **Chain-load.** It looks for `\EFI\BOOT\BOOTX64.EFI` on a filesystem whose
   device path **starts with that vendor node**, which means an ESP on the
   attached disk only. It is never a local disk. `LoadImage` + `StartImage`.
   Success never returns.
9. **Read the ESP itself when the firmware can't (#37, `src/esp.rs`).**
   Volumes stay 4096-byte, so a 4K image's ESP is a FAT with 4096-byte
   sectors (Linux mounts nothing smaller than the device's block). AMI Aptio
   4 mounts one and then answers `NOT_FOUND` for a file that is there. If
   step 8 loads nothing, stormbootx reads the GPT (CRC-checked), finds the
   ESP, reads `BOOTX64.EFI` from its FAT12/16/32 at any sector size from 512
   to 4096, and `LoadImage`s the bytes from the buffer, with the device path
   `Vendor(disk)/HD(n,GPT,…)/\EFI\BOOT\BOOTX64.EFI`. The console says
   `boot : the firmware did not load it (…); reading the ESP here`.
   stormuefi needs nothing more: it reads its pallets through whole-disk
   BlockIO and parses the GPT itself. No filesystem protocol is installed, so
   a bootloader that opens further files on its own ESP (GRUB, shim) would
   not find them this way. `esp = firmware` / `esp = stormbootx` picks one
   reader alone. It is read at start-up with the rest of `stormboot.conf`:
   nothing on the media is opened once an image is attached (#46).
10. **Report the bootloader's reads (#46).** While the image's bootloader
   (stormuefi) reads through the published BlockIO, stormbootx prints a
   `blockio :` line for a read that fails (with the NVMe/TCP error, which
   the firmware only sees as `DEVICE_ERROR`), a single read slower than 2 s,
   and every 64 MiB read with its MiB/s. A boot that stalls inside the
   bootloader then shows whether reads crawl, fail, or stopped being asked
   for.

### install-config.yaml

A node's first-boot input (stormcos#82, stormcos `docs/INSTALL-CONFIG.md`:
cluster name and domain, `sshKey`, `pullSecret`, `apiToken`, `hosts[]`)
travels **inside the boot media** (owner, 2026-10-02, #79). storminstall
writes it, stormbootx hands it down, the initramfs puts it in
`/state/config/install-config.yaml` on a first boot, and stormpump applies it.

**The slot** (storminstall `docs/config-slot.md`):
`\stormboot\install-config.yaml` on the media's ESP, the `STORMBOOTX` FAT
volume `build-boot-agent.sh` makes, beside `stormboot.conf`. No partition of
its own: the ESP is the one volume firmware reads however the media is
presented (El Torito image on a CD, the isohybrid MBR 0xEF / GPT entry on a
USB stick, the GPT ESP of the `.img`), and storminstall writes it with plain
FAT file I/O. At most 256 KiB; a real one is about 1 KiB. For it to keep
working, the ESP keeps its `\stormboot\` directory, `stormboot.conf`, and
some free space (the 4 MiB FAT12 ESP has ~4 MiB less any NIC drivers).

**The hand-down** (`src/installconf.rs`, `handoff::set_install_config`):
read with the rest of the media before step 1, whatever the boot does next,
into volatile EFI variables under #76's GUID, attributes 0x6. A firmware
variable may be capped at 1 KiB (EDK2's default `PcdMaxVariableSize`), so the
file goes in chunks:

| variable | value |
|---|---|
| `StormBootInstallConfig0` … `StormBootInstallConfig<N-1>` | the file's bytes, 768 each, the last one shorter |
| `StormBootInstallConfig` | `v1:<length>:<N>:<sha256, lower-case hex>` |

The header is set last, and a chunk the firmware refuses takes the ones
already set with it, so a header means the whole file is there. A reader
concatenates the `N` chunks (each efivarfs file is four attribute bytes and
then the data) and uses the result only if its length and SHA-256 match. The
console prints `install cfg : none on the media (…)`, or `handoff :
StormBootInstallConfig (<length> bytes in <N> chunk(s)) = v1:…`, and never the
content: it carries the pull secret and the API token. An empty file, one over
256 KiB, or one the variable store can't hold is reported and not handed down.

The variables are world-readable through efivarfs once Linux is up, so the
initramfs deletes them after copying (stormblock's side). The copy is made
only when `/state` has none, so booting old media again never overwrites a
node's applied config.

### Self-update

A medium written once must not stay on that version forever (#83; the X9
blades boot from USB sticks). `src/selfupdate.rs`, with the format and every
decision in `src/manifest.rs` (core-only, host-tested). The design is the
owner's (2026-10-02); stormcentral's half, the signing and the serving, is
stormcentral#279.

- **Where.** `update = http://stormcentral.g8.lo/api/v1/boothelpers/<golden>`
  in `stormboot.conf`, written by the golden build. stormbootx GETs
  `<url>/current` (the manifest), `<url>/current.sig` (an Ed25519 signature
  over those exact bytes, raw or hex) and `<url>/files/<path>`. No token.
  `update = off` pins a medium; no `update =` (media built by hand) checks
  nothing; a read-only medium (an ISO on virtual media) is skipped.
- **The manifest** is text:

  ```
  stormbootx-manifest 1
  version 0.13.0
  commit 1f27df3
  golden golden-stormbootx-rustnic-0123456789abcdef
  serial 42
  canary 52:54:00:12:34:56          (optional, repeatable: only these MACs)
  file <sha256> <size> EFI/BOOT/BOOTX64.EFI
  file <sha256> <size> stormboot/stormboot.conf
  file <sha256> <size> stormboot/drivers/stormnic-ixgbe.efi
  file <sha256> <size> startup.nsh
  ```

  Paths are only `startup.nsh`, `EFI/BOOT/*` and `stormboot/*` (never
  `stormboot/state` or `stormboot/local.conf`), and `BOOTX64.EFI` must be
  there. A media golden's `media.files` is exactly its `file` lines.
- **Authentic.** The signature is checked against the Ed25519 public keys
  compiled in (`manifest::RELEASE_KEYS`, current and next) before a byte of
  the manifest is believed, then each fetched file's size and SHA-256. Plain
  HTTP is enough: whoever answers can only serve what was signed. The
  private key never leaves the stormcentral VM. Since v0.14.0 (#86) that is
  stormcentral's key, `4fe28c02…0a171bce`, the one it serves open at
  `http://stormcentral.g8.lo/api/v1/stormbootx/keys`; a host test holds the
  bytes to that hex. A new key goes in as the next one a release before
  stormcentral signs with it. A test build may add one key at
  build time (`STORMBOOTX_UPDATE_TEST_KEY`, hex); no golden build sets it,
  and a binary that has one says so on every check.
- **Never backwards.** The serial must be above every serial the medium has
  carried, at or above `StormBootMinSerial` (the machine's highest passed
  trial), and above any serial that failed its trial on this medium. A
  rollback is a new, higher serial.
- **A/B.** Only the files that differ are fetched. Each is written as
  `*.new`; then the current file becomes `*.prev` and the new one takes its
  name, the bootloader last; a loadable driver the release no longer carries
  becomes `*.prev` too. `\stormboot\state` records the trial before the swap,
  and the machine warm-resets into the new set:
  `update : v0.12.0 -> v0.13.0 (golden-…, serial 6), 3 file(s), 280 KB; restarting into it`.
- **The trial.** The new binary counts its starts
  (`update : serial 6 on trial, start 1 of 2; it must reach an attach`) and
  passes once it has attached an image, or the engine answered a `local`
  intent (`update : serial 6 passed its trial (attached); it stays`). After
  two starts without that, the third puts every changed file back from
  `*.prev`, marks the serial failed on this medium, and chain-loads the
  restored binary. A boot that fails for a reason that is not the release
  (the engine down twice) also puts it back; the cost is a stick on the
  previous release until the next serial.
- **Linux is told**: `StormBootUpdate` (volatile, #76's vendor GUID) is
  `serial:<n>`, `trial:<n>:<start>`, `good:<n>` or `failed:<n>`.
- **`local.conf`** is the medium's own. It is read before `stormboot.conf`, so
  its keys win, and an update never writes it, except to keep a `name =` or
  `tag =` that the release's `stormboot.conf` would drop.
- **Room.** The new files are written beside the old, so the medium needs
  their size free (the `*.prev` copies about to be replaced count); a
  medium without room says so and is left alone. The 4 MiB ESP of
  `build-boot-agent.sh` holds a set of binary and drivers three times over.

`tests/update-ovmf.sh` boots a writable GPT medium on a USB stick
(`qemu-xhci` + `usb-storage`) under OVMF ten times against a stub
stormcentral and a test key: stormcentral's own first promotion
(`tests/fixtures/`, hex signature) verifies against the release key; a
manifest signed by another key is refused; serial 5 is written, restarts,
passes its trial on the attach; serial 5 again is `current`; serial 6 for
another machine's MAC only is refused, then, naming this machine as a canary
too, taken, retiring the driver it no longer carries to `.prev`; serial 7,
larger than the ESP, is refused for room; serial 8, whose `stormboot.conf`
names a dead NVMe port, fails both trial starts, and the third start puts
serial 6 back, which then refuses 8. The disk is checked with mtools after
each boot.

### Why chain-load

Publishing a disk and returning to the boot manager does not boot it. A disk
that appears while a boot option is running is not in `BootOrder`, so the
manager moves on and drops to setup. Real EDK2's `PartitionDxe` also skips a
handle that has BlockIO but no device path. That is why step 7 installs one.
OVMF is lenient here and hid it. The ESP match is strict because an earlier,
looser version booted a stale Windows install off a local SAS disk.

## The network path

**NIC drivers from the media (#26, `src/drivers.rs`).** Before the network
is brought up, every `*.efi` in `\stormboot\drivers\` on the boot volume is loaded
(`LoadImage` by device path) and started, then every handle is connected. One
`ConnectController` pass runs first, so the platform's own drivers claim every
NIC they will take and a media driver only gets the ones nothing else wanted.
The console prints `drivers : N of M started from \stormboot\drivers` and one
line per file; a driver that fails is reported and skipped. This is for
firmware that has no UEFI driver for its NICs, like the
Supermicro X9 blades (legacy-only Intel 10G and ConnectX-3). No directory, no
change.


**stormbootx's own TCP/IP (#56, `src/net.rs`).** The firmware's `EFI_TCP4` is
not used on any machine. It is optional, off in setup on some machines, and
missing on server3 (X9) even with a NIC driver loaded. And EDK2's own stack
can't be carried instead, because since edk2-stable202405 its IPv4 drivers
refuse to start without `EFI_RNG`, and TcpDxe without `EFI_HASH2`, which old
firmware lacks. [smoltcp](https://github.com/smoltcp-rs/smoltcp) 0.14 runs on
each NIC's SNP instead:

- **Each SNP is opened `EXCLUSIVE`**, so a firmware MNP bound to it lets go and
  can't take frames meant for smoltcp. A NIC the firmware won't release is
  used shared, and the console says so. On the fall-through every NIC is given
  back (closed and reconnected), so a later boot option finds the firmware's
  own stack again.
- **DHCP** is smoltcp's dhcpv4 socket, on every NIC at once, re-sent every 2 s
  (a 25G link takes seconds to train). It asks for options 1/3/6/12/15, and
  keeps the reply for the name (#23). It asks for option 42 too, for the
  NTP server (#77). ARP and TCP are smoltcp's.
- **UDP** carries one SNTP exchange and, for a server named by name, one A
  query per boot (#77), on the NIC that reached the engine.
- **A connection** goes out on the NIC that reached a target last, else on
  the best-ranked NIC with a lease: link up first, then the larger MTU (the
  storage port is the jumbo one). Each try gets 5 s while another leased NIC
  is left. A reset is an answer (nothing listens there), not a wrong wire.
  The NIC table prints once, at bring-up.
- **Waiting is never silent** (#88). While no NIC holds a lease, a
  connection prints `waiting for a lease (N s): nic 0 link UP, DHCP 3 out 0
  in, 14 frames in; …` once a second: frames in with no DHCP in is a wire
  that works and a server that doesn't answer; no frames in at all, a dark
  wire. Every SNP call is marked, and a 1 s timer event at `TPL_NOTIFY`
  prints `nic N: SNP.Receive (<driver>) has not returned after 5 s` (again
  every 30 s) while one is stuck, since a NIC driver call that never returns
  is outside every deadline and can't be abandoned from one thread. A driver
  that hangs at `TPL_NOTIFY` or with interrupts off stops the timer too; the
  `driver` line before `Start` is then the last word (`src/snpwatch.rs`).
- **Timeouts** are per operation without progress: 30 s for the engine API and
  the attach (`TcpSocket::connect`), 5 s for the PTR query, 8 s for the
  console's `connect`. Nagle and delayed ACKs are off, with a 256 KiB receive
  window (window scaling), so a 128 KiB read is one round trip.
- **Time** is the TSC, calibrated against `Stall` at bring-up.
- **Randomness** (`src/entropy.rs`) seeds smoltcp's TCP ISN and DHCP xid, and
  picks each connection's local port. It uses the first source present: the
  firmware's `EFI_RNG_PROTOCOL` (read, never installed, so the next stage
  never finds a weak RNG left behind), then RDSEED/RDRAND (RNDR on aarch64),
  then cycle-counter jitter across short stalls, hashed with SHA-256 together
  with the firmware time, the MAC and the SMBIOS UUID. It never refuses to
  boot for lack of entropy. `rng = cpu` or `rng = jitter` in `stormboot.conf`
  skips the sources above it.

Under OVMF with no firmware network stack, `tests/net-ovmf.sh` leases from
QEMU's slirp, reads a stub engine, and attaches a stub NVMe/TCP target at
4096-byte blocks. The firmware's FAT reads a 96 MiB `BOOTX64.EFI` through the
published BlockIO (about 120 MiB/s, KVM) and starts it. It runs once as
shipped, and once with the firmware RNG and RDRAND/RDSEED masked, which must
print `rng : jitter`. A stub SNTP server answers both (#77): on the first boot
with a time in 2031, which stormbootx must set and the payload must read back
from the RTC; on the second as an unsynchronised server, which must set
nothing and hand down `StormBootClock = unsynced`.

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

Every error in `run()`, and a `local` intent, ends in `fall_through`:

0. If an image was attached, it takes the disk back first
   (`blockio::withdraw`, #54): disconnect, uninstall BlockIO and the device
   path, drop the NVMe/TCP connection, and print `blockio : withdrawn`. The
   disk's functions live in this image, which the firmware unloads when it
   returns, so a disk left installed is one the boot manager calls into
   freed memory (pvetest1's #UD at RIP 0x47FFFFFCA). If the firmware refuses
   the uninstall, stormbootx says so and resets the machine rather than
   return. A start that finds a disk an earlier start left behind prints
   `blockio : N handle(s) an earlier start published are still installed`.
1. It prints `no network boot: <reason>`.
2. It offers a console: *press c within 5 s*. Silence continues, so an
   unattended machine never stops at a prompt.
3. It sets the clock from NTP if the boot has not yet (step 5a, #77), so the
   local disk's OS gets the right time too.
4. It gives every NIC back to the firmware (`net::release`, #56), so a later
   boot option (PXE, HTTP boot) finds the firmware's own stack bound again.
   It says so: `net : N of M NIC(s) given back to the firmware (exclusive
   SNP closed, reconnected)`, and names any NIC whose SNP would not close
   (#68). `tests/net-ovmf.sh`'s fifth boot falls through with the
   firmware's IPv4 stack on, and the firmware's PXE on the same NIC then
   boots the payload over TFTP.
5. It counts local disks (whole, non-removable BlockIO devices; the
   attached image is no longer one of them).
   - If there are any, it prints `falling through to the local disk (N found)`.
   - If there are none, it says so and waits 30 s.
6. It returns `EFI_ABORTED`, so the boot manager tries the next boot option.

The console (`src/shell.rs`):

| Command | Does |
|---|---|
| `nics` | every NIC with an `EFI_SIMPLE_NETWORK`: MAC, MTU, link, SNP state |
| `state` | the address each NIC leased on stormbootx's own stack |
| `dhcp [secs]` | bring the stack up if it is not, and wait up to `secs` (default 10) for every NIC's lease |
| `connect IP PORT` | open a TCP connection the way the attach does (8 s) |
| `pci [all]` | devices on the bus, with or without a driver |
| `fec [MODE]` | read the ConnectX FEC; with MODE (`default`/`rs`/`fc`/`off`/`autoneg`), write it |
| `reset` | warm reset, so firmware re-reads NV config |
| `boot` | leave the console and continue the fall-through |
| `help` | the list above |

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
| `dns` | none | DNS server for the PTR of the machine's own address, when its DHCP reply names none or cannot be read (#26); `build-boot-agent.sh --dns` |
| `ntp` | none (DHCP option 42, then `pool.ntp.org`) | NTP server for the hardware clock when the lease names none: `host[:port]`, or `off` to leave the clock alone (#77); `build-boot-agent.sh --ntp` |
| `local_when_bootable` | `false` | `true`: `auto` and every doubt boot a local disk whose ESP carries `BOOTX64.EFI` instead of claiming (#3). Off until intents work on forge |
| `esp` | `auto` | who reads the attached ESP (#37): `auto` (firmware, then stormbootx), `firmware`, or `stormbootx` |
| `rng` | `firmware` | the first entropy source tried (#56): `firmware`, `cpu` (skip `EFI_RNG`) or `jitter` (skip both); `build-boot-agent.sh --rng` |
| `media` | none | the media's label, printed under the banner (`media : fw`); written by the golden build, `build-boot-agent.sh --media` |
| `fec` | none | **recovery sticks only**: write this FEC and warm-reset |
| `update` | none (no self-update) | `http://host[:port]/path` of this medium's release on stormcentral, or `off` to pin the medium (#83, *Self-update*); written by the golden build, `build-boot-agent.sh --update` |
| `stamp` | none | parsed, not used |
| `version`, `commit` | none | **not read by stormbootx**: the stormbootx on this medium, written by every `build-boot-agent.sh` run (#94) so a tool reading the FAT (storminstall, which refuses media older than v0.15.0's install-config slot) needs no EFI binary run. The version is Cargo.toml's and must be in the `.efi` laid down, or the build fails; the commit is the binary's `STORMBOOTX_BUILD` stamp, or `unstamped`. A self-update replaces the file with the release's, so they stay true |

`\stormboot\local.conf`, when present, takes the same keys and is read first,
so its values win. It belongs to the medium: a self-update never replaces it.

## `tcp4probe`

A second binary, `src/tcp4probe.rs`. It reports which of the firmware's
network-stack protocols are present layer by layer, runs a `ConnectController`
pass when TCP4 is missing, and then creates and configures a TCP4 child. Since
#56 stormbootx needs only the SNP line of that report. The rest describes the
firmware, not a requirement. `src/tcp4.rs` and `src/dhcp4.rs` (the firmware
TCP4 client stormbootx used until 0.9) are compiled only into it now.
When a loader before it set them, it prints the `StormBootTag`,
`StormBootHostNqn`, `StormBootClock` and `StormBootUpdate` variables and
their attributes (`handed down : …`, #76, #77, #83), reassembles
`StormBootInstallConfig` and checks its length and SHA-256 (`install cfg :
…`, #79), then what the RTC reads (`rtc : …`);
`tests/net-ovmf.sh` starts it as the attached image's `BOOTX64.EFI` and
checks them.
A stick that boots it is made with `--probe` (see *Getting it onto a stick*).

## `espprobe`

A third binary, `src/espprobe.rs` (#37). It connects every controller, then
for each whole disk except the one it booted from whose GPT has an ESP, it
reports whether the firmware's FAT loads `\EFI\BOOT\BOOTX64.EFI`, reads the
file with `esp.rs`, and starts it from the buffer with `espboot.rs`, the same
code stormbootx falls back to. Then it powers off. `tests/esp-ovmf.sh` boots
it under OVMF against a 4096-byte virtio disk carrying a 4096-byte-sector
FAT16 ESP, and passes only if the payload it starts prints.

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
  rustc --edition 2021 --test src/dnsname.rs -o t/dnsname-test && ./t/dnsname-test && \
  rustc --edition 2021 --test src/esp.rs -o t/esp-test && ./t/esp-test && \
  rustc --edition 2021 --test src/sntp.rs -o t/sntp-test && ./t/sntp-test && \
  rustc --edition 2021 --test src/manifest.rs -o t/manifest-test && ./t/manifest-test && \
  rustc --edition 2021 --test src/installconf.rs -o t/installconf-test && ./t/installconf-test && \
  R=${CARGO_TARGET_DIR:-target}/x86_64-unknown-uefi/release && \
  tests/esp-ovmf.sh $R/espprobe.efi $R/tcp4probe.efi && \
  tests/net-ovmf.sh $R/stormbootx.efi $R/tcp4probe.efi && \
  scripts/build-boot-agent.sh --iso --binary $R/stormbootx.efi --media shelltest --output $PWD/t/s.iso && \
  LAYOUT=cd-last tests/shell-ovmf.sh t/s.iso old "media       : shelltest" && \
  LAYOUT=cd-last tests/shell-ovmf.sh t/s.iso ovmf "media       : shelltest" && \
  tests/update-ovmf.sh $R/tcp4probe.efi'
```

That builds `stormbootx.efi`, `tcp4probe.efi` and `espprobe.efi`, and runs the
eight host test suites. Then it boots espprobe under OVMF (`tests/esp-ovmf.sh`),
and stormbootx itself against the stub engine and NVMe/TCP target
(`tests/net-ovmf.sh`). Last, it builds an ISO and boots its `startup.nsh`
from an EFI Shell (`tests/shell-ovmf.sh`, #60), once with the old EDK shell
and once with OVMF's own. `tests/update-ovmf.sh` (#83) runs last, because it
rebuilds `stormbootx.efi` with a test key: ten self-update boots off a
writable USB stick (*Self-update*).
There is no host target and no `cargo test`. `src/sha256.rs`,
`src/intent.rs`, `src/universal.rs`, `src/dnsname.rs`, `src/esp.rs`,
`src/sntp.rs`, `src/manifest.rs` and `src/installconf.rs` are the exceptions: each uses only
`core` and names no `crate::` item, so each compiles as its own crate with
`rustc --test`. `esp.rs`'s tests build their images with `mkfs.fat` and
mtools, which must be on the `PATH`.
`--edition 2021` is required, because bare `rustc` defaults to 2015, where
`core` is not in scope.

`Cargo.lock` is tracked, and `uefi` is pinned at 0.39 / `uefi-raw` at 0.15,
`ed25519-compact` at 2.6.0 (verify only, no default features, #83).
Bump them deliberately, in their own commit.

### Getting it onto a stick

Packaging only; nothing here runs at boot. What ships is a **golden** (see
*How it ships* below); these scripts are what the golden build runs, and are
also usable by hand in a build. `scripts/build-boot-agent.sh` puts the built
`.efi` and a `stormboot.conf` onto boot media: a GPT `.img` to `dd` onto a
USB stick, or with `--iso` an El Torito `.iso` for BMC virtual media. The ISO
is isohybrid the way Debian's netinst is (an MBR `0xef` partition and a GPT
entry over `/esp.img`), and the ESP is at `mkfs.fat`'s own geometry (4 MiB:
FAT12, 2 KiB clusters): AMI Aptio 4 hung at POST A2 on the old pure El Torito
FAT16 ESP (#55); `tests/iso-layout.sh ISO…` checks both.

Every medium carries `\startup.nsh` (`media/startup.nsh`, #60) at the root
of its ESP, and of the ISO9660 tree. A machine with no boot option for the
media (an X9 blade that has never booted the virtual CD in UEFI) drops to the
firmware's EFI Shell, which runs it. It looks through `fs0`..`fs7` for the
volume carrying both `\EFI\BOOT\BOOTX64.EFI` and
`\stormboot\stormboot.conf`, and starts that `BOOTX64.EFI`. The second file
is the mark: a local disk's ESP has a `BOOTX64.EFI` too (stormuefi on an
installed disk, a stale Windows), never a `stormboot.conf`. The script uses
only what the EDK shell 2.31 (EFI 1.10 mode, what AMI Aptio 4 carries) and
Shell 2.x both take. `tests/shell-ovmf.sh ISO old|ovmf 'LINE' …` boots an
ISO this way under OVMF, with a decoy ESP mapped before the CD. `old` is
EdkShellBinPkg's `Shell_Full.efi` from edk2-stable201811, fetched and checked
against its pinned SHA-256.

Output
defaults to `tmp/images` in the checkout, which on dev is the build's own
drive and is deleted with it. Nothing is left on the build box.

```bash
./scripts/build-boot-agent.sh                    # one stick for every machine
./scripts/build-boot-agent.sh --iso              # same, as an ISO
./scripts/build-boot-agent.sh --pin --portal 192.168.31.202 \
    --nqn nqn.2026-09.lo.g16:stormcos --nsid 2   # one fixed namespace, no claim
./scripts/build-boot-agent.sh --probe            # boots tcp4probe instead
./scripts/build-boot-agent.sh --fec default      # FEC recovery stick

# NIC drivers for firmware without them (#26): the Rust stormnic drivers
./scripts/build-nic-drivers.sh tmp/drivers
./scripts/build-boot-agent.sh --iso --drivers tmp/drivers
```

`build-nic-drivers.sh` builds the Rust NIC drivers, each `--locked` from a
pinned commit of its own repo: `stormnic-ixgbe.efi` (Intel 82599/X540/X552,
`STORMNIC_IXGBE_REF`) and `stormnic-mlx4.efi` (ConnectX-3,
`STORMNIC_MLX4_REF`), beside a `STORMNIC-SOURCE.txt` naming each commit and
digest. `STORMNIC_DRIVERS="ixgbe"` builds one. It checks each is a PE
boot-service driver (subsystem 11). There is no iPXE in it, on any medium or
in any golden (owner on #81: "I dont want the ipxe code. Move to ours."; #91,
#52). The iPXE `intelx` driver that first got the X9 blades onto the network
(#26) is in the history.

`--help` lists the rest (`--drivers`, `--api-port`, `--port`, `--size`, `--binary`,
`--output`).

## Ports, health and shipping

stormbootx is a UEFI application, so it has no daemon, listens on no port and
has no health or metrics endpoint. What it reports goes to the console. It
only makes outbound connections:

| To | Default | Set by |
|---|---|---|
| engine API (health, intent read, claim) | `<portal>:9090`, HTTP/1.1 | `api_port` |
| DNS server (PTR of its own address, #23) | option 6 of its DHCP lease, TCP 53 | DHCP, else `dns` |
| DNS server (A record of the NTP server, when it is named, #77) | the same server, UDP 53 | DHCP, else `dns` |
| NTP server (one SNTP exchange, #77) | DHCP option 42, else `pool.ntp.org`, UDP 123 | DHCP, else `ntp` |
| NVMe/TCP portal (attach) | `<portal>:4420`, or what the claim returns | `port`, or the claim reply |
| stormcentral (the self-update's manifest, signature and files, #83) | the `update =` URL, HTTP/1.1; its host looked up over UDP 53 | `update` |

The portal defaults to `192.168.31.202` (forge). Since stormblock v17.0.0 the
engine API requires a token for everything except `POST
…/boothost/<tag>/claim` and `GET /api/v1/health`. stormbootx sends none, because firmware has nowhere to
keep one. The intent read (stormblock#148) is open in the same way; setting an
intent needs the token.

**How it ships: as goldens** (owner, 2026-09-28, #21: everything is a
golden). Nothing is kept on the build box. A machine's virtual CD is to be
served from the golden (minismbd#7, open), not from a copied file.
`deploy/build-golden.sh <golden> OUT` writes one golden's tree into OUT and
does nothing else, for stormcentral to run into the volume it mounts:

| golden | tree |
|---|---|
| `stormbootx` | `bin/stormbootx.efi`, `bin/tcp4probe.efi`, `boot/stormbootx.iso` (BMC virtual media), `boot/stormbootx.img` (USB), `boot/tcp4probe.iso`, `media/`, `media.files`, `SHA256SUMS`, `BUILD` |
| `stormbootx-rustnic` | `bin/stormbootx.efi`, `boot/stormbootx-rustnic.iso` (BMC virtual media), `media/`, `media.files`, `SHA256SUMS`, `BUILD` |
| `stormbootx-disk` | `boot/stormbootx-disk.img` (USB stick, the `stormbootx` medium), `SHA256SUMS`, `BUILD` |
| `stormbootx-rustnic-disk` | `boot/stormbootx-rustnic-disk.img` (USB stick, the `stormbootx-rustnic` medium), `SHA256SUMS`, `BUILD` |
| `nic-drivers` | `bin/stormnic-ixgbe.efi`, `bin/stormnic-mlx4.efi`, `STORMNIC-SOURCE.txt`, `SHA256SUMS`, `BUILD` (an EFI boothelper; no medium takes it as an input since #52, and it holds no iPXE since #91) |

`media/` is the medium's files as a tree (`EFI/BOOT/BOOTX64.EFI`,
`stormboot/stormboot.conf`, `stormboot/drivers/*`, `startup.nsh`), and
`media.files` one `<sha256> <size> <path>` line per file: what stormcentral
signs and serves when it promotes the golden, for media to update themselves
from (#83, stormcentral#279). Each medium's `stormboot.conf` carries
`update = http://stormcentral.g8.lo/api/v1/boothelpers/<golden>`.

**The USB sticks are their own goldens** (#41, owner on #35; #97).
`stormbootx-disk` and `stormbootx-rustnic-disk` each hold one image, a GPT
disk with a 64 MiB FAT16 ESP at 512-byte sectors carrying the same files as
the family's ISO. `dd` it whole onto a stick. Its `update =` names the
family's boothelper (`stormbootx` or `stormbootx-rustnic`), not its own, so a
stick takes the same signed promotion as the ISO. The ESP is 64 MiB rather
than the ISO's 4 because a stick is writable and updates itself, writing the
new set beside the old. The sc-build check boots each image as a USB stick
under OVMF (`tests/media-ovmf.sh IMG …`). Registering them with stormcentral
is stormcentral#190.

**Two media, side by side (#45, #52).** `stormbootx` is the
firmware-drivers medium: no `\stormboot\drivers` at all, so every NIC is
the firmware's own UEFI driver's (the Dell R230), and the console says
`drivers : none on the media; the firmware's own NIC drivers`. Since #52 it
carries no iPXE (owner on #81: "I dont want the ipxe code ... we can keep the
code to use built in nic firmware"); a machine whose firmware has no driver
for its NICs (the X9 blades) boots `stormbootx-rustnic`, the same agent from
the same commit with the Rust drivers: `stormnic-ixgbe.efi` built `--locked` from
`STORMNIC_IXGBE_REF` in `scripts/build-nic-drivers.sh`, and no iPXE NIC
driver, so the Rust driver is what a machine booting it tests. Since #51
(stormnic-ixgbe 0dd4267) it installs `EFI_SIMPLE_NETWORK_PROTOCOL` on a child
handle after its bring-up and DMA check, so the network stack can bind to the
Intel 10G. Since #63 (stormnic-ixgbe 8ea722a) its `Start` enables memory
decode and bus mastering on AMI Aptio 4, which refuses the PCI I/O attribute
calls (stormnic-ixgbe#19). Since #65 (stormnic-ixgbe 563ea8d) a reset whose
EEMNGCTL.CFG_DONE never sets is logged and `Start` continues, as on server3's
82599 (stormnic-ixgbe#21). Since #101 (stormnic-ixgbe 728b328) its console is
one line per bound NIC (MAC, link speed, or `link down after N ms` with
LINKS/AUTOC/AUTOC2/ESDP), plus warnings, and the full bring-up trace only
when the volatile EFI variable `StormnicVerbose` is set (stormnic-ixgbe#22
and #26); a linked boot is about 3 s faster (stormnic-ixgbe#24). Since #34 it
also carries `stormnic-mlx4.efi` for the ConnectX-3, built `--locked` from
`STORMNIC_MLX4_REF`. Since #50 (stormnic-mlx4 v0.2.0) its `Start` brings up the
ConnectX-3, keeps it, and installs `EFI_SIMPLE_NETWORK_PROTOCOL` on a child
handle per Ethernet port (about 1.5 s per NIC plus up to 5 s for link), so
the network stack can bind above it. Since #64 (stormnic-mlx4 v0.2.1) it finds
the UAR's PCI I/O BAR index through `GetBarAttributes`, because AMI Aptio 4
numbers BARs rather than BAR registers (stormnic-mlx4#15). Since #66
(v0.2.3) it prints each port's speed, autonegotiation and module at the link
wait and on every link change, and the first own frame the adapter loops
back (stormnic-mlx4#21). Each Rust driver
is checked to be PE subsystem 11 (EFI boot-service driver) when it is built.
The rustnic media builds its own drivers and takes no nic-drivers golden. Each ISO's `stormboot.conf` names the variant, and the console
prints it under the banner:

```
media       : fw
media       : rustnic ixgbe@728b328 mlx4@0e50017
```

The fw medium's `BUILD` says `drivers = none`, and a `--drivers` given to
`build-golden.sh stormbootx` is ignored. Its `stormboot.conf` names forge
(`192.168.31.202`) and `dns = 192.168.31.252`; which image a machine boots is
its boothost on the engine, so one golden boots every machine. stormbootx is
a stormcentral component of kind `media`, and so is `stormbootx-rustnic`:
`stormcentral component build <name>` builds each into a drive golden whose
bytes are its `boot/<name>.iso`.

## Firmware requirements

- **A UEFI driver for the NIC** (`EFI_SIMPLE_NETWORK`), the firmware's or one
  on the media (#26), and nothing above it. The firmware's
  MNP/IP4/DHCP4/TCP4 are not used (#56). A firmware driver is usually loaded
  only when the NIC is in UEFI/PXE mode in setup (Dell: Integrated NIC
  **Enabled with PXE**; `GlobalSlotDriverDisable` off, or add-in cards have
  no UEFI driver). The Supermicro X9 blades' NICs carry legacy option ROMs
  only, so they boot `stormbootx-rustnic`, which brings its own.
- Any OVMF will do for emulation, Fedora's included (`tests/net-ovmf.sh`).

## In the code, not active

- `registry::claim` / `registry::existing`: the older sbregistry
  `/v1/clones/claim` path at `sbregistry.gt.lo:5100`, behind
  `USE_REGISTRY = false` in `main.rs`.
- `config::render` / `config::write_file` and the `stamp` key: the first
  self-update plan (#2), superseded by #83, which writes through its own
  `selfupdate.rs`.
- The FEC self-heal (automatic write on "all ports down") was switched off in
  0.3.6. It was triggered by a single link sample. The reasoning is in
  `main.rs` step 2b.

## Status

v0.15.1. Running on hardware since 2026-09-05. A Dell PowerEdge R230 (C2NR0Q2)
booted the ISO over iDRAC virtual media, claimed `boothost/C2NR0Q2` and
attached a 32 GiB 4K clone from forge over 25 GbE. The console of that first
attach, verbatim (the build before chain-loading, ea26be1):

```
service tag : C2NR0Q2
tcp4        : available
claim       : boothost/C2NR0Q2 at 192.168.31.202:9090
    interface 0 answered (MTU 1500, link up)
  claimed a clone of this machine's image
  portal    : 192.168.31.202:4420  nsid 7
  namespace : 8388608 blocks x 4096 bytes  (32 GiB)
  transfer  : 128 KiB per command  (controller MDTS 5; path MTU 1500)
blockio     : published on handle 0x8301ae98
RESULT: remote image is a local disk. Firmware can boot it.
```

The same day, with chain-loading (dc44c71), it started stormuefi, which
booted stormcos. Today's build ends a good boot with `RESULT: image attached;
starting its bootloader.`

On 2026-09-28 a Supermicro X9 blade (server1), whose firmware has no UEFI
driver for its NICs, loaded `ipxe-intelx.efi` from the media, got
`tcp4 : available` and reached the engine. Booted again from the golden
(`golden-stormbootx-74a242a6f0e89f75`, a709f9f), it named itself `server1`
from the PTR, claimed `boothost/server1` and attached its clone over NVMe/TCP
(#26, closed). The release disk's `BOOTX64.EFI` then read as `NOT_FOUND`
on that firmware, a 4096-byte-block namespace (#33). Volumes stay 4K (owner,
2026-09-29), so stormbootx now reads the ESP itself when the firmware can't
(#37). On 2026-09-30 server1 booted release 11.56 from a 4096-byte
namespace (v0.7.0 media) once that release's ESP was FAT16 (stormcos#188),
which closed #33. Which reader loaded `BOOTX64.EFI` there (the firmware's
FAT or `esp.rs`) is not yet known (#37). The kernel console on the X9's SOL
(COM2, `ttyS1`) is not stormbootx's: the release names `console=ttyS1,115200`
too (stormcos#220), and stormuefi ≥ 0.9.0 puts the port ACPI SPCR names last
(stormuefi#23). The same
night server3, also an X9, booted on smoltcp (v0.9.0): `tcp4 : smoltcp over
SNP`, `rng : rdrand`, a lease, the claim, the attach, stormuefi and a running
stormcos (#56, #68).

Open issues:

| Issue | What |
|---|---|
| #37 | X9 blades on a 4096-byte namespace: server1 boots 11.56; which reader loaded it is still open; #67 hardens `esp.rs` |
| #36 | the golden media's pinned fallback (nsid 2) |
| #69–#75, #80 | more Rust NIC drivers (iPXE is gone from every medium and golden, #91; the X9 blades boot the rustnic media) |
| #83 | self-update: in the binary, tested under OVMF, stormcentral's key compiled in (#86); no medium updates until stormcentral promotes a golden (`stormcentral stormbootx promote`) |
| #4, #10, #14 | inventory; the shared initiator; test containers |

A slide deck of the above is in [`docs/presentation.md`](docs/presentation.md)
(Marp: `npx @marp-team/marp-cli docs/presentation.md`).

Related: [stormuefi](https://github.com/glennswest/stormuefi) (stage two) and
[stormnetboot](https://github.com/glennswest/stormnetboot) (the earlier
network-boot project, whose PXE chain this USB/NVMe-TCP path retired).
