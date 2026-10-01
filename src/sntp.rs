//! SNTP (RFC 4330) and UTC calendar arithmetic, for setting the hardware
//! clock before Linux starts (#77).
//!
//! The X9 blades have no RTC battery, and neither their BIOS nor their BMC
//! sets the clock from NTP: after a power cut every one of them boots in
//! 2026-01-01 or 1970. stormbootx has a network before any OS does, so it asks
//! one NTP server once and, through `clock.rs`, sets the RTC with UEFI
//! `SetTime`. This module is the wire format and the arithmetic; the exchange
//! and the firmware calls are `clock.rs`'s.
//!
//! - **The request** is the 48-byte client packet: LI 0, version 4, mode 3,
//!   and a random transmit timestamp. Nothing else is filled in (RFC 4330
//!   §5 allows that) so nothing about this machine's clock leaves it.
//! - **A reply is believed only if** it is a server reply (mode 4), the server
//!   is synchronised (LI ≠ 3, stratum 1..15; stratum 0 is a kiss-o'-death),
//!   its transmit timestamp is non-zero, and its originate timestamp echoes
//!   ours, so a stray or spoofed datagram cannot set the clock.
//! - **Era:** NTP seconds wrap in 2036. A timestamp with the top bit clear is
//!   read as era 1 (after 2036-02-07), as RFC 4330 §3 suggests, which covers
//!   1968..2104.
//! - **The calendar** is proleptic Gregorian UTC, from Howard Hinnant's
//!   `civil_from_days`/`days_from_civil`. No time zone: the RTC holds UTC
//!   (`clock.rs`).
//!
//! Same constraints as `intent.rs`, `sha256.rs`, `universal.rs`, `dnsname.rs`
//! and `esp.rs`: `core` only and no `crate::` item, so the tests run on the
//! host:
//!
//! ```text
//! rustc --edition 2021 --test src/sntp.rs -o t/sntp-test
//! ```

/// The size of an SNTP packet with no extensions.
pub const PACKET: usize = 48;

/// The NTP port.
pub const PORT: u16 = 123;

/// Seconds from 1900-01-01 (the NTP epoch) to 1970-01-01.
const NTP_TO_UNIX: u64 = 2_208_988_800;

/// The client request: LI 0, VN 4, mode 3, and `nonce` as the transmit
/// timestamp, which the server echoes back as the originate timestamp.
pub fn request(nonce: u64) -> [u8; PACKET] {
    let mut p = [0u8; PACKET];
    p[0] = 0x23;
    p[40..48].copy_from_slice(&nonce.to_be_bytes());
    p
}

/// Why a reply was not believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Short,
    NotAServerReply,
    Unsynchronised,
    KissOfDeath,
    BadStratum,
    NotOurs,
    NoTime,
}

impl Reject {
    pub fn text(self) -> &'static str {
        match self {
            Reject::Short => "a reply shorter than 48 bytes",
            Reject::NotAServerReply => "not a server reply (mode 4)",
            Reject::Unsynchronised => "the server says it is not synchronised (LI 3)",
            Reject::KissOfDeath => "the server refused (stratum 0, kiss-o'-death)",
            Reject::BadStratum => "a stratum above 15",
            Reject::NotOurs => "a reply to another request (originate timestamp differs)",
            Reject::NoTime => "a reply with no transmit timestamp",
        }
    }
}

/// The server's transmit time from a reply to the request carrying `nonce`,
/// in nanoseconds since 1970-01-01 UTC.
pub fn reply(msg: &[u8], nonce: u64) -> Result<u64, Reject> {
    if msg.len() < PACKET {
        return Err(Reject::Short);
    }
    if msg[0] & 0x07 != 4 {
        return Err(Reject::NotAServerReply);
    }
    if msg[0] >> 6 == 3 {
        return Err(Reject::Unsynchronised);
    }
    match msg[1] {
        0 => return Err(Reject::KissOfDeath),
        1..=15 => {}
        _ => return Err(Reject::BadStratum),
    }
    if msg[24..32] != nonce.to_be_bytes() {
        return Err(Reject::NotOurs);
    }
    let secs = u32::from_be_bytes([msg[40], msg[41], msg[42], msg[43]]) as u64;
    let frac = u32::from_be_bytes([msg[44], msg[45], msg[46], msg[47]]) as u64;
    if secs == 0 && frac == 0 {
        return Err(Reject::NoTime);
    }
    // Era 1 when the top bit is clear (RFC 4330 §3).
    let secs = if secs & 0x8000_0000 == 0 { secs + (1 << 32) } else { secs };
    let unix = secs.checked_sub(NTP_TO_UNIX).ok_or(Reject::NoTime)?;
    Ok(unix * 1_000_000_000 + ((frac * 1_000_000_000) >> 32))
}

