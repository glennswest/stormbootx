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

/// A port's speed class (#203), named as `fec_table.<name>` names it.
/// Declared slowest first, so the highest of a port's capabilities is `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Speed {
    G10,
    G25,
    G40,
    G50,
    G50Pam4,
    G100,
    G100Pam4,
    G200,
    G400,
}

impl Speed {
    pub fn name(self) -> &'static str {
        match self {
            Speed::G10 => "10g",
            Speed::G25 => "25g",
            Speed::G40 => "40g",
            Speed::G50 => "50g",
            Speed::G50Pam4 => "50g-pam4",
            Speed::G100 => "100g",
            Speed::G100Pam4 => "100g-pam4",
            Speed::G200 => "200g",
            Speed::G400 => "400g",
        }
    }

    /// The FEC the standard gives this speed (IEEE 802.3 and the 25G/50G
    /// Ethernet Consortium): none at 10G and 40G, RS(528) at 25G, 50G over
    /// two lanes and 100G NRZ, RS(544) at the PAM4 speeds. Never the card's
    /// negotiated default: the switch does not negotiate either (owner,
    /// 2026-11-13: "Double auto is a problem"). The card has one RS value, so
    /// `rs-544` is `rs` to it.
    pub fn standard(self) -> &'static str {
        match self {
            Speed::G10 | Speed::G40 => "none",
            Speed::G25 | Speed::G50 | Speed::G100 => "rs",
            Speed::G50Pam4 | Speed::G100Pam4 | Speed::G200 | Speed::G400 => "rs-544",
        }
    }
}

/// PTYS `eth_proto_capability` bits (the PRM, Linux's `mlx5e_link_modes`).
const LEGACY_SPEEDS: &[(u32, Speed)] = &[
    (2, Speed::G10),  // 10GBASE-CX4
    (3, Speed::G10),  // 10GBASE-KX4
    (4, Speed::G10),  // 10GBASE-KR
    (12, Speed::G10), // 10GBASE-CR
    (13, Speed::G10), // 10GBASE-SR
    (14, Speed::G10), // 10GBASE-ER/LR
    (26, Speed::G10), // 10GBASE-T
    (6, Speed::G40),  // 40GBASE-CR4
    (7, Speed::G40),  // 40GBASE-KR4
    (15, Speed::G40), // 40GBASE-SR4
    (16, Speed::G40), // 40GBASE-LR4/ER4
    (27, Speed::G25), // 25GBASE-CR
    (28, Speed::G25), // 25GBASE-KR
    (29, Speed::G25), // 25GBASE-SR
    (18, Speed::G50), // 50GBASE-SR2
    (30, Speed::G50), // 50GBASE-CR2
    (31, Speed::G50), // 50GBASE-KR2
    (20, Speed::G100), // 100GBASE-CR4
    (21, Speed::G100), // 100GBASE-SR4
    (22, Speed::G100), // 100GBASE-KR4
    (23, Speed::G100), // 100GBASE-LR4/ER4
];

/// PTYS `ext_eth_proto_capability` bits (ConnectX-6 on; `mlx5e_ext_link_mode`).
const EXT_SPEEDS: &[(u32, Speed)] = &[
    (4, Speed::G10),       // 10GBASE-XFI/XAUI-1
    (5, Speed::G40),       // 40GBASE-XLAUI-4
    (6, Speed::G25),       // 25GAUI-1, 25GBASE-CR/KR
    (7, Speed::G50),       // 50GAUI-2, 50GBASE-CR2/KR2
    (8, Speed::G50Pam4),   // 50GAUI-1, 50GBASE-CR/KR
    (9, Speed::G100),      // CAUI-4, 100GBASE-CR4/KR4
    (10, Speed::G100Pam4), // 100GAUI-2
    (11, Speed::G100Pam4), // 100GAUI-1
    (12, Speed::G200),     // 200GAUI-4
    (13, Speed::G200),     // 200GAUI-2
    (14, Speed::G200),     // 200GAUI-1
    (15, Speed::G400),     // 400GAUI-8
    (16, Speed::G400),     // 400GAUI-4
];

/// A port's speed: the highest the card is **capable** of on it (PTYS,
/// `eth_proto_capability` and its extended form), never what the link runs
/// at, so no link state reaches the pin (#203). The extended field wins when
/// the card fills it. `None` below 10G or when nothing is known.
pub fn speed_of(eth_cap: u32, ext_cap: u32) -> Option<Speed> {
    let (bits, table) = if ext_cap != 0 { (ext_cap, EXT_SPEEDS) } else { (eth_cap, LEGACY_SPEEDS) };
    table.iter().filter(|(b, _)| bits & (1 << b) != 0).map(|(_, s)| *s).max()
}

