//! The log's wire format (#162): stormcast's frames, built here.
//!
//! The fleet's log stream is one framing on one multicast group: RFC 5424
//! over UDP, facility `local0`, to `239.255.42.1:5514`
//! (glennswest/stormcast, `docs/wire.md`). stormpump and stormd emit it
//! through the stormcast crate; stormcentral's receiver (`[syslog]`, UDP
//! 5514) and mcastsyslog read it. stormcast is `std` (it owns a socket), so
//! this `no_std` binary cannot link it. These are its rules, byte for byte,
//! and the tests below are its wire document's examples:
//!
//! ```text
//! <PRI>1 TIMESTAMP HOSTNAME APP-NAME - - - MSG
//! ```
//!
//! - PRI is `16 * 8 + severity`; the severity is guessed from the line as
//!   stormcast's `Severity::of` does (klog's letter, else the leftmost level
//!   word in the first 120 bytes, ties to the more severe, else info).
//! - HOSTNAME (255 bytes) and APP-NAME (48) are one token of printable ASCII:
//!   anything else is `_`, empty is `-`. TIMESTAMP is `-` unless it is one.
//! - MSG has ANSI CSI/OSC sequences stripped and is cut on a character
//!   boundary. stormcast cuts at 8192 bytes; this cuts the whole frame to
//!   [`MAX_FRAME`], because smoltcp here sends no IP fragments.
//!
//! Also the two settings that name a unicast target: `syslog =` in
//! `stormboot.conf` and `syslog:` in `install-config.yaml` ([`target`],
//! [`yaml_syslog`]).
//!
//! Same constraints as `sha256.rs` and the other core-only modules: `core`
//! and `alloc` only and no `crate::` item, so the tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/logwire.rs -o t/logwire-test && ./t/logwire-test
//! ```

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The group every storm node emits to (stormcast's `DEFAULT_GROUP`).
pub const GROUP: [u8; 4] = [239, 255, 42, 1];
pub const GROUP_PORT: u16 = 5514;

/// IPv4 multicast TTL, as stormcast sets it: far enough to reach a watcher
/// on another subnet of the site, not the estate.
pub const GROUP_TTL: u8 = 4;

/// A bare address in a setting means the syslog port (stormpump's rule).
pub const DEFAULT_PORT: u16 = 514;

/// The facility the whole fleet uses.
pub const FACILITY_LOCAL0: u8 = 16;

/// The longest frame put on the wire: one 1500-byte Ethernet frame less the
/// IP and UDP headers.
pub const MAX_FRAME: usize = 1472;

/// HOSTNAME and APP-NAME limits (RFC 5424, stormcast).
pub const MAX_HOSTNAME: usize = 255;
pub const MAX_APP_NAME: usize = 48;

/// Syslog severities: 3 error, 4 warning, 6 info, …
pub const CRITICAL: u8 = 2;
pub const ERROR: u8 = 3;
pub const WARNING: u8 = 4;
pub const INFO: u8 = 6;
pub const DEBUG: u8 = 7;

/// The severity a line is written at, as stormcast's `Severity::of` reads
/// it.
pub fn severity_of(line: &str) -> u8 {
    let mut n = 120.min(line.len());
    while !line.is_char_boundary(n) {
        n -= 1;
    }
    let head = &line[..n];
    let b = head.as_bytes();
    if b.len() > 5 && b[1..5].iter().all(|c| c.is_ascii_digit()) {
        match b[0] {
            b'F' => return CRITICAL,
            b'E' => return ERROR,
            b'W' => return WARNING,
            b'I' => return INFO,
            b'D' => return DEBUG,
            _ => {}
        }
    }
    let upper = head.to_ascii_uppercase();
    let mut best: Option<(usize, u8)> = None;
    for (needle, sev) in [
        ("PANIC", CRITICAL),
        ("FATAL", CRITICAL),
        ("ERROR", ERROR),
        ("WARN", WARNING),
        ("DEBUG", DEBUG),
        ("TRACE", DEBUG),
        ("INFO", INFO),
    ] {
        if let Some(at) = upper.find(needle) {
            match best {
                Some((prev, _)) if prev <= at => {}
                _ => best = Some((at, sev)),
            }
        }
    }
    best.map(|(_, s)| s).unwrap_or(INFO)
}

/// A header field as one token: printable ASCII 33..=126 (else `_`, one per
/// character), `-` when empty, at most `max` bytes.
pub fn header_token(v: &str, max: usize) -> String {
    if v.is_empty() {
        return String::from("-");
    }
    v.chars().map(|c| if (33..=126).contains(&(c as u32)) { c } else { '_' }).take(max).collect()
}

/// A timestamp as sent: itself when it is one token, else `-`.
fn header_time(ts: &str) -> &str {
    if !ts.is_empty() && ts.bytes().all(|b| (33..=126).contains(&b)) {
        ts
    } else {
        "-"
    }
}

/// `s` without ANSI CSI and OSC sequences (stormcast's `strip_ansi`).
pub fn strip_ansi(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b {
            i += 1;
            match b.get(i) {
                Some(b'[') => {
                    i += 1;
                    while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                Some(b']') => {
                    i += 1;
                    while i < b.len() && b[i] != 0x07 {
                        if b[i] == 0x1b && b.get(i + 1) == Some(&b'\\') {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                Some(_) => i += 1,
                None => break,
            }
        } else {
            let n = s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            out.push_str(&s[i..i + n]);
            i += n;
        }
    }
    out
}

/// One frame: `<PRI>1 TS HOST TAG - - - MSG`, at most [`MAX_FRAME`] bytes,
/// the message cut on a character boundary.
pub fn frame(ts: &str, host: &str, tag: &str, msg: &str) -> Vec<u8> {
    let msg = strip_ansi(msg);
    let pri = FACILITY_LOCAL0 * 8 + severity_of(&msg);
    let head = format!(
        "<{pri}>1 {} {} {} - - - ",
        header_time(ts),
        header_token(host, MAX_HOSTNAME),
        header_token(tag, MAX_APP_NAME)
    );
    let mut room = MAX_FRAME.saturating_sub(head.len()).min(msg.len());
    while !msg.is_char_boundary(room) {
        room -= 1;
    }
    let mut out = Vec::with_capacity(head.len() + room);
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(&msg.as_bytes()[..room]);
    out
}

/// RFC 3339 UTC with microseconds, from the date's fields:
/// `2026-10-21T18:31:25.000000Z`.
pub fn rfc3339(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8, micros: u32) -> String {
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{micros:06}Z")
}

/// Where a setting sends the log.
#[derive(Debug, PartialEq, Eq)]
pub enum Target<'a> {
    /// `off`: nothing goes on the network, not even the group.
    Off,
    /// A host (a name or an address) and a port, beside the group.
    To(&'a str, u16),
}

/// `syslog =` / `syslog:`: `host:port`, a bare host (port 514, as
/// stormpump's `stormpump.syslog=`), or `off`. `None` for a value that is
/// none of those.
pub fn target(v: &str) -> Option<Target<'_>> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("off") {
        return Some(Target::Off);
    }
    let (host, port) = match v.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok().filter(|&p| p > 0)?),
        None => (v, DEFAULT_PORT),
    };
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_');
    ok.then_some(Target::To(host, port))
}

