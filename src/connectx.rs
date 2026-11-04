//! Which Mellanox PCI functions `mlxfec.rs` reads FEC from, and what the
//! console says about the others (#25).
//!
//! A ConnectX-4 or later physical function has the 25G-and-up NV FEC
//! configuration `mlxfec` reads (and for `fec_pin` or a recovery stick writes). A
//! ConnectX-3 or ConnectX-3 Pro (the Supermicro X9 blades' 10/40G card) has
//! no RS-FEC at all: it is a ConnectX, and it has nothing to report. That is
//! a different line from a Mellanox function this code doesn't know (a
//! virtual function, a newer part), and neither is "no ConnectX on the bus",
//! which before #25 the console printed beside a ConnectX-3 it had just
//! listed.
//!
//! Also where to look (#186): a machine can have several PCI root bridges, each
//! with its own bus range. The R6415's EPYC has four, at buses 00, 40, 80 and c0,
//! and its card is at 84:00.x. A walk that starts at bus 0 on each bridge finds
//! nothing past the first, because firmware refuses a config read outside a
//! bridge's range. `bus_range` and `walk` are that logic.
//!
//! `core` only and no `crate::` item, so the tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/connectx.rs -o t/connectx-test && ./t/connectx-test
//! ```

/// Physical-function ids whose FEC this code reads (mtcr_ul_com.c:3263).
/// Virtual functions are deliberately absent: they have no NV configuration.
pub const WITH_FEC: &[(u16, &str)] = &[
    (0x1013, "ConnectX-4"),
    (0x1015, "ConnectX-4 Lx"),
    (0x1017, "ConnectX-5"),
    (0x1019, "ConnectX-5 Ex"),
    (0x101b, "ConnectX-6"),
    (0x101d, "ConnectX-6 Dx"),
    (0x101f, "ConnectX-6 Lx"),
    (0x1021, "ConnectX-7"),
];

/// ConnectX parts with no FEC setting: 10/40G, no RS-FEC (#25).
pub const WITHOUT_FEC: &[(u16, &str)] = &[(0x1003, "ConnectX-3"), (0x1007, "ConnectX-3 Pro")];

/// The physical functions stormnic-mlx5 claims (its `DEVICES`, at the pin in
/// `scripts/build-nic-drivers.sh`), which `prefer_media_drivers = mlx5` takes
/// from the firmware's driver (#131). ConnectX-7 (1021) has a FEC to read and is
/// not here: the driver does not claim it, and a take it would decline only
/// disturbs a working NIC.
pub const MLX5_TAKE_OVER: &[(u16, u16)] = &[
    (0x15b3, 0x1013),
    (0x15b3, 0x1015),
    (0x15b3, 0x1017),
    (0x15b3, 0x1019),
    (0x15b3, 0x101b),
    (0x15b3, 0x101d),
    (0x15b3, 0x101f),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A part whose FEC `mlxfec` reads.
    Fec(&'static str),
    /// A ConnectX with no FEC to report.
    NoFec(&'static str),
    /// A Mellanox function this code doesn't know.
    Unknown,
}

/// What a Mellanox (vendor 15b3) function with this device id is.
pub fn kind(device: u16) -> Kind {
    if let Some((_, m)) = WITH_FEC.iter().find(|(id, _)| *id == device) {
        return Kind::Fec(m);
    }
    if let Some((_, m)) = WITHOUT_FEC.iter().find(|(id, _)| *id == device) {
        return Kind::NoFec(m);
    }
    Kind::Unknown
}

/// The line after the scan, given how many Mellanox functions it saw: none
/// is the only time there is no ConnectX on the bus. Each function seen has
/// its own line already.
pub fn closing_line(mellanox_seen: usize) -> Option<&'static str> {
    (mellanox_seen == 0).then_some("no ConnectX on the bus")
}

/// What the FEC pin does this boot (#186).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// Nothing to write, and no record to settle.
    Nothing,
    /// The last boot's write is what the card now says: forget the record.
    Held,
    /// A port differs and no pin has been tried: write, then one warm reset.
    Write,
    /// A port differs after a pin was tried: say so, and never write or reset.
    Stuck,
}

