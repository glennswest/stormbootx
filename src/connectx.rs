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

/// How far the pin has gone for the difference it is settling (#203), from
/// `StormBootFecPin`'s record: nothing yet, one warm reset, one cold reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    None,
    Warm,
    Cold,
}

/// The reset that makes the card load its NV value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset {
    Warm,
    Cold,
}

/// What the FEC pin does this boot (#186, #203).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// Nothing to write, and no record to settle.
    Nothing,
    /// The card runs, and will boot with, the wanted FEC after a pin: forget
    /// the record.
    Held,
    /// A port's next-boot value differs: write it, then this reset.
    Write(Reset),
    /// Every next-boot value is right but a port still runs another FEC: this
    /// reset, so the card loads it.
    Reset(Reset),
    /// A warm and a cold reset did not get the card running the wanted FEC:
    /// never write or reset again; the boot does not go on (the owner,
    /// 2026-11-13: "the fec needs to be right other no point in booting").
    Stuck,
}

/// The pin's whole decision. Its inputs are what the card's NV said (`ports`
/// read; `next_differ` of them with another next-boot value than wanted,
/// `run_differ` running another; `failed` reads) and how far a pin has gone.
/// **Link state is not an input, by the owner's terms and on purpose**: one
/// early link sample is what cost the R230 its 25G ports on 2026-09-07, and
/// a port is right when it runs the wanted FEC, not when it is up. A record is
/// forgotten only on a whole read saying both are right, so a card that went
/// missing or a failed read cannot re-arm a reset. Each stage is one reset:
/// warm, then cold, then stuck; never a loop.
pub fn pin_action(ports: usize, next_differ: usize, run_differ: usize, failed: usize, tried: Stage) -> Pin {
    if next_differ == 0 && run_differ == 0 {
        return if tried != Stage::None && ports > 0 && failed == 0 { Pin::Held } else { Pin::Nothing };
    }
    let reset = match tried {
        Stage::None => Reset::Warm,
        Stage::Warm => Reset::Cold,
        Stage::Cold => return Pin::Stuck,
    };
    if next_differ > 0 { Pin::Write(reset) } else { Pin::Reset(reset) }
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

/// Tries (1 ms apart) after which one plausible ticket that never moved is
/// the UEFI driver's parked lock, not a holder about to let go (#99): the
/// whole cooperative wait cost the Dell 2 s per ConnectX-4 Lx function.
pub const PARKED_TRIES: u32 = 50;

/// Is the cap9 lock parked, so that waiting longer only costs time? `moved`
/// counts the held value changing. A live holder's value moves and gets the
/// whole wait; 0 is no holder, and all ones is a read that never reached the
/// card, so neither is a ticket to take over.
pub fn parked(tries: u32, moved: u32, held: u32) -> bool {
    tries >= PARKED_TRIES && moved == 0 && held != 0 && held != 0xffff_ffff
}

/// Which value an MNVDA read asks for (#202): the one stored for the next card
/// reset, the one loaded at the last reset, or the firmware's default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NvQuery {
    Next,
    Current,
    Default,
}

/// MNVDA's register status for "no such TLV stored" (reg_access, status 8).
pub const REG_RESOURCE_NOT_AVAILABLE: u32 = 8;

/// The NV header's first dword (tools_open_nv_hdr_fifth_gen,
/// tools_open_layouts.h): length [8:0], writer_id [20:16] (mlxconfig's 0x9),
/// read_current [22], default_ [23], over_en [25] as mlxconfig sets it.
///
/// A TLV nobody has ever written is not in NV, and a plain read of it answers
/// "resource not available" (#202: forge1's ConnectX-4 Lx, never configured,
/// read as 0 ports). mlxconfig then reads the same TLV with `default_` set:
/// the value the card runs on. `mlxfec` does the same; only when the default
/// is absent too is the port not there.
pub fn nv_hdr(len: u32, q: NvQuery) -> u32 {
    let mut d = (len & 0x1ff) | (0x9 << 16) | (1 << 25);
    match q {
        NvQuery::Next => {}
        NvQuery::Current => d |= 1 << 22,
        NvQuery::Default => d |= 1 << 23,
    }
    d
}