/// Where a port's wanted FEC came from (#203).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `fec_pin.<MAC>` or `fec_pin.<BDF>`.
    Port,
    /// `fec_pin =`.
    Pin,
    /// `fec_table.<speed> =`.
    Table(Speed),
    /// The standard's FEC for a port that is not a 25G one.
    Standard(Speed),
}

/// Why a port is left as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// No `fec_pin` for it at all.
    NotPinned,
    /// `fec_pin = off`.
    Off,
    /// Its own key says `off`.
    PortOff,
    /// `fec_table.<speed> = off`.
    TableOff(Speed),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice<'a> {
    Leave(Why),
    Want(&'a str, Source),
}

/// One port's wanted FEC (#203; the owner, 2026-11-13: "nothing is to be
/// hard-coded"; "no auto - 25gig = rs"; "set the fec standard per card nic
/// optic speed standard. Double auto is a problem"). Its own key (`port`)
/// beats everything; `off` leaves it alone. Otherwise each port takes its
/// speed's (`speed`: the highest in its PTYS capability, never the link):
/// `fec_table.<speed>` (`table`, already looked up for that speed) if the
/// media names one, else `fec_pin` at 25G, else the standard's. A port whose
/// speed the card did not say takes `fec_pin`: the media named it. Values
/// arrive lower-cased; the caller says whether one is a FEC at all.
pub fn choose<'a>(pin: Option<&'a str>, port: Option<&'a str>, table: Option<&'a str>, speed: Option<Speed>) -> Choice<'a> {
    let (value, source) = match (port, pin) {
        (Some(v), _) => (v, Source::Port),
        (None, Some(v)) => (v, Source::Pin),
        (None, None) => return Choice::Leave(Why::NotPinned),
    };
    match value {
        "off" if source == Source::Port => return Choice::Leave(Why::PortOff),
        "off" => return Choice::Leave(Why::Off),
        _ if source == Source::Port => return Choice::Want(value, source),
        _ => {}
    }
    match (speed, table) {
        (Some(s), Some("off")) => Choice::Leave(Why::TableOff(s)),
        (Some(s), Some(t)) => Choice::Want(t, Source::Table(s)),
        (Some(Speed::G25), None) | (None, _) => Choice::Want(value, Source::Pin),
        (Some(s), None) => Choice::Want(s.standard(), Source::Standard(s)),
    }
}

// --- PPLM, the port's live FEC (#202) ---------------------------------------
//
// forge1's ConnectX-4 Lx (15b3:1015) has no FEC NV TLV at all: MNVDA answers
// "resource not available" stored and as a default. The FEC is then set where
// Linux's `ethtool --set-fec` sets it: PPLM, the port's FEC override, through
// the same access register. Layout from Linux's `mlx5_ifc_pplm_reg_bits`
// (0x40 bytes): dword 0 `local_port[23:16]`, dword 3 `fec_mode_active[23:0]`,
// dword 4 the override capabilities and dword 5 the admin values, both
// `56g[19:16] 100g[15:12] 50g[11:8] 25g[7:4] 10g_40g[3:0]`, each a bitmask
// over `MLX5E_FEC_*`: bit 0 no FEC, bit 1 FireCode, bit 2 RS(528), and in
// `fec_mode_active` bit 7 RS(544). An admin of 0 is the firmware's choice.

/// PPLM's register id and size.
pub const REG_PPLM: u16 = 0x5023;
pub const PPLM_DWORDS: usize = 16;
pub const PPLM_ACTIVE: usize = 3;
pub const PPLM_CAP: usize = 4;
pub const PPLM_ADMIN: usize = 5;

/// The NV FEC numbers (`nv_link_phy_conf.phy_fec_override`), which `mlxfec::Fec`
/// reads: 0 device default, 1 off, 2 RS, 3 FireCode. PPLM is spoken in them so
/// a port reads the same whichever way it was read.
pub const NV_DEFAULT: u32 = 0;
pub const NV_OFF: u32 = 1;
pub const NV_RS: u32 = 2;
pub const NV_FC: u32 = 3;

/// Where a speed's 4-bit field sits in PPLM's capability and admin dwords.
/// `None` for the PAM4 speeds, which live in other dwords this does not write
/// (no card here runs them). A port whose speed is unknown is taken as 25G:
/// it is `fec_pin`'s, and `fec_pin` is the 25G ports' FEC.
pub fn pplm_shift(speed: Option<Speed>) -> Option<u32> {
    match speed {
        Some(Speed::G10) | Some(Speed::G40) => Some(0),
        Some(Speed::G25) | None => Some(4),
        Some(Speed::G50) => Some(8),
        Some(Speed::G100) => Some(12),
        _ => None,
    }
}