/// A UTC calendar time to the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// The calendar time of `secs` seconds after 1970-01-01 UTC.
pub fn civil(secs: u64) -> Civil {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil_from_days, with the era shifted to start on 0000-03-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Civil {
        year: year as u16,
        month: month as u8,
        day: day as u8,
        hour: (rem / 3600) as u8,
        minute: (rem / 60 % 60) as u8,
        second: (rem % 60) as u8,
    }
}

/// Seconds after 1970-01-01 UTC of a calendar time, or `None` before 1970 or
/// for a field out of range (an RTC that lost power can read anything).
pub fn unix(c: &Civil) -> Option<u64> {
    let (y, m, d) = (c.year as i64, c.month as i64, c.day as i64);
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || c.hour > 23 || c.minute > 59 || c.second > 59 {
        return None;
    }
    // days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400 + c.hour as u64 * 3600 + c.minute as u64 * 60 + c.second as u64)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `YYYY-MM-DD hh:mm:ss`, for the console.
pub fn format(c: &Civil) -> [u8; 19] {
    let mut out = *b"0000-00-00 00:00:00";
    let mut put = |at: usize, v: u32, width: usize| {
        let mut v = v;
        for i in (0..width).rev() {
            out[at + i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
    };
    put(0, c.year as u32, 4);
    put(5, c.month as u32, 2);
    put(8, c.day as u32, 2);
    put(11, c.hour as u32, 2);
    put(14, c.minute as u32, 2);
    put(17, c.second as u32, 2);
    out
}

/// The step from the RTC's time to NTP's, in whole seconds, and whether it is
/// worth a `SetTime`: more than a second either way, or an RTC that reads no
/// valid time at all (`rtc` is `None`).
pub fn step(rtc: Option<u64>, ntp: u64) -> (i64, bool) {
    match rtc {
        Some(r) => {
            let d = ntp as i64 - r as i64;
            (d, d.unsigned_abs() > 1)
        }
        None => (0, true),
    }
}

/// What `ntp =` in `stormboot.conf` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting<'a> {
    /// `ntp = off` (or `no`, `false`, `0`, `none`): leave the clock alone.
    Off,
    /// A host (an IPv4 address or a DNS name) and a port.
    Server(&'a str, u16),
}

/// `host`, `host:port`, or `off`. `None` for a value that is neither, which
/// `clock.rs` reports and treats as absent.
pub fn setting(v: &str) -> Option<Setting<'_>> {
    let v = v.trim();
    if matches!(v, "off" | "no" | "false" | "0" | "none") {
        return Some(Setting::Off);
    }
    let (host, port) = match v.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok().filter(|&p| p != 0)?),
        None => (v, PORT),
    };
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    ok.then_some(Setting::Server(host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply_with(first: u8, stratum: u8, origin: u64, secs: u32, frac: u32) -> [u8; PACKET] {
        let mut r = [0u8; PACKET];
        r[0] = first;
        r[1] = stratum;
        r[24..32].copy_from_slice(&origin.to_be_bytes());
        r[40..44].copy_from_slice(&secs.to_be_bytes());
        r[44..48].copy_from_slice(&frac.to_be_bytes());
        r
    }

    #[test]
    fn the_request_is_a_v4_client_packet() {
        let q = request(0x0102_0304_0506_0708);
        assert_eq!(q[0], 0x23); // LI 0, VN 4, mode 3
        assert!(q[1..40].iter().all(|&b| b == 0));
        assert_eq!(&q[40..], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_good_reply_reads_as_unix_nanoseconds() {
        // 2026-10-01 21:14:03.5 UTC = Unix 1790889243.5.
        let unix_s: u64 = 1_790_889_243;
        let ntp = (unix_s + NTP_TO_UNIX) as u32;
        let r = reply_with(0x24, 2, 77, ntp, 0x8000_0000);
        assert_eq!(reply(&r, 77), Ok(unix_s * 1_000_000_000 + 500_000_000));
        let c = civil(unix_s);
        assert_eq!(&format(&c), b"2026-10-01 21:14:03");
        // LI 1 (a leap second pending) is still synchronised.
        assert!(reply(&reply_with(0x64, 2, 77, ntp, 0), 77).is_ok());
    }

    #[test]
    fn replies_that_must_not_set_a_clock() {
        let ntp = (1_790_889_243 + NTP_TO_UNIX) as u32;
        assert_eq!(reply(&[0x24; 47], 1), Err(Reject::Short));
        assert_eq!(reply(&reply_with(0x23, 2, 1, ntp, 0), 1), Err(Reject::NotAServerReply));
        assert_eq!(reply(&reply_with(0xE4, 2, 1, ntp, 0), 1), Err(Reject::Unsynchronised));
        assert_eq!(reply(&reply_with(0x24, 0, 1, ntp, 0), 1), Err(Reject::KissOfDeath));
        assert_eq!(reply(&reply_with(0x24, 16, 1, ntp, 0), 1), Err(Reject::BadStratum));
        assert_eq!(reply(&reply_with(0x24, 2, 2, ntp, 0), 1), Err(Reject::NotOurs));
        assert_eq!(reply(&reply_with(0x24, 2, 1, 0, 0), 1), Err(Reject::NoTime));
    }

    #[test]
    fn era_one_after_2036() {
        // 2040-01-01 00:00:00 UTC = Unix 2208988800, NTP 4417977600 (wrapped).
        let wrapped = (2_208_988_800u64 + NTP_TO_UNIX - (1 << 32)) as u32;
        assert!(wrapped & 0x8000_0000 == 0);
        let ns = reply(&reply_with(0x24, 1, 9, wrapped, 0), 9).unwrap();
        assert_eq!(ns / 1_000_000_000, 2_208_988_800);
        assert_eq!(&format(&civil(2_208_988_800)), b"2040-01-01 00:00:00");
    }

    #[test]
    fn the_calendar_round_trips() {
        for (s, text) in [
            (0u64, "1970-01-01 00:00:00"),
            (951_782_400, "2000-02-29 00:00:00"),
            (1_767_225_605, "2026-01-01 00:00:05"),
            (1_790_889_243, "2026-10-01 21:14:03"),
            (4_107_542_399, "2100-02-28 23:59:59"),
            (4_107_542_400, "2100-03-01 00:00:00"),
        ] {
            let c = civil(s);
            assert_eq!(core::str::from_utf8(&format(&c)).unwrap(), text);
            assert_eq!(unix(&c), Some(s), "{text}");
        }
        // Every day from 1970 to 2110 round-trips.
        let mut s = 0;
        while s < 4_420_000_000u64 {
            assert_eq!(unix(&civil(s)), Some(s));
            s += 86_400 + 3_661;
        }
    }

    #[test]
    fn an_rtc_that_reads_nonsense_is_no_time() {
        let c = |year, month, day| Civil { year, month, day, hour: 0, minute: 0, second: 0 };
        assert_eq!(unix(&c(2026, 2, 29)), None);
        assert_eq!(unix(&c(2026, 13, 1)), None);
        assert_eq!(unix(&c(2026, 0, 1)), None);
        assert_eq!(unix(&c(1969, 12, 31)), None);
        assert_eq!(unix(&c(2024, 2, 29)).map(|s| s % 86_400), Some(0));
        assert_eq!(unix(&Civil { hour: 24, ..c(2026, 1, 1) }), None);
    }

    #[test]
    fn a_second_either_way_is_left_alone() {
        assert_eq!(step(Some(100), 100), (0, false));
        assert_eq!(step(Some(100), 101), (1, false));
        assert_eq!(step(Some(101), 100), (-1, false));
        assert_eq!(step(Some(100), 102), (2, true));
        assert_eq!(step(Some(1_767_225_605), 1_790_889_243), (23_663_638, true));
        assert_eq!(step(Some(200), 100), (-100, true));
        assert_eq!(step(None, 100), (0, true));
    }

    #[test]
    fn the_ntp_setting() {
        assert_eq!(setting("pool.ntp.org"), Some(Setting::Server("pool.ntp.org", 123)));
        assert_eq!(setting(" 10.0.2.2:41123 "), Some(Setting::Server("10.0.2.2", 41123)));
        assert_eq!(setting("off"), Some(Setting::Off));
        assert_eq!(setting("none"), Some(Setting::Off));
        assert_eq!(setting("host:0"), None);
        assert_eq!(setting("host:x"), None);
        assert_eq!(setting(""), None);
        assert_eq!(setting("a b"), None);
    }
}