/// The pin's whole decision. Its inputs are what the card's NV said (`ports`
/// read, `differ` of them with another next-boot value than RS, `failed` reads)
/// and whether a pin was already tried. **Link state is not an input, by the
/// owner's terms and on purpose**: one early link sample is what cost the R230
/// its 25G ports on 2026-09-07. A record is forgotten only on a whole read saying
/// RS, so a card that went missing or a failed read cannot re-arm the write.
pub fn pin_action(ports: usize, differ: usize, failed: usize, tried: bool) -> Pin {
    match (differ > 0, tried) {
        (true, false) => Pin::Write,
        (true, true) => Pin::Stuck,
        (false, true) if ports > 0 && failed == 0 => Pin::Held,
        (false, _) => Pin::Nothing,
    }
}

const TAG_QWORD: u8 = 0x8a;
const TAG_END: u8 = 0x79;
const QWORD_LEN: usize = 3 + 0x2b;
const RESOURCE_BUS: u8 = 2;

/// The first and last bus in a root bridge's `Configuration()` resources: ACPI
/// QWORD descriptors, then an end tag. `read(n)` is the table's byte `n`.
/// `None` for anything else, which leaves the caller on bus 0, never a panic:
/// the uefi crate's own parser panics on a tag it doesn't know, and this runs on
/// every machine's boot path. EDK2 can leave the maximum 0 and state a length, so
/// the length is asked first.
pub fn bus_range(read: impl Fn(usize) -> u8) -> Option<(u8, u8)> {
    let le64 = |at: usize| (0..8).fold(0u64, |v, i| v | ((read(at + i) as u64) << (8 * i)));
    for n in 0..64 {
        let off = n * QWORD_LEN;
        match read(off) {
            TAG_QWORD if read(off + 3) == RESOURCE_BUS => {
                let (min, max, len) = (le64(off + 0x0e), le64(off + 0x16), le64(off + 0x26));
                if min > 0xff {
                    return None;
                }
                let last = if len > 0 {
                    min.saturating_add(len - 1)
                } else if max >= min {
                    max
                } else {
                    0xff
                };
                return Some((min as u8, last.min(0xff) as u8));
            }
            TAG_QWORD => {}
            TAG_END => return None,
            _ => return None,
        }
    }
    None
}

