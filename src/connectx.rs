//! Which Mellanox PCI functions `mlxfec.rs` reads FEC from, and what the
//! console says about the others (#25).
//!
//! A ConnectX-4 or later physical function has the 25G-and-up NV FEC
//! configuration `mlxfec` reads (and on a recovery stick writes). A
//! ConnectX-3 or ConnectX-3 Pro (the Supermicro X9 blades' 10/40G card) has
//! no RS-FEC at all: it is a ConnectX, and it has nothing to report. That is
//! a different line from a Mellanox function this code doesn't know (a
//! virtual function, a newer part), and neither is "no ConnectX on the bus",
//! which before #25 the console printed beside a ConnectX-3 it had just
//! listed.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_x9_blades_connectx3_has_no_fec_and_is_still_a_connectx() {
        assert_eq!(kind(0x1003), Kind::NoFec("ConnectX-3"));
        assert_eq!(kind(0x1007), Kind::NoFec("ConnectX-3 Pro"));
        // server1's console (#25): one function seen, so no "no ConnectX".
        assert_eq!(closing_line(1), None);
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