/// The reads one value takes, in order (#202): what is stored, then the
/// default when nothing is.
pub fn nv_fallback(q: NvQuery) -> &'static [NvQuery] {
    match q {
        NvQuery::Next => &[NvQuery::Next, NvQuery::Default],
        NvQuery::Current => &[NvQuery::Current, NvQuery::Default],
        NvQuery::Default => &[NvQuery::Default],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv_header_bits_are_mlxconfigs() {
        // 256 bytes, writer mlxconfig, over_en: the read the R230 always made.
        assert_eq!(nv_hdr(256, NvQuery::Next), 0x0209_0100);
        assert_eq!(nv_hdr(256, NvQuery::Current), 0x0249_0100);
        assert_eq!(nv_hdr(256, NvQuery::Default), 0x0289_0100);
        assert_eq!(nv_hdr(8, NvQuery::Next) & 0x1ff, 8);
    }

    #[test]
    fn a_never_written_tlv_reads_its_default() {
        // forge1 (#202): nothing stored must not read as no port.
        assert_eq!(nv_fallback(NvQuery::Next), &[NvQuery::Next, NvQuery::Default]);
        assert_eq!(nv_fallback(NvQuery::Current), &[NvQuery::Current, NvQuery::Default]);
        assert_eq!(nv_fallback(NvQuery::Default), &[NvQuery::Default]);
        assert_eq!(REG_RESOURCE_NOT_AVAILABLE, 8);
    }

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

    /// Boots of a machine. `keeps` says whether the card's NV keeps a write,
    /// `loads` after which reset (0 none, 1 warm, 2 cold) the running value
    /// follows NV. Returns the resets done and whether the last boot was stuck.
    fn boots(keeps: bool, loads: u8, n: usize) -> (usize, bool) {
        let (mut next_ok, mut run_ok, mut tried, mut resets, mut stuck) = (false, false, Stage::None, 0, false);
        for _ in 0..n {
            stuck = false;
            let act = pin_action(1, !next_ok as usize, !run_ok as usize, 0, tried);
            let reset = match act {
                Pin::Write(r) => {
                    next_ok = keeps;
                    Some(r)
                }
                Pin::Reset(r) => Some(r),
                Pin::Held => {
                    tried = Stage::None;
                    None
                }
                Pin::Stuck => {
                    stuck = true;
                    None
                }
                Pin::Nothing => None,
            };
            if let Some(r) = reset {
                resets += 1;
                tried = if r == Reset::Warm { Stage::Warm } else { Stage::Cold };
                let level = if r == Reset::Warm { 1 } else { 2 };
                if loads != 0 && level >= loads {
                    run_ok = next_ok;
                }
            }
        }
        (resets, stuck)
    }

    #[test]
    fn a_card_that_loads_on_a_warm_reset_takes_one() {
        assert_eq!(boots(true, 1, 10), (1, false));
    }

    #[test]
    fn a_card_that_needs_a_cold_reset_gets_one_after_the_warm() {
        assert_eq!(boots(true, 2, 10), (2, false));
    }

    #[test]
    fn a_card_that_never_runs_it_stops_after_two_resets_and_does_not_boot() {
        assert_eq!(boots(true, 0, 10), (2, true));
        assert_eq!(boots(false, 1, 10), (2, true));
    }

    #[test]
    fn the_pin_writes_only_a_difference() {
        assert_eq!(pin_action(2, 0, 0, 0, Stage::None), Pin::Nothing);
        assert_eq!(pin_action(2, 1, 1, 0, Stage::None), Pin::Write(Reset::Warm));
        assert_eq!(pin_action(2, 1, 1, 0, Stage::Warm), Pin::Write(Reset::Cold));
        assert_eq!(pin_action(2, 1, 0, 0, Stage::Cold), Pin::Stuck);
        assert_eq!(pin_action(2, 0, 0, 0, Stage::Warm), Pin::Held);
        assert_eq!(pin_action(2, 0, 0, 0, Stage::Cold), Pin::Held);
    }

    #[test]
    fn a_right_next_boot_value_the_card_does_not_run_is_a_reset_with_no_write() {
        assert_eq!(pin_action(2, 0, 1, 0, Stage::None), Pin::Reset(Reset::Warm));
        assert_eq!(pin_action(2, 0, 1, 0, Stage::Warm), Pin::Reset(Reset::Cold));
        assert_eq!(pin_action(2, 0, 2, 0, Stage::Cold), Pin::Stuck);
    }

    #[test]
    fn a_missing_card_or_a_failed_read_keeps_the_record() {
        assert_eq!(pin_action(0, 0, 0, 0, Stage::Warm), Pin::Nothing);
        assert_eq!(pin_action(1, 0, 0, 1, Stage::Cold), Pin::Nothing);
        assert_eq!(pin_action(0, 0, 0, 1, Stage::None), Pin::Nothing);
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

    /// How many tries a run of held values takes to be called parked, or
    /// `None` if it never is; counted as `vsc_lock_timed` counts them.
    fn tries_until_parked(held: impl Fn(u32) -> u32, limit: u32) -> Option<u32> {
        let (mut last, mut moved) = (0, 0);
        for i in 0..limit {
            let now = held(i);
            if i > 0 && now != last {
                moved += 1;
            }
            last = now;
            if parked(i + 1, moved, last) {
                return Some(i + 1);
            }
        }
        None
    }

    #[test]
    fn a_parked_lock_is_known_after_50_tries_not_the_whole_wait() {
        // The R230's values: 0x3, 0x7 on a cold boot, 0x462, 0x466 on a warm one.
        for ticket in [0x3, 0x7, 0x462, 0x466] {
            assert_eq!(tries_until_parked(|_| ticket, 2048), Some(PARKED_TRIES));
        }
    }

    #[test]
    fn a_value_that_moves_keeps_the_whole_wait() {
        assert_eq!(tries_until_parked(|i| 3 + i, 2048), None);
        // Once, late: the holder is alive, however still it looked before.
        assert_eq!(tries_until_parked(|i| if i < 40 { 3 } else { 7 }, 2048), None);
    }

    #[test]
    fn no_holder_and_a_dead_read_are_never_parked() {
        assert_eq!(tries_until_parked(|_| 0, 2048), None);
        assert_eq!(tries_until_parked(|_| 0xffff_ffff, 2048), None);
    }
}