/// Walk one root bridge from `start`, following bridges and renumbering nothing.
/// `read(bus, dev, fun, reg)` is a config dword, `None` when the firmware refuses
/// it; `found(bus, dev, fun, vendor, device)` is called for every function.
pub fn walk(
    start: u8,
    mut read: impl FnMut(u8, u8, u8, u8) -> Option<u32>,
    mut found: impl FnMut(u8, u8, u8, u16, u16),
) {
    let mut seen = [false; 256];
    let mut stack = [0u8; 256];
    stack[0] = start;
    let mut depth = 1;
    while depth > 0 {
        depth -= 1;
        let bus = stack[depth];
        if seen[bus as usize] {
            continue;
        }
        seen[bus as usize] = true;
        for dev in 0..32u8 {
            for fun in 0..8u8 {
                let Some(id) = read(bus, dev, fun, 0) else { continue };
                let vendor = (id & 0xffff) as u16;
                if vendor == 0xffff || vendor == 0 {
                    if fun == 0 {
                        break;
                    }
                    continue;
                }
                let header = read(bus, dev, fun, 0x0c).map_or(0, |v| ((v >> 16) & 0xff) as u8);
                if header & 0x7f == 0x01 {
                    if let Some(v) = read(bus, dev, fun, 0x18) {
                        let secondary = ((v >> 8) & 0xff) as u8;
                        if secondary != 0 && !seen[secondary as usize] && depth < stack.len() {
                            stack[depth] = secondary;
                            depth += 1;
                        }
                    }
                }
                found(bus, dev, fun, vendor, (id >> 16) as u16);
                if fun == 0 && header & 0x80 == 0 {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One root of the R6415 (#186): buses 80..=bf, as forge1's Linux lists them.
    /// A root complex at 80:00.0, a bridge at 80:03.1 to bus 84, and the two
    /// functions of the mezzanine ConnectX-4 Lx. Outside its range the firmware
    /// refuses the read, as EDK2's root bridge does.
    fn r6415(bus: u8, dev: u8, fun: u8, reg: u8) -> Option<u32> {
        if !(0x80..=0xbf).contains(&bus) {
            return None;
        }
        Some(match (bus, dev, fun, reg) {
            (0x80, 0, 0, 0) => 0x1450_1022,
            (0x80, 3, 0, 0) => 0x1452_1022,
            (0x80, 3, 1, 0) => 0x1453_1022,
            (0x80, 3, 0, 0x0c) => 0x0080_0000,
            (0x80, 3, 1, 0x0c) => 0x0081_0000,
            (0x80, 3, 1, 0x18) => 0x0084_8400 | 0x80,
            (0x84, 0, 0, 0) | (0x84, 0, 1, 0) => 0x1015_15b3,
            (0x84, 0, 0, 0x0c) => 0x0080_0000,
            (_, _, _, 0) => 0xffff_ffff,
            _ => 0,
        })
    }

    fn mellanox(start: u8) -> [Option<(u8, u8, u8)>; 2] {
        let mut out = [None; 2];
        let mut n = 0;
        walk(start, r6415, |b, d, f, vendor, device| {
            if vendor == 0x15b3 {
                assert_eq!(device, 0x1015);
                out[n] = Some((b, d, f));
                n += 1;
            }
        });
        out
    }

    #[test]
    fn the_r6415s_card_is_found_from_its_root_bridges_first_bus() {
        assert_eq!(mellanox(0x80), [Some((0x84, 0, 0)), Some((0x84, 0, 1))]);
    }

    #[test]
    fn a_walk_from_bus_0_misses_it_which_was_186() {
        assert_eq!(mellanox(0), [None, None]);
    }

    #[test]
    fn a_bridge_loop_ends() {
        let mut n = 0;
        walk(
            5,
            |bus, dev, fun, reg| match (bus, dev, fun, reg) {
                (5, 0, 0, 0) | (6, 0, 0, 0) => Some(0x0001_1234),
                (5, 0, 0, 0x0c) | (6, 0, 0, 0x0c) => Some(0x0001_0000),
                (5, 0, 0, 0x18) => Some(6 << 8),
                (6, 0, 0, 0x18) => Some(5 << 8),
                (_, _, _, 0) => Some(0xffff_ffff),
                _ => Some(0),
            },
            |_, _, _, _, _| n += 1,
        );
        assert_eq!(n, 2);
    }

    /// A descriptor as UEFI lays it out: min at 0x0e, max at 0x16, length at 0x26.
    fn qword(kind: u8, min: u64, max: u64, len: u64) -> [u8; QWORD_LEN] {
        let mut d = [0u8; QWORD_LEN];
        d[0] = TAG_QWORD;
        d[3] = kind;
        d[0x0e..0x16].copy_from_slice(&min.to_le_bytes());
        d[0x16..0x1e].copy_from_slice(&max.to_le_bytes());
        d[0x26..0x2e].copy_from_slice(&len.to_le_bytes());
        d
    }

    fn range_of(table: &[u8]) -> Option<(u8, u8)> {
        bus_range(|i| table.get(i).copied().unwrap_or(TAG_END))
    }

    #[test]
    fn the_bus_range_is_read_past_the_memory_and_io_descriptors() {
        let mut t = [0u8; 3 * QWORD_LEN + 2];
        t[..QWORD_LEN].copy_from_slice(&qword(0, 0xe000_0000, 0xefff_ffff, 0x1000_0000));
        t[QWORD_LEN..2 * QWORD_LEN].copy_from_slice(&qword(1, 0x1000, 0x1fff, 0x1000));
        t[2 * QWORD_LEN..3 * QWORD_LEN].copy_from_slice(&qword(2, 0x80, 0xbf, 0x40));
        t[3 * QWORD_LEN] = TAG_END;
        assert_eq!(range_of(&t), Some((0x80, 0xbf)));
    }

    #[test]
    fn a_length_outranks_a_maximum_of_zero() {
        assert_eq!(range_of(&qword(2, 0x40, 0, 0x20)), Some((0x40, 0x5f)));
        assert_eq!(range_of(&qword(2, 0x40, 0x5f, 0)), Some((0x40, 0x5f)));
        assert_eq!(range_of(&qword(2, 0x40, 0, 0)), Some((0x40, 0xff)));
        assert_eq!(range_of(&qword(2, 0x80, 0, 0x1000)), Some((0x80, 0xff)));
    }

    #[test]
    fn a_table_with_no_bus_range_is_none_and_never_a_panic() {
        assert_eq!(range_of(&[TAG_END]), None);
        assert_eq!(range_of(&[0x88, 0, 0, 2]), None);
        assert_eq!(range_of(&qword(0, 0, 0xff, 0x100)), None);
        assert_eq!(range_of(&qword(2, 0x100, 0, 1)), None);
    }

    /// Boots of a machine, with a card whose NV keeps a write or doesn't.
    /// Returns how many boots wrote, each write being one warm reset.
    fn resets(keeps_it: bool, boots: usize) -> usize {
        let (mut next_is_rs, mut tried, mut resets) = (false, false, 0);
        for _ in 0..boots {
            match pin_action(1, !next_is_rs as usize, 0, tried) {
                Pin::Write => {
                    tried = true;
                    next_is_rs = keeps_it;
                    resets += 1;
                }
                Pin::Held => tried = false,
                Pin::Nothing | Pin::Stuck => {}
            }
        }
        resets
    }

    #[test]
    fn the_pin_resets_once_whether_or_not_the_card_keeps_it() {
        assert_eq!(resets(true, 10), 1);
        assert_eq!(resets(false, 10), 1);
    }

    #[test]
    fn the_pin_writes_only_a_difference_and_only_once() {
        assert_eq!(pin_action(2, 0, 0, false), Pin::Nothing);
        assert_eq!(pin_action(2, 1, 0, false), Pin::Write);
        assert_eq!(pin_action(2, 1, 0, true), Pin::Stuck);
        assert_eq!(pin_action(2, 0, 0, true), Pin::Held);
    }

    #[test]
    fn a_missing_card_or_a_failed_read_keeps_the_record() {
        assert_eq!(pin_action(0, 0, 0, true), Pin::Nothing);
        assert_eq!(pin_action(1, 0, 1, true), Pin::Nothing);
        assert_eq!(pin_action(0, 0, 1, false), Pin::Nothing);
    }

    #[test]
    fn the_x9_blades_connectx3_has_no_fec_and_is_still_a_connectx() {
        assert_eq!(kind(0x1003), Kind::NoFec("ConnectX-3"));
        assert_eq!(kind(0x1007), Kind::NoFec("ConnectX-3 Pro"));
        // server1's console (#25): one function seen, so no "no ConnectX".
        assert_eq!(closing_line(1), None);
    }

    #[test]
    fn mlx5_takes_what_its_driver_claims_and_nothing_else() {
        assert_eq!(MLX5_TAKE_OVER.len(), 7);
        // The R230's card.
        assert!(MLX5_TAKE_OVER.contains(&(0x15b3, 0x1015)));
        for &(vendor, device) in MLX5_TAKE_OVER {
            assert_eq!(vendor, 0x15b3);
            // Each a known physical function: never a VF, whose id is unknown.
            assert!(matches!(kind(device), Kind::Fec(_)), "{device:04x}");
        }
        // The driver does not claim ConnectX-7, and mlx4's parts are not mlx5.
        for device in [0x1021, 0x1003, 0x1007, 0x1016] {
            assert!(!MLX5_TAKE_OVER.contains(&(0x15b3, device)), "{device:04x}");
        }
    }

    #[test]
    fn the_r230s_connectx4_lx_is_read() {
        assert_eq!(kind(0x1015), Kind::Fec("ConnectX-4 Lx"));
        assert_eq!(kind(0x1021), Kind::Fec("ConnectX-7"));
    }

    #[test]
    fn a_virtual_function_or_an_unknown_part_is_unknown() {
        // ConnectX-3 VF, ConnectX-4 Lx VF, a BlueField.
        for id in [0x1004, 0x1016, 0xa2d6] {
            assert_eq!(kind(id), Kind::Unknown, "{id:#x}");
        }
    }

    #[test]
    fn no_connectx_only_when_nothing_from_mellanox() {
        assert_eq!(closing_line(0), Some("no ConnectX on the bus"));
        assert_eq!(closing_line(3), None);
    }
}