/// A PPLM admin or capability bit for an NV FEC number; 0 (the firmware's
/// choice) for the default, `None` for anything else.
pub fn pplm_bit(nv: u32) -> Option<u32> {
    match nv {
        NV_DEFAULT => Some(0),
        NV_OFF => Some(1),
        NV_FC => Some(2),
        NV_RS => Some(4),
        _ => None,
    }
}

/// The admin field as an NV FEC number: one bit, or 0. `None` for several
/// bits, which this code never writes.
pub fn pplm_admin_nv(dword: u32, shift: u32) -> Option<u32> {
    match (dword >> shift) & 0xf {
        0 => Some(NV_DEFAULT),
        1 => Some(NV_OFF),
        2 => Some(NV_FC),
        4 => Some(NV_RS),
        _ => None,
    }
}

/// `fec_mode_active` as an NV FEC number, as Linux reads it (a bitmask, its
/// first bit). `None` for 0, nothing running (a link that is not up), or a
/// mode with no NV number.
pub fn pplm_active_nv(active: u32) -> Option<u32> {
    let a = active & 0x00ff_ffff;
    if a == 0 {
        return None;
    }
    match a.trailing_zeros() {
        0 => Some(NV_OFF),
        1 => Some(NV_FC),
        2 | 7 => Some(NV_RS),
        _ => None,
    }
}

/// The admin dword with one speed's field set to `nv`, every other field kept.
/// `None` when the capability dword does not offer it there (the default is
/// always offered).
pub fn pplm_set_admin(admin: u32, cap: u32, shift: u32, nv: u32) -> Option<u32> {
    let bit = pplm_bit(nv)?;
    if bit != 0 && (cap >> shift) & bit == 0 {
        return None;
    }
    Some((admin & !(0xf << shift)) | (bit << shift))
}

/// PAOS (0x5006, 0x10 bytes; `mlx5_ifc_paos_reg_bits`): dword 0
/// `local_port[23:16] admin_status[11:8] oper_status[3:0]`, dword 1 `ase[31]`,
/// the admin-status update enable. Status 1 up, 2 down.
pub const REG_PAOS: u16 = 0x5006;
pub const PAOS_DWORDS: usize = 4;
pub const PAOS_UP: u32 = 1;
pub const PAOS_DOWN: u32 = 2;

pub fn paos_admin(dword0: u32) -> u32 {
    (dword0 >> 8) & 0xf
}

