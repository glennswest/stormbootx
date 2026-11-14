//! A PCI root bridge's bus range (#186), for the failure console's `pci`.
//!
//! A machine can have several PCI root bridges, each with its own bus range.
//! The R6415's EPYC has four, at buses 00, 40, 80 and c0, and its card is at
//! 84:00.x. A walk that starts at bus 0 on each bridge finds nothing past the
//! first, because firmware refuses a config read outside a bridge's range.
//!
//! `core` only and no `crate::` item, so the tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/pcibus.rs -o t/pcibus-test && ./t/pcibus-test
//! ```

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
