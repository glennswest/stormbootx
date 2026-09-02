# Changelog

## [Unreleased]

### 2026-09-02
- **docs:** project `CLAUDE.md` (build, module map, load-bearing facts, work plan)
  and this changelog.

## [v0.1.0] — 2026-09-02

### Added
- `stormbootx`, a UEFI NVMe/TCP boot extension: read the service tag out of
  SMBIOS, attach a remote image over `nvme-tcp://`, publish it as
  `EFI_BLOCK_IO_PROTOCOL` and let the firmware boot it.
- `smbios.rs` — SMBIOS type 1 serial number, read with no network.
- `tcp4.rs` — a blocking socket over `EFI_TCP4_PROTOCOL`.
- `nvme.rs` — NVMe/TCP initiator: ICReq/ICResp, Fabrics Connect, admin and I/O
  queues, R2T/H2CData writes.
- `blockio.rs` — install `EFI_BLOCK_IO_PROTOCOL` and `ConnectController`.
- `registry.rs` — claim an image from sbregistry over plain HTTP on TCP4.
- `config.rs` — read the target from `\stormboot\stormboot.conf` on the volume
  found via `EFI_LOADED_IMAGE_PROTOCOL`.
- `scripts/build-boot-agent.sh` — build the GPT/ESP boot image.