/// The value of a top-level `syslog:` key in install-config.yaml, quotes
/// taken off and a trailing comment dropped. Only a key at column 0: a
/// `syslog:` under a host or an option is not the install's.
pub fn yaml_syslog(text: &str) -> Option<&str> {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("syslog:") else { continue };
        let mut v = rest.trim();
        if let Some(i) = v.find(" #") {
            v = v[..i].trim_end();
        }
        let quoted = |q: char| v.len() >= 2 && v.starts_with(q) && v.ends_with(q);
        if quoted('"') || quoted('\'') {
            v = &v[1..v.len() - 1];
        }
        return (!v.is_empty()).then_some(v);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(f: &[u8]) -> &str {
        core::str::from_utf8(f).unwrap()
    }

    #[test]
    fn the_wire_documents_examples() {
        // stormcast docs/wire.md.
        assert_eq!(text(&frame("", "", "", "m")), "<134>1 - - - - - - m");
        assert_eq!(
            text(&frame("2026-09-20T00:00:00.000000Z", "storm-1", "kube proxy", "up")),
            "<134>1 2026-09-20T00:00:00.000000Z storm-1 kube_proxy - - - up"
        );
        assert_eq!(header_token("h\té", MAX_HOSTNAME), "h__");
        assert_eq!(text(&frame("2026-09-20 00:00:00", "h", "t", "x")), "<134>1 - h t - - - x");
    }

    #[test]
    fn severity_as_stormcast_reads_it() {
        assert_eq!(severity_of("E0825 14:52:15.123456 boom"), ERROR);
        assert_eq!(severity_of("W0825 x"), WARNING);
        assert_eq!(severity_of("WARN leaderelection: GET lease failed: error sending request"), WARNING);
        assert_eq!(severity_of("level=error msg=x"), ERROR);
        assert_eq!(severity_of("FATAL: no root"), CRITICAL);
        assert_eq!(severity_of("claim       : boothost/server1 at 192.168.31.202:9090"), INFO);
        // Past 120 bytes nothing is read, and a cut never splits a character.
        let long = format!("{}ERROR", "—".repeat(50));
        assert_eq!(severity_of(&long), INFO);
        assert!(text(&frame("-", "h", "t", "an ERROR here")).starts_with("<131>1 - h t "));
    }

    #[test]
    fn escapes_are_stripped() {
        assert_eq!(strip_ansi("\x1b[1;31mred\x1b[0m and \x1b]0;title\x07done"), "red and done");
        assert_eq!(strip_ansi("a\x1b]2;t\x1b\\b"), "ab");
        assert_eq!(strip_ansi("trailing \x1b"), "trailing ");
    }

    #[test]
    fn a_frame_fits_one_ethernet_frame() {
        let msg = "—".repeat(1000); // 3000 bytes
        let f = frame("-", "server1", "stormbootx", &msg);
        assert!(f.len() <= MAX_FRAME, "{}", f.len());
        assert!(f.len() > MAX_FRAME - 3);
        assert!(core::str::from_utf8(&f).is_ok());
        let short = frame("-", "server1", "stormbootx", "nic 0: leased 10.0.2.15/24");
        assert_eq!(text(&short), "<134>1 - server1 stormbootx - - - nic 0: leased 10.0.2.15/24");
    }

    #[test]
    fn timestamps() {
        assert_eq!(rfc3339(2026, 10, 21, 18, 31, 5, 42), "2026-10-21T18:31:05.000042Z");
    }

    #[test]
    fn targets() {
        assert_eq!(target("off"), Some(Target::Off));
        assert_eq!(target(" OFF "), Some(Target::Off));
        assert_eq!(target("stormcentral.g8.lo:5514"), Some(Target::To("stormcentral.g8.lo", 5514)));
        assert_eq!(target("192.168.8.10"), Some(Target::To("192.168.8.10", 514)));
        assert_eq!(target("host:0"), None);
        assert_eq!(target("host:x"), None);
        assert_eq!(target(":5514"), None);
        assert_eq!(target("two words"), None);
        assert_eq!(target(""), None);
    }

    #[test]
    fn install_config_key() {
        let y = "cluster: c1\nsyslog: \"stormcentral.g8.lo:5514\"  # the collector\nhosts:\n  - name: a\n    syslog: x\n";
        assert_eq!(yaml_syslog(y), Some("stormcentral.g8.lo:5514"));
        assert_eq!(yaml_syslog("hosts:\n  syslog: x\n"), None);
        assert_eq!(yaml_syslog("syslog: 10.0.0.1\r\n"), Some("10.0.0.1"));
        assert_eq!(yaml_syslog("syslog: 'off'"), Some("off"));
        assert_eq!(yaml_syslog("syslog:\n"), None);
    }
}
