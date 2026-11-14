# CLAUDE.md — stormbootx

A UEFI application that attaches a remote image over NVMe/TCP, publishes it
as `EFI_BLOCK_IO_PROTOCOL` so the firmware's partition and FAT drivers see its
GPT and ESP, and chain-loads the image's `\EFI\BOOT\BOOTX64.EFI`. ~320 KB,
`no_std`. It carries its own TCP/IP (smoltcp on SNP, #56), so the one thing it
needs from the firmware's network side is a NIC driver's `EFI_SIMPLE_NETWORK`.

**It is stage one of two, not a duplicate of stormuefi.** stormbootx answers
*which image* and attaches it; the `BOOTX64.EFI` it starts on a stormcos image
is **stormuefi**, which selects, verifies and boots a pallet off that disk and
has no network code at all. A node booting from its own drive runs stormuefi
alone. Legacy BIOS is **stormboot4bios** (planned, own repo; both stages in one
loader; it will link `crates/nvme-tcp-initiator`, #10).

Read the cross-project rules in `../CLAUDE.md` first — in particular **build
with `sc-build` after pushing, never on this VM and never as root**, and
scratch files go in `tmp/`.

## Build

Push first, then `sc-build` from this checkout. It builds the pushed commit
on a fresh build VM as `stormbuild`. There is no checkout there, and no `ssh root@`.
The plain `cargo build && cargo test` default does not suit a `no_std` UEFI
crate, so name the command. This builds the three binaries, runs the thirteen
host suites and the initiator crate's tests, boots `espprobe` and stormbootx under OVMF, boots an ISO's
`startup.nsh` from two EFI Shells, and runs the self-update's boots:

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
  rustc --edition 2021 --test src/inventory.rs -o t/inventory-test && ./t/inventory-test && \
  rustc --edition 2021 --test src/connectx.rs -o t/connectx-test && ./t/connectx-test && \
  rustc --edition 2021 --test src/logwire.rs -o t/logwire-test && ./t/logwire-test && \
  rustc --edition 2021 --test src/command.rs -o t/command-test && ./t/command-test && \
  rustc --edition 2021 --test src/distmedia.rs -o t/distmedia-test && ./t/distmedia-test && \
  cargo test -p nvme-tcp-initiator && \
  R=${CARGO_TARGET_DIR:-target}/x86_64-unknown-uefi/release && \
  tests/esp-ovmf.sh $R/espprobe.efi $R/tcp4probe.efi && \
  STORMNIC_DRIVERS="virtio mlx5 e1000e igb" scripts/build-nic-drivers.sh $PWD/t/nd && \
  VIRTIO_EFI=t/nd/stormnic-virtio.efi MLX5_EFI=t/nd/stormnic-mlx5.efi E1000E_EFI=t/nd/stormnic-e1000e.efi IGB_EFI=t/nd/stormnic-igb.efi tests/net-ovmf.sh $R/stormbootx.efi $R/tcp4probe.efi && \
  scripts/build-boot-agent.sh --iso --binary $R/stormbootx.efi --media shelltest --output $PWD/t/s.iso && \
  LAYOUT=cd-last tests/shell-ovmf.sh t/s.iso old "media       : shelltest" && \
  LAYOUT=cd-last tests/shell-ovmf.sh t/s.iso ovmf "media       : shelltest" && \
  LOCAL_EFI=$R/tcp4probe.efi LIMIT=120 tests/shell-ovmf.sh t/s.iso old "from the local disk" "handing over to the local disk" "EFI\BOOT lists" && \
  SECOND_CD=1 SCREEN_PNG=1 LIMIT=180 tests/shell-ovmf.sh t/s.iso old "returning to what started it" && \
  SECOND_CD=1 LIMIT=180 tests/shell-ovmf.sh t/s.iso ovmf "returning to what started it" && \
  tests/screen-ovmf.sh t/s.iso && \
  tests/stick.sh && deploy/build-golden.sh stormbootx-stick $PWD/t/stick && \
  deploy/build-golden.sh stormbootx $PWD/t/fw && scripts/build-stormboot-serve.sh $PWD/t/serve && \
  tests/usb-layout.sh t/fw/boot/stormbootx-usb.img t/fw/media && ! tests/usb-layout.sh t/fw/boot/stormbootx.img && \
  NIC=virtio tests/media-ovmf.sh t/fw/boot/stormbootx-usb.img "media       : fw" "fallback    : none" "fec pin     : rs (\\stormboot\\stormboot.conf): 0 port(s) read" && \
  deploy/build-golden.sh stormbootx-rustnic $PWD/t/rn && tests/usb-layout.sh t/rn/boot/stormbootx-rustnic-usb.img t/rn/media && \
  NIC=virtio tests/media-ovmf.sh t/rn/boot/stormbootx-rustnic-usb.img "media       : rustnic" "; stormnic-virtio.efi drives it" && \
  NIC=e1000e tests/media-ovmf.sh t/rn/boot/stormbootx-rustnic-usb.img "e1000e@d0bc5ec" "stormnic-e1000e 0.1.0: " "nic 0: driver stormnic-e1000e.efi" && \
  NIC=igb tests/media-ovmf.sh t/rn/boot/stormbootx-rustnic-usb.img "igb@80c9ea5" "stormnic-igb 0.1.0: " "nic 0: driver stormnic-igb.efi" && \
  tests/laptop-ovmf.sh t/fw/boot/stormbootx.iso $R/tcp4probe.efi t/serve/stormboot-serve && \
  deploy/build-golden.sh stormbootx-mlx5test $PWD/t/mlx5 && tests/iso-layout.sh t/mlx5/boot/stormbootx-mlx5test.iso && \
  tests/usb-layout.sh t/mlx5/boot/stormbootx-mlx5test-usb.img t/mlx5/media && \
  NIC=virtio tests/media-ovmf.sh t/mlx5/boot/stormbootx-mlx5test.iso "mlx5@66de545 test" "stormnic drivers verbose" "stormnic-mlx5.efi started" "; stormnic-virtio.efi drives it" "prefer    : mlx5: no such NIC on this machine" && \
  REPRO_MEDIA="stormbootx stormbootx-rustnic stormbootx-disk stormbootx-rustnic-disk stormbootx-mlx5test" tests/repro.sh && \
  deploy/build-golden.sh stormbootx-arm64 $PWD/t/arm && \
  EFI_NAME=BOOTAA64.EFI tests/iso-layout.sh t/arm/boot/stormbootx-arm64.iso && \
  EFI_NAME=BOOTAA64.EFI tests/usb-layout.sh t/arm/boot/stormbootx-arm64-usb.img t/arm/media && \
  deploy/build-golden.sh stormbootx-arm64-disk $PWD/t/armdisk && \
  tests/fecpin-ovmf.sh && \
  scripts/build-stormdist.sh $PWD/t/sd && tests/dist-ovmf.sh $R/tcp4probe.efi t/sd/stormdist && \
  tests/update-ovmf.sh $R/tcp4probe.efi'
```

`deploy/build-golden.sh stormbootx-arm64` (#84) builds the agent for
`aarch64-unknown-uefi` (adding the std with rustup until stormcentral#604)
and lays its ISO down with `BOOTAA64.EFI`; `iso-layout.sh` checks it. Nothing
boots it yet: there is no `qemu-system-aarch64` or AAVMF on the template.

`tests/repro.sh` (#53) builds `stormbootx.efi` from two paths with two
`CARGO_HOME`s and requires the same bytes: `build.rs` links with `/Brepro`
and `/DEBUG:NONE` (lld-link's time stamp and PDB GUID), and
`scripts/cargo-repro.sh` remaps the checkout, target dir and `CARGO_HOME`
out of panic locations. Goldens and `build-boot-agent.sh` build through it;
any new build of a shipped `.efi` must too. `REPRO_MEDIA` (#130) builds
those media goldens twice, from two clones, and requires every file the
same: `build-boot-agent.sh` takes its dates from `SOURCE_DATE_EPOCH` (the
commit's time) and its FAT volume ID and GPT GUIDs from the medium's
contents; xorriso writes Rock Ridge with owner 0 (`-r`), `TZ=UTC`; the
stormnic drivers build remapped with `/Brepro`. The second build runs with
umask 077, another zone and (`unshare -r`) another uid. Nothing on a medium
may come from the clock, a random number or the build user.

`tests/update-ovmf.sh` (#83) must stay last: it rebuilds `stormbootx.efi`
with an Ed25519 test key compiled in (`STORMBOOTX_UPDATE_TEST_KEY`) and boots
a writable GPT medium on a USB stick thirteen times through
`tests/update-boots.sh`, sourced by `net-ovmf.sh` for its stubs (#89):
an `update =` naming a host must be looked up at the lease's DNS server
(#148);
stormcentral's real serial-1 manifest (`tests/fixtures/`, v0.14.0) verifies
and is declined as older than the running version, writing only `min = 1`,
then "declined here before" (#96); test releases carry the running version
with a `-test` suffix; bad
signature refused; serial 5 written, restarted, trial passed on the attach;
serial 5 `current`; serial 6 refused as another machine's canary, then taken
as this one's, retiring a driver; serial 7 refused for room; serial 8 (a
dead NVMe port in its conf) fails two trial starts and the third puts serial
6 back; a fresh FAT32 medium (#185) takes serial 5 and passes its trial. mtools
checks the disk after each boot.

`tests/net-ovmf.sh` (#56) boots stormbootx under OVMF with no firmware
network stack, against a stub engine, a stub NVMe/TCP target (4096-byte
blocks, a 96 MiB `BOOTX64.EFI`) and a stub SNTP server (#77: boot 1 must set
the RTC to 2031, boot 2's unsynchronised answer must set nothing). It runs twice: as shipped, and with the
firmware RNG and RDRAND/RDSEED masked (`rng : jitter`). The shipped boot's
ISO names an `update =`, which must be skipped as read-only (#83). With
`VIRTIO_EFI` (stormnic-virtio, built in the job) it boots three more times
(#108): `prefer_media_drivers = virtio` must take a transitional and a
modern-only virtio-net NIC from OVMF's drivers and carry the claim and the
attach over stormnic-virtio, and without the key the firmware keeps it.
Its `httpboot` boot (#172) has no CD: `tests/bootvar.py` writes a
`Uri(http://…/httpboot.iso)` boot option and BootNext into OVMF's variable
store, the firmware's HTTP boot downloads the ISO into a RAM disk, and
stormbootx must say `media : a RAM disk`, decline the self-update for it,
claim, attach and start the payload. `httpimg` (#185) does it with the
FAT32 stick image, as `application/vnd.efi-img`. Never name a stub `http.py` (#173).

A boot that has not started stormbootx (its banner on serial) after
`START_LIMIT` (60) seconds never does (#124): net-ovmf and screen-ovmf print
the firmware's debug port's last 40 lines, what the guest (monitor: status,
RIP) and the host (QEMU's threads and wait channels, load, memory) are
doing, then boot it once more; only a second stall fails. The stall it
caught (2026-10-15, screen-ovmf) was OVMF in DXE looping ATAPI PACKET
commands at q35's emulated ATA CD (`-cdrom`), QEMU at 96% CPU; so the
tests attach the ISO as a `scsi-cd` on virtio-scsi with a bootindex, as
shell-ovmf always has (it never stalled). It was not PEI: the console
showed only its first 90 lines, which the debug port's PEI lines fill. A
failing boot prints the debug port's tail too. Never `-cdrom` in a test.

With `MLX5_EFI` net-ovmf boots three more (#131): QEMU has no ConnectX, so
`mlx5-giveback` loads the real stormnic-mlx5 under the virtio family's file
name, where it must decline the NIC and the firmware must have it back and carry
the boot, the failure the Dell can hit; `mlx5-none` both families on one medium;
`mlx5-absent` the key with no driver.

`tests/shell-ovmf.sh ISO old|ovmf 'LINE' …` (#60) boots an ISO from an
EFI Shell with no boot option for it, and requires its `startup.nsh` to
start stormbootx: `old` is the EDK shell Aptio 4 carries, `ovmf` the build
box's Shell 2.x. OVMF connects only devices in the boot order, so the test's
CD carries a bootindex after the shell's; without it the CD is never mapped.
With `LOCAL_EFI=FILE` (#145) the shell sits on a USB stick and a fixed disk
carries FILE as its `BOOTX64.EFI`: the fall-through must start it (the
command uses tcp4probe, whose `boot fs : \EFI\BOOT lists …` proves it ran).
With `SECOND_CD=1` (#180) a second medium is mapped after the first, and
`startup.nsh` must start only one; `SCREEN_PNG=1` prints the screen as a PNG.

`tests/stick.sh` (#151) builds `stormbootx-stick` for
`x86_64-unknown-linux-musl` with a test key and runs it against signed
releases from a local HTTP stub: a directory as the stick's ESP (update,
trial, failed serial, `update = off`, older, tampered and other-key
manifests, canary, dry run) and a machine of image files for the refusals
(fixed disk, slab, mounted, held). The mount itself needs root: not here.

`tests/laptop-ovmf.sh GOLDEN.ISO PAYLOAD STORMBOOT-SERVE` (#164) boots a
golden's ISO as the laptop app makes it (`\stormboot\local.conf` and
`install-config.yaml` added with mtools) against stormblock's real
`stormboot-serve`, built from the commit pinned in
`scripts/build-stormboot-serve.sh` (stormblock v22.3.0): by name, by MAC,
`--intent local`, a name not served, and `record` (#167: a signed `hold`
and then `poweroff` PUT on the laptop's command record, stormblock#494), and `maint`
(#167: stormboot-serve answers with the node's own image, which must not be
attached). Bump that pin deliberately.

`tests/screen-ovmf.sh ISO` (#144) boots the shell test's ISO with no
network and checks the panel through QEMU monitor screendumps
(`tests/screen.py`): the header bar drawn, the failed step red, F2 to the
details and Esc back, and no panel text in the serial log. `SCREEN_PNG=1`
(it and net-ovmf's `shipped` boot) prints each dump as a base64 PNG line.

`tests/fecpin-ovmf.sh` (#186) builds stormbootx with a pretend card
(`STORMBOOTX_FEC_TEST`, in its own target dir; `build-golden.sh` refuses it) and
boots six times with no `-no-reboot`: a write and one real warm reset, a card that
forgets the value (one reset, never two), `fec_pin = off` on that machine, a
refused write, a card already on RS, and no `fec_pin`; then `fec_table.25g = off`, per-port keys, and an
`api` boot (#203): `GET /api/v1/fec`, a signed `fec all rs`, `fec-reset`, then rs runs. QEMU has no ConnectX, so
mlxfec's own read and write are not in it.

`tests/iso-layout.sh ISO …` checks an ISO is isohybrid with a FAT12 ESP,
the layout Aptio 4 boots (#55). `tests/usb-layout.sh IMG [TREE]` (#185) checks
a golden's `-usb.img` is a GPT with one real FAT32 ESP, the layout the R6415
boots, with its `media/` files byte for byte. `tests/media-ovmf.sh ISO 'LINE' …` boots a media ISO under OVMF and requires
console lines (#45: `media : …`); build the goldens under `t/` in the same
job to use it. `NIC=virtio` gives it a virtio-net NIC (#108).

The last line boots `espprobe` under the build VM's OVMF (KVM there) against a
4096-byte disk (#37). `R=` is the one place the target dir is named, and it
reads `CARGO_TARGET_DIR` rather than assuming `target/`.

Don't `ls target/...` afterwards. the build VM sets its own `CARGO_TARGET_DIR`, so the
`ls` fails, and sc-build files that as a `build-failure` issue (#16 was one).

There is no `cargo test`: this is a `no_std` UEFI binary with no host target, so
the compile *is* the check. A macOS build is not possible at all here — the
target is `x86_64-unknown-uefi` and the code is entirely firmware-facing.

`src/sha256.rs` is the one exception, and it is worth keeping. It touches only
`core` and names no `crate::` item, so it compiles standalone as its own crate
and the FIPS vectors can actually be run (the `rustc --test` lines in the
`sc-build` command above).

`--edition 2021` is not optional: bare `rustc` defaults to edition 2015, where
`core` is not in scope and the file will not compile even though it is correct.

Anything that gives that module a dependency on the rest of the crate takes
those vectors out of reach. Don't.

`src/intent.rs` follows the same rule for the same reason: it parses the boot
intent reply and decides, using `core` only, and the HTTP exchange stays in
`registry.rs`. Its tests are the proof that every doubt reads as `auto`. `src/universal.rs`
(#15) is the third: the engine-version gate, the MAC choice, the serial
placeholders (#7, stormipmi's vectors) and the claim
reply's `host` object, `core` only. `tcp4probe.rs` includes it by `#[path]`
because `tcp4.rs` uses it, so a new `crate::` use in `tcp4.rs` must be
carried there too (#24 was that). `src/dnsname.rs` (#23) is the fourth:
DHCP option parsing and the PTR wire format, tested against microdns
replies captured on 2026-09-27. `src/esp.rs` (#37) is the fifth: GPT and
FAT, tested on images `mkfs.fat` and mtools build inside the test, so they
must be on the build box's `PATH` (they are on the build VM). `src/sntp.rs` (#77) is
the sixth: the SNTP packet, the reply checks and the UTC calendar.
`src/manifest.rs` (#83) is the seventh: the self-update's manifest, state
file, start/serial decisions and HTTP framing. `src/installconf.rs` (#79) is
the eighth: `install-config.yaml`'s chunk variables and header; tcp4probe
includes it and `sha256.rs` by `#[path]` to read the hand-down back.
`src/inventory.rs` (#4) is the ninth: the firmware inventory sent with the
claim, its SMBIOS parse (BMC, CPU, memory) and its JSON under the engine's
16 KiB; it alone carries `extern crate alloc` (it builds strings).
`src/connectx.rs` (#25) is the tenth: which Mellanox ids `mlxfec` reads
FEC from, which are ConnectX-3s with none, when "no ConnectX" is true, and
which ids `prefer_media_drivers = mlx5` takes (#131), and where to look (#186: a root
bridge's bus range from its `Configuration()` table, and the walk from it).
`src/command.rs` (#167) is the eleventh: the signed command's text and body,
the verbs, and the API's HTTP request parser; `extern crate alloc` like
`inventory.rs`. The signature check is `api.rs`'s (ed25519-compact).
`src/distmedia.rs` (#198) is the twelfth: a stormdist medium's ISO 9660
directory records, `BOOT.toml`, `RELEASE.toml`'s console fields, the release
key `DIST_KEYS` and the read-rate line; core + alloc. Its signature check is
`localinstall.rs`'s.

`tests/dist-ovmf.sh TCP4PROBE STORMDIST` (#198) makes a signed bootable full
ISO with the real `stormdist make` (`scripts/build-stormdist.sh`, stormdist
v0.5.0 pinned; the build VM can fetch the private repo and mkfs.iso.rs), whose
ESP's `BOOTX64.EFI` is tcp4probe beside `RELEASE.MRK`, and boots a stormbootx
built with `STORMBOOTX_DIST_TEST_KEY` (its own target dir) with no network:
`cd-local` (a CD, `install_source = local`), `usb-forge` (a USB stick, the
network gives nothing, nothing local boots), `installed` (a fixed disk that
boots wins), `tampered` (an ESP byte flipped after signing), `otherkey`,
`off`. The medium boots on its own after stormbootx returns, so a positive
boot needs `starting … from the stormdist medium` before tcp4probe's lines.

`tests/api-boots.sh` (#167), sourced at the end of `net-ovmf.sh`, boots
three times with slirp forwarding the guest's API (9180) to the host: a failed
claim holds (`hold = 30`, but nothing local boots, so until a command);
status, inventory and log read; a non-command, another key, another host
and a replayed id refused; `format APIDISK1` zeroes that disk's first and
last MiB and nothing between; `boot-next FFFF` and an unknown serial
refused; `maint` with no `boothost/maint` holds again; `inventory` again;
`poweroff`. Then the engine's record carries a signed `hold`, reported
done; `boot-next 0000` resets. A third, `apimaint`: the record's signed
`maint`, the stub serving `boothost/maint`: the claim carries
`"image":"maint"` and the clone boots.

`Cargo.lock` is tracked, as it should be for anything that produces a binary.
Without it every build resolved fresh, and this is a firmware binary whose
whole dependency surface is two crates that move: a `cargo build` on 2026-09-02
offered `uefi` 0.40 against the 0.39 the code was written for. Nothing here
should change under a build nobody asked to change it. Bump the pin
deliberately, in its own commit, and rebuild.

`./scripts/build-boot-agent.sh` is packaging only: it puts the built `.efi` and a
`stormboot.conf` onto a GPT `.img` (or, with `--iso`, an `.iso` instead), by
default in `tmp/images` in the checkout. Nothing in the agent creates media.

**What ships is a golden, never a file on a build VM** (owner, 2026-09-28, #21).
`deploy/build-golden.sh stormbootx|stormbootx-rustnic|stormbootx-disk|stormbootx-rustnic-disk|stormbootx-mlx5test|stormbootx-arm64|nic-drivers OUT` writes that golden's tree
into OUT and nothing else (README, *How it ships*). Every build gets a fresh
drive, and anything left on the VM is destroyed, so never write outside the job
(no `/build/images`, no `/build/stormbuild/images`). To look at media in an
`sc-build`, build it under `t/` and inspect it in the same job. The golden
request, once stormbootx is a component, is `stormcentral component build
stormbootx --url http://stormcentral.g8.lo`.

## Version locations

| File | Field |
|---|---|
| `Cargo.toml` | `version` |
| `crates/stormbootx-stick/Cargo.toml` | `version` (the same) |
| `CHANGELOG.md` | latest release heading |
| git tag | `vX.Y.Z` |

## Module map

| File | Job |
|---|---|
| `src/main.rs` | `run()`: the boot, step by step; the hold and the boot again (#167); the fall-through |
| `src/api.rs` | stormbootx's own API (#167): `/api/v1/status`, `/inventory`, `/log`, `/fec` (#203), `POST /api/v1/command`; Ed25519 over `RELEASE_KEYS` and `command_key =`, the `StormBootCommandId` floor (NV); the engine's record (stormblock#494); the commands done at the boot's fixed points; the hold (`hold =`, `hold_retry =`) |
| `src/command.rs` | the signed command's text, body and verbs (`fec`, `fec-reset`, #203), and the API's HTTP parsing (core + alloc, host-tested) |
| `src/smbios.rs` | the serials (Type 1 → 2 → 3, placeholders and shared chassis serials rejected), before any network exists; the MAC as the floor |
| `src/net.rs` | the TCP/IP stack (#56): smoltcp on every NIC's SNP (opened exclusively), DHCP, `TcpSocket`, the TSC clock; its own ARP for the next hops (#168); the API's listening sockets, served from every poll (#167); `release` gives the NICs back on the fall-through |
| `src/profile.rs` | the `timing :` line (#168): a mark at the end of each step, durations on the TSC |
| `src/snpwatch.rs` | which driver is under each SNP (named before the first call), and a `TPL_NOTIFY` timer that names an SNP call that has not returned (#88) |
| `src/entropy.rs` | randomness for the ISN, DHCP xid and ports: firmware `EFI_RNG`, then RDSEED/RDRAND (RNDR on aarch64), then jitter through SHA-256; never fails |
| `src/tcp4.rs` | tcp4probe only since #56: a blocking socket over the firmware's TCP4 |
| `src/drivers.rs` | load NIC drivers from `\stormboot\drivers` on the media (#26), after the platform's own bind; `take_over` gives a `prefer_media_drivers` family (`virtio`, `mlx5`) its NICs (#108, #131); `nic_verbose` sets `StormnicVerbose` before the drivers load (#80) |
| `src/dhcp4.rs` | tcp4probe only since #56: DHCP through the firmware's `EFI_DHCP4` |
| `src/nvme.rs` | the NVMe/TCP initiator's UEFI end: `TcpSocket` as its `Transport`, `boot::stall` (#10) |
| `crates/nvme-tcp-initiator` | the NVMe/TCP initiator itself (#10): `no_std` + `alloc`, no dependencies, generic over a `Transport` and a `Platform`, shared with stormboot4bios; reads up to 8 in flight (#100); host tests against a fake controller (`cargo test -p nvme-tcp-initiator`) |
| `src/handoff.rs` | `StormBootTag`/`StormBootHostNqn`, volatile EFI variables naming the machine to Linux's initramfs (#76, stormblock#249); `StormBootEngine` (#183); `StormBootClock` (#77); `StormBootUpdate` (#83); `StormBootInstallConfig` + chunks (#79) |
| `src/selfupdate.rs` | the boot medium updates itself (#83): trial count and revert at start; fetch, Ed25519 check, `*.new`/`*.prev` swap and restart after `net::up`; `mark_good` after the attach; `StormBootMinSerial` (NV) |
| `src/manifest.rs` | the self-update's signed manifest, `RELEASE_KEYS` (stormcentral's, #86), `\stormboot\state`, trial and serial decisions, HTTP framing, `update =` (core-only, host-tested) |
| `src/installconf.rs` | `install-config.yaml` (#79): the media slot, the chunk variables' names and the `v1:<len>:<N>:<sha256>` header (core-only, host-tested) |
| `src/clock.rs` | the RTC set from NTP before Linux (#77): option 42 / `ntp =` / `pool.ntp.org`, one bounded SNTP exchange, `SetTime` in UTC |
| `src/sntp.rs` | SNTP request and reply checks, NTP era → Unix, the UTC calendar (core-only, host-tested) |
| `src/blockio.rs` | publish the namespace as a block device, then chain-load its `BOOTX64.EFI` (the firmware's FAT, then `esp.rs`) |
| `src/distmedia.rs` | a stormdist release medium (#198): ISO 9660 primary-volume lookup, `BOOT.toml`, `RELEASE.toml`'s version/kind/bytes, `DIST_KEYS` (stormcentral's release key, not the boothelper key), the read-rate line (core + alloc, host-tested) |
| `src/localinstall.rs` | install from a stormdist medium (#198): find and check it before the network (both signatures, `release_sha256`), read `boot/esp.img` timed and check `esp_sha256`, start its `BOOTX64.EFI` from those bytes under the firmware's filesystem on the medium's ESP; `install_source =` |
| `src/intent.rs` | the boot intent (`install`/`local`/`auto`) read before the claim; every doubt is `auto` |
| `src/inventory.rs` | the firmware inventory with the claim (#4, #20, #167 `"v":2`): SMBIOS Type 0/1/2/4/17/38/42, ATA IDENTIFY, NVMe Identify, SCSI INQUIRY and VPD parsers, partition kinds, the JSON and its budget, the console lines (core-only, host-tested) |
| `src/hardware.rs` | what the inventory is collected from: the stack's NICs, their drivers and MTU, PCI class 01 (connected first), BlockIO disks with `EFI_DISK_INFO`, NVMe pass-thru, `EFI_SCSI_IO` VPD and their GPT/MBR and ESP `\EFI`, the SMBIOS table; once, before anything is attached |
| `src/registry.rs` | `find_engine`: the boot port, else `api_port`, for the whole boot (#183); read the intent; claim `boothost/<tag>`, or `boothost/default` by MAC, every claim naming the agent (#90); read the engine's version. The old sbregistry `/v1/clones/claim` path was removed (#17) |
| `src/dnsname.rs` | the machine's DNS name (#23): DHCP options 12/15/6, PTR query and answer over DNS/TCP |
| `src/universal.rs` | universal boot (#15): is the engine new enough, which MAC is the machine's, what host the reply named; which SMBIOS serial is a placeholder (#7, matches stormipmi) |
| `src/esp.rs` | the attached ESP read without the firmware's FAT (#37): GPT, FAT12/16/32 at 512..4096-byte sectors; listings, reads at any position and lookups from a directory for `espfs.rs` (#42); `partitions`, every GPT entry, for the inventory (#167) |
| `src/espboot.rs` | read `BOOTX64.EFI` through `esp.rs` and `LoadImage` it from the buffer, under the ESP's path |
| `src/espfs.rs` | a read-only `EFI_SIMPLE_FILE_SYSTEM` over `esp.rs` on the bridged ESP's partition handle (#42), so the bootloader opens its other files; `withdraw` before the disk goes |
| `src/espprobe.rs` | third binary: who can read a 4K ESP; booted under OVMF by `tests/esp-ovmf.sh` |
| `src/sha256.rs` | the digest, because `EFI_HASH2` is optional |
| `src/config.rs` | the target, read from the media rather than compiled in; `local.conf` read before `stormboot.conf` (#83); `boot_from_ram_disk` (#172: the image's volume has a RAM-disk device-path node, i.e. UEFI HTTP boot of the ISO; `media : a RAM disk (UEFI HTTP boot): the downloaded image, gone at reset`, and `selfupdate` declines it) |
| `src/shell.rs` | timed, never-forced failure console before the fall-through |
| `crates/stormbootx-stick` | the node's stick updater (#151): static musl Linux; `src/manifest.rs` and `src/sha256.rs` by path; find (sysfs, refusals), verify, decide, apply as `selfupdate.rs`; built for x86_64 and, since #172, aarch64 (`bin/stormbootx-stick-arm64`, `BOOTAA64.EFI`, family `stormbootx-arm64`); `tests/stick.sh` |
| `src/ui.rs` | the boot panel (#144): its own `ConOut` in the system table; every line to the serial consoles and F2, the steps drawn on the screen consoles; `end` hands the screen back |
| `src/connectx.rs` | which Mellanox ids have a FEC to read, which are ConnectX-3s with none (#25), the scan's closing line, and `pin_action`, the pin's decision (#186) (core-only, host-tested) |
| `src/mlxfec.rs` | ConnectX FEC NV read and write: the pin, a `fec =` recovery stick, the shell's `fec` |
| `src/fecpin.rs` | the FEC pin at boot (#186, #203): `fec_pin = rs`, a warm reset then a cold one then `stuck` (the boot holds), `StormBootFecPin`; the API's `fec`/`fec-reset` and `/api/v1/fec` JSON; a pretend card under `STORMBOOTX_FEC_TEST` for `tests/fecpin-ovmf.sh` |
| `src/tcp4probe.rs` | second binary: does this machine's firmware carry TCP4? |

## Load-bearing facts

These have each cost a debugging session. Do not "simplify" them away.

- **PSDT = 01b (`FLAGS_SGL`) on every NVMe command.** A zero FLAGS byte says
  "PRPs are used", and there are no PRPs over a fabric. stormblockmk does not
  validate it; the Linux target rejects the command with Invalid Field.
- **`CC.EN` before any admin command.** Fabrics Connect only establishes a
  queue. Identify before the controller is enabled gets Command Sequence Error.
- **`Poll` or nothing completes.** EFI networking is asynchronous; a token that
  is never pumped never retires and the boot hangs with no error.
- **`ConnectController` after installing BlockIO.** Installing the protocol
  alone leaves a block device nothing has looked at — no GPT parsed, no ESP.
- **Chain-load; never leave the boot to the boot manager.** A disk that appears
  mid-boot-option is not in `BootOrder`; the machine drops to setup. Real EDK2
  also skips a bare BlockIO in `PartitionDxe`, so the handle carries a vendor
  device-path node, and the ESP is matched **strictly** by it — a loose match
  once booted a stale Windows install off a local SAS disk.
- **stormbootx's TCP/IP is its own (#56, owner 2026-09-30).** smoltcp on SNP,
  on every machine, and the firmware's TCP4 is never asked for: server3 (X9)
  had a NIC driver loaded and still no `EFI_TCP4`. EDK2's own stack is not
  an alternative: since edk2-stable202405 Ip4/Udp4/Dhcp4/TcpDxe refuse to
  start without `EFI_RNG` (`PseudoRandomU32` in their start), and TcpDxe
  without `EFI_HASH2`. SNP is opened `EXCLUSIVE` so a firmware MNP can't
  steal frames; `net::release` gives the NICs back on the fall-through.
  Never install an `EFI_RNG`: the Linux EFI stub seeds its RNG from it.
  Several facts below are about the firmware stack; they remain true of
  firmware and of `tcp4probe`, and each is kept where it still applies to
  `net.rs` (multiple NICs, the late-driver wait, never trusting one link
  sample).
- **Fedora's OVMF has no upper network stack.** SNP present, MNP/IP4/TCP4
  absent, and `ConnectController` over every handle does not change it. Since
  #56 that makes it exactly the emulator to test with: `tests/net-ovmf.sh`
  runs stormbootx's whole network path on it.
- **A server has more than one network stack, and the first is not yours.**
  Each NIC carries its own `EFI_TCP4` service binding, so `handles.first()` is a
  coin flip between the 1 GbE management port and the 25 GbE storage port. The
  wrong one gives `tcp4 : available` and then `NO_MAPPING` forever. Every
  interface is tried, ranked by link then descending MTU — this is the storage
  path, so the jumbo port goes first.
- **Never assume the platform ran DHCP.** `use_default_address` needs an address
  the platform already holds, and nothing guarantees one: the policy may be
  `STATIC`, or DHCP may only run as part of a PXE attempt nobody asked for.
  `dhcp4.rs` leases one directly and states it in `Tcp4ConfigData`. Fallback
  only — `EFI_DHCP4` is optional, like every other stack this refuses to need.
- **The first boot option may run before the network stack exists.** On the
  Dell (C2NR0Q2), same firmware and same boot session: the *first* boot option
  reported `EFI_TCP4 is not present` and the *second*, seconds later, found it
  **already bound**. `ConnectController` cannot bind a driver the platform has
  not dispatched, so more passes were never the answer — time was.
  `ensure_available` now retries for up to 5 s and reports the delay. Do not
  "optimise" that wait away: without it, whichever stick happens to be first in
  the boot order is the one that silently decides the machine has no network.
- **A real server may not carry the stack either, and that is a setup switch.**
  2026-09-03, first hardware run: a Dell (service tag C2NR0Q2) booted
  stormbootx, read its own tag out of SMBIOS, and reported `EFI_TCP4 is not
  present` — *after* the full `ConnectController` pass, so the drivers were not
  in the loaded firmware at all rather than merely unbound. On Dell the switch
  is the UEFI network stack: Integrated NIC set to **Enabled with PXE**, or
  **UEFI Network Stack** under Network Settings. Enabling PXE is not because
  anything wants PXE — it is what makes firmware load MNP/IP4/TCP4.
- **The stack can be there with no driver under it** (#26, Supermicro X9,
  2026-09-27). SnpDxe…TcpDxe all loaded, Network stack = Enabled, and the
  only UEFI NIC driver in the firmware (`PRO/1000`) manages no device: the
  Intel 10G and the ConnectX-3 carry legacy option ROMs only. No SNP, so no
  TCP4, and no setup switch fixes it. The media carries the driver
  (`\stormboot\drivers` on the rustnic medium: the Rust stormnic drivers;
  iPXE's was the first, and no medium carries it since #91). The platform's own drivers
  bind first, so a media driver never displaces a native one, with one
  opt-in exception: `prefer_media_drivers = virtio` (rustnic media only, #108)
  takes VMs' virtio-net NICs from OVMF's VirtioNetDxe for stormnic-virtio,
  and gives them back if it does not bind. Judge a disconnect by the
  `BY_DRIVER` opens left on PciIo, never its status (pve's OVMF answers
  `NOT_FOUND` when every driver came off).
- **Never close an event the TCP driver may still signal** (#26, server1,
  2026-09-28). A token that timed out is still queued; closing its event and
  then aborting the connection (`Drop`'s `Configure(NULL)`) signals freed
  pool. EDK2 checks the signature and shrugs; AMI Aptio 4 did not, and the
  next `CreateEvent` failed with `INVALID_PARAMETER`. `Tcp4Socket::retire`
  aborts first. And `CheckEvent` *clears* what it reports, so a token's state
  is `pump`'s record, never a second `CheckEvent`.
- **microdns sends no DHCP option 12** (#26). It registers the reservation's
  name in DNS instead, so on a microdns network the name comes from the PTR,
  and the reservation names the NIC (`server1a`), not the machine (`server1`).
- **Serials repeat within a chassis** (#26). Seven X9 blades report the
  enclosure's serial as Type 1. Placeholders are not the only non-identity.
- **Proxmox's OVMF does, and it is the emulator to use.** Verified 2026-09-03
  on pve.g8.lo (`pve-edk2-firmware`, Nov 2025) with `tcp4probe` on VM 2062:
  every protocol reads *absent* as found and all nine appear after a
  `ConnectController` pass — the `BoundAfterFullPass` case #5 added. The boot
  menu lists `UEFI HTTPv4/v6`, which is why: HTTP boot pulls TCP4 in. So the
  network path *can* be exercised in a VM, on Proxmox rather than on Fedora's
  OVMF.
- **A 4K image's ESP is a 4096-byte-sector FAT, and old firmware misreads
  it** (#37, #33). Linux won't mount a FAT whose sector is smaller than the
  device's block, and volumes stay 4096 (owner: "512 will kill our
  performance"). AMI Aptio 4 mounts such an ESP and then answers `NOT_FOUND`
  for `BOOTX64.EFI`. Fedora's OVMF (EDK2) loads it fine (`tests/esp-ovmf.sh`). When the firmware loads
  nothing, `esp.rs` reads the file and `LoadImage` takes the buffer. A 512e
  view would not have fixed Aptio: the boot sector still says 4096.
- **The transfer size comes from MDTS, never from the MTU.** Sizing a command
  to fit one frame inverts: a 9000 path lands on 8 KiB and a 1500 path on
  64 KiB. TCP segments to the MSS and never IP-fragments, so frames are not
  the constraint; round trips are. Since #100 `read` keeps up to 8
  commands in flight within one call, which is what attacks them.
- **There is no DNS *discovery* in this binary.** The portal is a fixed
  appliance address named on the media or compiled in; *which image* is a
  `boothost/<name>` synonym claimed from the engine. Discovery was removed
  rather than switched off — it was a second place for the answer to live
  and a timeout on every boot in a zone nobody published. Don't reintroduce
  it. The one DNS query here since #23 is the machine asking the PTR of **its
  own address** for **its own name**, over TCP to the DHCP-given server,
  and only when DHCP option 12 gave none. It never finds a portal, and a
  network with no names boots exactly as before.
- **A machine is its DNS name, and the name is the first label**
  (#23, stormblock#199: hosts are `server3`, `stormblock1`). It comes from
  the lease of **the interface that reached the engine**, not any NIC's, because
  a management port and a storage port may hold different reservations.
  DHCP and PTR can disagree (g10's `192.168.10.31`: reservation `gb10b`,
  PTR `gb10.g10.lo`); DHCP wins, as the owner ordered. A 404 on the name
  moves on to the MAC. The name claim carries `{mac, serial}` so the engine
  can tie a new name to a host it knows (stormblock#204).
- **A machine nothing has named claims `boothost/default` by its MAC — and
  only from an engine with stormblock#200** (#15, universal boot). There
  the engine keys the claim on the MAC and gives each machine its own
  clone (`mac-<hex>`). On an engine from v17 to 19.3.0 the same claim is
  tag `default`: one boot clone name for every machine, each claim
  releasing the clone the last one is running on. So `universal.rs` gates
  it on the public `/api/v1/health` version being **strictly after 19.3.0**,
  and an unreadable version is "no". Don't loosen that gate to make a test
  pass. Not the SMBIOS serial either: serials are not unique (seven
  MicroCloud nodes share one), and a serial claim pins them all to one host.
- **The MAC is the lowest usable one of every NIC, not `handles.first()`.**
  SNP handles appear in driver-bind order, which `ensure_available`'s
  on-demand binding can change between boots. Same lesson as the multi-NIC
  fact below, in the identity instead of the socket.
- **A boot path must never need the network in order to boot without it.**
  Every failure in discovery or attach falls through to the local disk. One
  provisioning outage must not become a fleet outage.
- **smoltcp asks ARP for one address a second per interface** (#168).
  `NeighborCache::limit_rate` is global, and a socket whose neighbour is
  missing is silenced for a second. So a first contact with a new on-link
  host within a second of another (the DNS server after the engine, the NTP
  server after that) waited a whole second, 1 s each on every machine.
  `Nic::prime` sends our own ARP requests for the router, the lease's DNS
  and NTP servers, and each connection's next hop. smoltcp fills its cache
  from any reply aimed at us. Don't take it out to "let smoltcp do ARP".
- **A Dell 14G refuses the hybrid ISO off a stick and over HTTP boot** (#185,
  R6415, BIOS 1.12.2, 2026-11-04): `Boot Failed`, nothing of ours on the
  console; a hand-built GPT + FAT32 stick with the same binary started at
  once. So `boot/<golden>-usb.img`. The ISO stays hybrid with a FAT12 ESP for
  Aptio 4 (#55), and the `-disk` goldens stay FAT16 for the blades: a change to
  any of them answers to a firmware nobody here can emulate. **stormcentral's
  media build keeps only `boot/<comp>.iso` and `media/`:** a new file in the
  tree does not reach a drive golden unless stormcentral takes it.
- **Link-down early in UEFI is not evidence of anything.** A 25G RS link needs
  seconds to come up after a reset, and `snp.media_present` sampled once at
  that moment reads a perfectly healthy card as dark. This is the same fact as
  the `ensure_available` retry above, and forgetting it in a second place cost
  the R230 its 25G ports on 2026-09-07: the FEC self-heal believed a single
  all-down sample, wrote NV config to the card and warm-reset. **Never let a
  one-shot link read authorise a write.** If a decision depends on link state,
  wait for it the way `ensure_available` waits for the stack.
- **The 25G links dropping is usually the switch, not this binary.** dsw1
  latches an SFP28 port every time the peer powers off: light present, no PCS
  lock, immune to `shutdown`/`no shutdown` and to every host-side reset. Only a
  FEC *transition* clears it — both ports came up on the final stage of the
  walk, on CL108-RS, the value they had held for twelve hours. It is automated
  in the **`dswfecfix`** repo, running on the switch under cron; do not
  re-implement the recovery here, and do not read a dark 25G link on this
  fabric as evidence about stormbootx until that log has been checked.
- **The boot writes the NIC's persistent config only under `fec_pin = rs`, the
  owner's decision (#186, 2026-11-04, reversing 2026-09-07's rule), and only on
  the card's own NV value.** The argument it overrode still stands: a wrong
  fabric is fixed on the fabric, and a boot-time NV write is invisible,
  per-machine, survives reinstall, and can only be undone from the thing it just
  broke the network on. So the pin never reads a link (`connectx::pin_action`
  takes no link state), resets at most twice (#203: warm, then cold, then
  a hold; `StormBootFecPin` names the stage and is set before the write and
  each reset), and says what it wrote on the console and on the next boot.
  A port is right when it **runs** the wanted FEC, not when it is up (owner,
  2026-11-13). Don't add a link input or drop the record.

- **The self-update believes the signature and nothing else** (#83). The
  manifest is fetched over plain HTTP from a name DHCP's DNS resolves, so
  everything about where it came from is forgeable; the Ed25519 check against
  the compiled-in `RELEASE_KEYS` comes before the manifest is parsed, and
  each file's size and SHA-256 before it is written. Never add a path that
  writes the medium on a hash alone, and never compile a test key into a
  golden: `STORMBOOTX_UPDATE_TEST_KEY` is for `tests/update-ovmf.sh` only.
- **A serial never goes down on a medium, and a failed one is never retried
  there.** A stick that falls back after a trial would otherwise take the same
  release again on its next boot and loop. The fix for a bad release is a
  new serial. **Nor does the version** (#96): a serial newer than the
  medium's can still carry an older release than the running binary (a
  stick written from today's build, an old promotion), and the serial alone
  once rewrote a v0.15.0 stick to v0.14.0. A deliberate rollback says
  `downgrade true`.
- **The trial is counted at start, before anything that could hang.** A new
  set that hangs in the NIC driver or the network never reaches the code that
  would notice; counting first means the third power cycle puts the old set
  back. The swap renames the bootloader last, and the state is written
  before the swap, so an interrupted swap is put back by the same count.

## Asking the machine before booting it

On a Dell with iDRAC, the hardware questions can be answered without a boot,
which is worth remembering after a day spent inferring them from console output
that turned out to come from three different stale sticks.

```bash
# Every component the Lifecycle Controller knows about.
curl -sk -u root:<pw> -X POST -H 'Content-Type: application/json' \
  https://<idrac>/redfish/v1/Managers/iDRAC.Embedded.1/Actions/Oem/EID_674_Manager.ExportSystemConfiguration \
  -d '{"ExportFormat":"XML","ShareParameters":{"Target":"ALL"}}'
# -> 202 + Location: a task; poll it, then: grep -oE 'FQDD="[^"]+"'
```

**Check `CollectSystemInventoryOnRestart` in that export before trusting it.**
Disabled means the inventory is stale and proves nothing.

That is how C2NR0Q2's real problem was found on 2026-09-04: the export listed
`NIC.Embedded.1-1-1`, `NIC.Embedded.2-1-1` and nothing else — no `NIC.Slot.*`,
no `RAID.Slot.*` — with CSIOR *enabled*, `Slot1`/`Slot2` enabled and
`BootMode=Uefi`. Both add-in cards were absent from the bus, which on an R230
means the riser they share. Every network symptom above it — no DHCP answer, no
jumbo interface, `NO_MAPPING` on both NICs — was the machine honestly reporting
that the port with the cable in it did not exist.

iDRAC8's Redfish (firmware 2.50) is v1.0.2 and has **no** `PCIeDevices`,
`Storage`, or manager `Attributes` endpoints. `EthernetInterfaces`,
`Storage/Controllers`, `Logs/Sel` and the SCP export are what it does have.

## Work plan

The work plan is docs/work-plan.md (not loaded by default: grep -n "^### " docs/work-plan.md, then read only your section).

## Status

v0.41.0 (#198: a stormdist release medium installs offline, `install_source =`; the rustnic media carry stormnic-igb 80c9ea5, #69, and stormnic-mlx4 f3d3bc2, #195). v0.40.0 (#70: the rustnic media carry stormnic-e1000e d0bc5ec; `prefer_media_drivers = e1000e`). v0.39.2 (#192: the rustnic media carry stormnic-ixgbe 4b4813d, jumbo frames). v0.39.1 (#189: a clean `local` with a disk that boots does not wait at `press c`). v0.39.0 (#186: `fec_pin = rs` on every golden, the FEC pin at boot). v0.38.1 (#186: the ConnectX scan starts at each root bridge's first bus). v0.38.0 (#185: `boot/<golden>-usb.img`, the GPT + FAT32 stick a Dell 14G boots). v0.37.0 (#131: `prefer_media_drivers = mlx5`, the `stormbootx-mlx5test` medium). v0.36.0 (#183: the engine's boot port, falling back to `api_port`; `StormBootEngine`). v0.35.1 (#180: `startup.nsh` starts one medium only; the last screen line names the build). v0.35.0 (#172: UEFI HTTP boot of the ISO, the arm64 USB stick and stick updater). v0.34.0 (#167: `maint` claims and boots the maintenance image, stormblock#497). v0.33.1 (#167: the intent's host is a name; laptop-ovmf boots the real command record). v0.33.0 (#167 phase 1: stormbootx's own API on port 9180, Ed25519-signed commands, the hold on a failed boot; goldens `hold = 120`). v0.32.0 (#167's inventory v2, #168's ARP priming and timing line, #99's parked cap9 in 50 ms). v0.29.0 (#151: stormbootx-stick; released, frozen until 2026-10-17, then the master promotes it). v0.28.2 was #130 (reproducible media); v0.28.0 was #144 (the boot panel); v0.27.0 was #148 (the self-update resolves its host); v0.26.0 was #145 (the fall-through starts the local disk itself); v0.25.0 was #128: #4, #10, #17, #25, #36, #42, #53, #67 and #138. **First complete NVMe/TCP attach on real hardware: 2026-09-05**
(and the same day, the full chain: chain-load into stormuefi and a running
stormcos kernel), on a
Dell PowerEdge R230 (service tag C2NR0Q2) booting the agent over iDRAC virtual
media, attaching a 32 GiB clone from forge over a 25 GbE Mellanox port:

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

Every fix in v0.3.0 confirmed on metal in that one boot: the 4096-byte LBA read
from FLBAS (not assumed 512), the MDTS-derived 128 KiB transfer (not the
inverted 8 KiB), multi-NIC selection taking the live 25G port over a
`handles.first()` guess, and the service-tag claim end to end.

### The bring-up, and what each wall was

Three days, and every failure was real hardware or infrastructure, not the
binary — but each one first looked like a stormbootx bug. One of the fixes
was itself wrong: item 4's `fec off` on the switch got the link up that day
and broke the fabric later (see the `#7` correction). The rest held.

1. `EFI_TCP4 is not present` — the UEFI network stack was disabled in setup.
   Enabling it fixed the onboard ports; the console `tcp4` line and #5's
   ConnectController-with-wait were what made it legible.
2. `NO_MAPPING` after 20 s — assumed missing DHCP, was really `handles.first()`
   landing on a NIC with no cable. Fixed by trying every interface, fastest by
   MTU first.
3. Only 2 of 4 NICs present — `GlobalSlotDriverDisable = Enabled` in BIOS
   disabled every slot's option ROM, so both add-in cards were invisible to
   firmware. Found via the iDRAC SCP export, not a boot.
4. All 4 NICs present but 25G ports `link down` — a FEC mismatch, and the
   conclusion drawn here was **wrong and is the breaking change**. The A/B at
   the time (`fec off` linked, `cl108` stayed dark) was read as "the Mellanox
   wants no FEC", and `fec off` was set on all 48 SFP28 ports of dsw1. That
   is what later left ports line-protocol-down for days with light present and
   produced other faults on the links that did come up: no FEC on 25GBASE-SR is
   out of spec, and the ConnectX-4 Lx device-default negotiates cl108-rs.
   Corrected 2026-09-06 (#7): the switch ports run `fec CL108-RS` — the
   standalone uppercase interface command; lowercase and `no fec off` are
   rejected — and the card is left alone. **Never set `fec off` on the 25G
   ports again.** See the `#7` entry in the work plan.
5. Link up but no lease — a red herring; the g16 DHCP server was on the L2 and
   answered directly once a port carried traffic.

### Known follow-ups

- The Mellanox presents MTU 1500 to firmware (its own UEFI driver's SNP,
  which nothing can raise), so the path is not jumbo end-to-end even though
  the switch ports are 9216. Since #165 stormbootx takes DHCP option 26 up
  to what the SNP carries; 9000 on the Dell needs a driver of ours under it. Transfer size is unaffected
  (MDTS drives it), but raising the card's UEFI MTU would let a 9000 path show.
- Three `boothost-C2NR0Q2` clones accumulated from repeated claims during
  bring-up; harmless, but the claim mints a fresh clone each call.
