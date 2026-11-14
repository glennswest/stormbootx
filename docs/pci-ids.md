# PCI IDs: which NIC a stormnic driver takes (#202)

stormbootx chooses a NIC's driver by PCI ID and knows nothing else about the
card (the owner, 2026-11-14: "Why … would stormbootx know anything about the
hardware other than what driver to use"; "we only use our drivers going
forward"). `src/drivers.rs`'s `FAMILIES` holds each stormnic driver's list,
and `take_over` gives every NIC on one of the lists to its driver. The lists
are checked against each driver's own at its pin by
`scripts/build-nic-drivers.sh` (`same_ids`), and the build fails when they
differ.

## Match vendor:device, never the subsystem

The owner asked (2026-11-14) about OEM relabels: "dell does a relable,
supermicro, hp also"; "there could be 10-100's of pci ids that are mlx5". This
note checked how they show up, against pciutils' `pci.ids` (2026.11.14) and
Linux master's driver tables (fetched 2026-11-14).

- **A relabelled card keeps the chip's vendor:device.** The OEM's mark is the
  *subsystem* id (subvendor 1028 Dell, 103c/1590 HP/HPE, 15d9 Supermicro,
  17aa Lenovo, 1bd4 IEIT, 193d H3C, and so on). `pci.ids` lists 19 subsystems
  for `15b3:1015` (ConnectX-4 Lx), 38 for `8086:10fb` (82599 SFP+) and 66 for
  `8086:1521` (I350). The list is not complete: C2NR0Q2's Dell card is not in
  it, and Linux still binds it as `15b3:1015`.
- **Linux matches vendor:device with any subsystem** (`PCI_VDEVICE`,
  `PCI_ANY_ID`). That is every entry in `ixgbe_pci_tbl`, `igb_pci_tbl`,
  `e1000_pci_tbl`, `mlx4_pci_table` and `mlx5_core_pci_table`. A relabel
  needs no entry of its own. So the "hundreds" are subsystems, and the device
  ids are few: mlx5 has 14 physical functions in Linux.
- **So stormbootx and every stormnic driver match vendor:device only.** A
  subsystem match would miss every relabel nobody listed.

## Our lists against Linux's (physical functions; VFs left out)

| driver (pin) | ours | Linux | Linux has, we don't | we have, Linux doesn't |
|---|---|---|---|---|
| stormnic-mlx5 (66de545) | 7 | 14 | 1021 CX-7, 1023 CX-8, 1025 CX-9, 1027 CX-10, 2101 CX-10 NVLink-C2C, a2d2/a2d6/a2dc/a2df BlueField 1–4 (and 1011 Connect-IB, IB only) | — |
| stormnic-mlx4 (f3d3bc2) | 2 | 13 + ConnectX/-2 | 1005, 1006, 1008, 1009, 100a–100f, 1010 (MT275xx family members) | — |
| stormnic-ixgbe (4b4813d) | 37 | 52 | 82598: 10b6, 10c6, 10c7, 10c8, 10db, 10dd, 10e1, 10ec, 10f1, 10f4, 1508, 150b; E610: 57ae–57b2 | 155c X540 bypass, 155d 82599 bypass |
| stormnic-igb (80c9ea5) | 36 | 35 | DH89xxCC: 0438, 043a, 043c, 0440 | 10e2, 1509, 1534, 1535, 15f6 |
| stormnic-e1000e (d0bc5ec) | 67 | 113 | 46 older parts: 8257x (105e–10a5, 10b9–10bf, 10d5, 10d9, 10da, 10ea…), ICH8–10 LAN (1049–104d, 10bd, 10c0–10cd, 10de, 10df, 10e5, 10ef, 10f0, 10f5, 1501, 150c, 1525, 294c) | 57b5, 57b6 |
| stormnic-virtio (1a04808) | 1041, 1000 | 1041, 1000 (virtio_net) | — | — |

Each gap is the driver's to decide: some of those parts need other code (the
82598 is not an 82599, and the older e1000e MACs differ). Each is filed on
its driver's repo. When a driver widens its list, its pin bump here carries
the same ids into `FAMILIES`, or the build fails.

How it was checked: the five Linux tables were parsed for `PCI_VDEVICE(…)`,
with the `*_DEV_ID_*` macros resolved from each driver's header. Each pinned
driver's list came from `git show <pin>:src/ids.rs` (or `src/main.rs`'s
`DEVICES`).