/// PAOS's first two dwords to set a local port's admin status.
pub fn paos_set(port: u8, status: u32) -> [u32; 2] {
    [((port as u32) << 16) | ((status & 0xf) << 8), 1 << 31]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pplm_speaks_nv_numbers() {
        // 25G at [7:4]; unknown speed is fec_pin's, the 25G field.
        assert_eq!(pplm_shift(Some(Speed::G25)), Some(4));
        assert_eq!(pplm_shift(None), Some(4));
        assert_eq!(pplm_shift(Some(Speed::G10)), Some(0));
        assert_eq!(pplm_shift(Some(Speed::G100)), Some(12));
        assert_eq!(pplm_shift(Some(Speed::G200)), None);
        assert_eq!(pplm_admin_nv(0x40, 4), Some(NV_RS));
        assert_eq!(pplm_admin_nv(0x10, 4), Some(NV_OFF));
        assert_eq!(pplm_admin_nv(0x20, 4), Some(NV_FC));
        assert_eq!(pplm_admin_nv(0x0f0f, 4), Some(NV_DEFAULT));
        assert_eq!(pplm_admin_nv(0x60, 4), None);
        // Linux's bitmask: RS(528) is bit 2, RS(544) bit 7; 0 runs nothing.
        assert_eq!(pplm_active_nv(0x4), Some(NV_RS));
        assert_eq!(pplm_active_nv(0x80), Some(NV_RS));
        assert_eq!(pplm_active_nv(0x1), Some(NV_OFF));
        assert_eq!(pplm_active_nv(0x2), Some(NV_FC));
        assert_eq!(pplm_active_nv(0), None);
        assert_eq!(pplm_active_nv(0xff00_0000), None);
    }

    #[test]
    fn pplm_admin_write_keeps_every_other_speed() {
        // A ConnectX-4 Lx: 25G offers none, FC and RS (0x7 at [7:4]).
        let cap = 0x0000_0071;
        assert_eq!(pplm_set_admin(0x0000_0001, cap, 4, NV_RS), Some(0x0000_0041));
        assert_eq!(pplm_set_admin(0x0000_0021, cap, 4, NV_RS), Some(0x0000_0041));
        assert_eq!(pplm_set_admin(0x0000_0041, cap, 4, NV_DEFAULT), Some(0x0000_0001));
        // Not offered: refused, never written blind.
        assert_eq!(pplm_set_admin(0, 0x0000_0030, 4, NV_RS), None);
        assert_eq!(pplm_set_admin(0, cap, 4, 7), None);
    }

    #[test]
    fn paos_fields() {
        assert_eq!(paos_admin(0x0001_0101), PAOS_UP);
        assert_eq!(paos_set(1, PAOS_DOWN), [0x0001_0200, 0x8000_0000]);
        assert_eq!(paos_set(2, PAOS_UP), [0x0002_0100, 0x8000_0000]);
    }

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
    #[test]
    fn forge1s_connectx4_lx_is_a_25g_port() {
        // 1G, 10GBASE-KR/CR/SR, 25GBASE-CR/KR/SR: what a ConnectX-4 Lx states.
        let cap = (1 << 1) | (1 << 4) | (1 << 12) | (1 << 13) | (1 << 27) | (1 << 28) | (1 << 29);
        assert_eq!(speed_of(cap, 0), Some(Speed::G25));
    }

    #[test]
    fn the_highest_capability_wins_and_the_extended_field_outranks_the_legacy_one() {
        assert_eq!(speed_of((1 << 13) | (1 << 15), 0), Some(Speed::G40));
        assert_eq!(speed_of((1 << 20) | (1 << 27), 0), Some(Speed::G100));
        assert_eq!(speed_of(1 << 27, (1 << 6) | (1 << 12)), Some(Speed::G200));
        assert_eq!(speed_of(0, 1 << 8), Some(Speed::G50Pam4));
    }

    #[test]
    fn below_10g_or_nothing_is_no_speed() {
        assert_eq!(speed_of(0, 0), None);
        assert_eq!(speed_of((1 << 0) | (1 << 1) | (1 << 25), 0), None);
    }

    #[test]
    fn the_standard_never_negotiates() {
        use Speed::*;
        for s in [G10, G25, G40, G50, G50Pam4, G100, G100Pam4, G200, G400] {
            assert!(["none", "rs", "rs-544"].contains(&s.standard()), "{}", s.name());
        }
        assert_eq!(G25.standard(), "rs");
        assert_eq!(G10.standard(), "none");
    }

    #[test]
    fn a_ports_own_key_beats_fec_pin() {
        assert_eq!(choose(Some("rs"), Some("fc"), None, None), Choice::Want("fc", Source::Port));
        assert_eq!(choose(Some("rs"), Some("off"), None, None), Choice::Leave(Why::PortOff));
        assert_eq!(choose(None, Some("rs"), None, None), Choice::Want("rs", Source::Port));
        assert_eq!(choose(Some("rs"), None, None, Some(Speed::G25)), Choice::Want("rs", Source::Pin));
    }

    #[test]
    fn off_or_nothing_leaves_the_port() {
        assert_eq!(choose(None, None, Some("fc"), Some(Speed::G25)), Choice::Leave(Why::NotPinned));
        assert_eq!(choose(Some("off"), None, None, Some(Speed::G25)), Choice::Leave(Why::Off));
    }

    #[test]
    fn fec_pin_is_the_25g_ports_fec() {
        let g25 = Some(Speed::G25);
        let g10 = Some(Speed::G10);
        assert_eq!(choose(Some("rs"), None, None, g25), Choice::Want("rs", Source::Pin));
        assert_eq!(choose(Some("rs"), None, Some("fc"), g25), Choice::Want("fc", Source::Table(Speed::G25)));
        assert_eq!(choose(Some("rs"), None, Some("off"), g25), Choice::Leave(Why::TableOff(Speed::G25)));
        // Another speed: its table entry, else the standard's.
        assert_eq!(choose(Some("rs"), None, None, g10), Choice::Want("none", Source::Standard(Speed::G10)));
        assert_eq!(choose(Some("rs"), None, None, Some(Speed::G100)), Choice::Want("rs", Source::Standard(Speed::G100)));
        assert_eq!(choose(Some("rs"), None, None, Some(Speed::G50Pam4)), Choice::Want("rs-544", Source::Standard(Speed::G50Pam4)));
        assert_eq!(choose(Some("rs"), None, Some("none"), g10), Choice::Want("none", Source::Table(Speed::G10)));
        // The card did not say: the media's value.
        assert_eq!(choose(Some("rs"), None, Some("fc"), None), Choice::Want("rs", Source::Pin));
        // A port's own key beats the table.
        assert_eq!(choose(Some("rs"), Some("rs"), Some("off"), g10), Choice::Want("rs", Source::Port));
    }
}
