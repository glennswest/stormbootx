//! The machine's DNS name, from the network it booted on (#23).
//!
//! Machines are known to the engine by their DNS name (`server3`,
//! `stormblock1`; stormblock#199), not by an SMBIOS serial that seven
//! MicroCloud nodes share or a service tag nobody wants to type. The name comes
//! from the network the machine is on, in the owner's order:
//!
//! 1. **DHCP option 12** (host name) from the lease on the interface that
//!    reached the engine, with option 15 (domain) for the console line.
//!    microdns sends a reservation's `hostname`.
//! 2. **Reverse DNS**: the PTR of that address, asked of the option 6 DNS
//!    server. **Over TCP**, because TCP4 is the one protocol stack this binary
//!    needs anyway; microdns answers DNS over TCP (checked on g8 and g10,
//!    2026-09-27), and EFI_UDP4 would be another optional stack to refuse.
//!
//! Since #77 there is one more query: the A record of an NTP server named by
//! name (`pool.ntp.org`), for `clock.rs`. That one goes over **UDP**, which
//! smoltcp now carries for SNTP anyway; the PTR stays on TCP, where it was
//! proven against microdns.
//!
//! The engine keys a host on the **first label**, so `server3.g10.lo` claims
//! `boothost/server3`. The full name is printed.
//!
//! This is not the DNS *discovery* that was removed on 2026-09-03: nothing here
//! finds the portal, and a machine on a network that publishes no name boots
//! exactly as before. Every doubt (no option, an invalid label, no DNS server,
//! a DNS error) is "no name from here", never a guess.
//!
//! Same constraints as `intent.rs`, `sha256.rs` and `universal.rs`: `core`
//! only and no `crate::` item, so the tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/dnsname.rs -o t/dnsname-test
//! ```

/// The DHCP magic cookie, in wire order, after the 236-byte BOOTP header.
const MAGIC: [u8; 4] = [99, 130, 83, 99];
const OPTIONS_AT: usize = 236 + 4;

pub const OPT_DNS: u8 = 6;
pub const OPT_HOST_NAME: u8 = 12;
pub const OPT_DOMAIN: u8 = 15;
/// NTP servers (#77): a list of addresses, like option 6.
pub const OPT_NTP: u8 = 42;

/// One option's data from a whole BOOTP/DHCP message (header onwards).
///
/// Options overloaded into `sname`/`file` (option 52) are not read: microdns
/// does not overload, and a name found by reading the boot file field would be
/// a guess.
pub fn dhcp_option(msg: &[u8], code: u8) -> Option<&[u8]> {
    if msg.len() < OPTIONS_AT || msg[236..OPTIONS_AT] != MAGIC {
        return None;
    }
    let mut i = OPTIONS_AT;
    while i < msg.len() {
        match msg[i] {
            0 => i += 1,
            255 => return None,
            c => {
                let len = *msg.get(i + 1)? as usize;
                let data = msg.get(i + 2..i + 2 + len)?;
                if c == code {
                    return Some(data);
                }
                i += 2 + len;
            }
        }
    }
    None
}

/// The first address in option 6 (or in option 42, which has the same shape).
pub fn first_dns(opt: &[u8]) -> Option<[u8; 4]> {
    let a: [u8; 4] = opt.get(..4)?.try_into().ok()?;
    (a != [0; 4] && a != [255; 4]).then_some(a)
}

/// An option's text without the NULs and dots some servers pad it with.
pub fn text(raw: &[u8]) -> Option<&str> {
    let s = core::str::from_utf8(raw).ok()?;
    let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).trim_end_matches('.');
    (!s.is_empty()).then_some(s)
}

/// An RFC 1123 label: 1 to 63 letters, digits and hyphens, not starting or
/// ending with a hyphen.
pub fn valid_label(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 63
        && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !l.starts_with('-')
        && !l.ends_with('-')
}

/// A whole DNS name whose every label is valid.
pub fn valid_name(n: &str) -> bool {
    n.len() <= 253 && n.split('.').all(valid_label)
}

/// The host the engine knows this name by: its first label, when the whole
/// name is valid. `server3.g10.lo` → `server3`.
pub fn host_label(name: &str) -> Option<&str> {
    let name = name.trim_end_matches('.');
    if !valid_name(name) {
        return None;
    }
    name.split('.').next()
}

/// The machine a NIC's name belongs to: `server1a` → `server1`.
///
/// A reservation names the **NIC**, and a machine with two ports has two:
/// `server1a` and `server1b`, `gb10b` beside a machine called `gb10`. The
/// machine is the name without that suffix (owner, #26). The rule is exactly
/// that shape: a single lowercase letter after a digit, at the end of the
/// label. A label with no digit before its last letter (`forge`, `dev`) and
/// one that ends in a digit (`stormblock1`) is already a machine, and comes
/// back unchanged.
pub fn machine_label(host: &str) -> &str {
    let b = host.as_bytes();
    match b {
        [.., d, l] if b.len() >= 3 && d.is_ascii_digit() && l.is_ascii_lowercase() => {
            &host[..host.len() - 1]
        }
        _ => host,
    }
}

/// A PTR query for `addr`, framed for DNS over TCP (two-byte length first).
/// Returns the bytes written into `out`.
pub fn ptr_query(addr: [u8; 4], id: u16, out: &mut [u8; 64]) -> usize {
    let mut n = 2;
    let mut put = |b: &[u8], n: &mut usize| {
        out[*n..*n + b.len()].copy_from_slice(b);
        *n += b.len();
    };
    // id, flags = RD, one question, no other records.
    put(&id.to_be_bytes(), &mut n);
    put(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0], &mut n);
    for octet in addr.iter().rev() {
        let mut digits = [0u8; 3];
        let mut len = 0;
        let mut v = *octet;
        loop {
            digits[len] = b'0' + v % 10;
            len += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        digits[..len].reverse();
        put(&[len as u8], &mut n);
        put(&digits[..len], &mut n);
    }
    put(b"\x07in-addr\x04arpa\x00", &mut n);
    put(&[0, 12, 0, 1], &mut n); // PTR, IN
    let body = (n - 2) as u16;
    out[..2].copy_from_slice(&body.to_be_bytes());
    n
}

/// The name in the first PTR answer of a DNS reply (without the TCP length
/// prefix), written dotted into `out`. `None` for another id, an error rcode,
/// no PTR answer, or anything malformed.
pub fn ptr_answer(msg: &[u8], id: u16, out: &mut [u8; 256]) -> Option<usize> {
    if msg.len() < 12 || msg[..2] != id.to_be_bytes() {
        return None;
    }
    let is_reply = msg[2] & 0x80 != 0;
    let rcode = msg[3] & 0x0f;
    if !is_reply || rcode != 0 {
        return None;
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]);
    let an = u16::from_be_bytes([msg[6], msg[7]]);
    let mut at = 12;
    for _ in 0..qd {
        at = skip_name(msg, at)? + 4;
    }
    for _ in 0..an {
        at = skip_name(msg, at)?;
        let rr = msg.get(at..at + 10)?;
        let kind = u16::from_be_bytes([rr[0], rr[1]]);
        let rdlen = u16::from_be_bytes([rr[8], rr[9]]) as usize;
        let rdata = at + 10;
        if rdata + rdlen > msg.len() {
            return None;
        }
        if kind == 12 {
            return read_name(msg, rdata, out);
        }
        at = rdata + rdlen;
    }
    None
}

/// An A query for `name`, unframed (for UDP, #77). Returns the bytes written
/// into `out`, or `None` for a name that is not a valid DNS name.
pub fn a_query(name: &str, id: u16, out: &mut [u8; 300]) -> Option<usize> {
    let name = name.trim_end_matches('.');
    if !valid_name(name) {
        return None;
    }
    out[..2].copy_from_slice(&id.to_be_bytes());
    // flags = RD, one question, no other records.
    out[2..12].copy_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    let mut n = 12;
    for label in name.split('.') {
        out[n] = label.len() as u8;
        out[n + 1..n + 1 + label.len()].copy_from_slice(label.as_bytes());
        n += 1 + label.len();
    }
    out[n..n + 5].copy_from_slice(&[0, 0, 1, 0, 1]); // root, A, IN
    Some(n + 5)
}

/// The address in the first A answer of a reply to query `id`, past any
/// CNAMEs (`pool.ntp.org` answers with several A records; the first is
/// taken). `None` for another id, an error rcode or no A record.
pub fn a_answer(msg: &[u8], id: u16) -> Option<[u8; 4]> {
    if msg.len() < 12 || msg[..2] != id.to_be_bytes() || msg[2] & 0x80 == 0 || msg[3] & 0x0f != 0 {
        return None;
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]);
    let an = u16::from_be_bytes([msg[6], msg[7]]);
    let mut at = 12;
    for _ in 0..qd {
        at = skip_name(msg, at)? + 4;
    }
    for _ in 0..an {
        at = skip_name(msg, at)?;
        let rr = msg.get(at..at + 10)?;
        let kind = u16::from_be_bytes([rr[0], rr[1]]);
        let class = u16::from_be_bytes([rr[2], rr[3]]);
        let rdlen = u16::from_be_bytes([rr[8], rr[9]]) as usize;
        let rdata = msg.get(at + 10..at + 10 + rdlen)?;
        if kind == 1 && class == 1 && rdlen == 4 {
            let a: [u8; 4] = rdata.try_into().ok()?;
            return (a != [0; 4]).then_some(a);
        }
        at += 10 + rdlen;
    }
    None
}

/// Past a (possibly compressed) name.
fn skip_name(msg: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *msg.get(at)?;
        match len {
            0 => return Some(at + 1),
            l if l & 0xc0 == 0xc0 => return Some(at + 2),
            l => at += 1 + l as usize,
        }
    }
}

/// A name, following compression pointers (a bounded number of them).
fn read_name(msg: &[u8], mut at: usize, out: &mut [u8; 256]) -> Option<usize> {
    let mut n = 0;
    let mut hops = 0;
    loop {
        let len = *msg.get(at)? as usize;
        if len == 0 {
            return (n > 0).then_some(n);
        }
        if len & 0xc0 == 0xc0 {
            hops += 1;
            if hops > 16 {
                return None;
            }
            at = ((len & 0x3f) << 8) | *msg.get(at + 1)? as usize;
            continue;
        }
        if len > 63 {
            return None;
        }
        let label = msg.get(at + 1..at + 1 + len)?;
        let dot = usize::from(n > 0);
        if n + dot + len > out.len() {
            return None;
        }
        if dot == 1 {
            out[n] = b'.';
        }
        out[n + dot..n + dot + len].copy_from_slice(label);
        n += dot + len;
        at += 1 + len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> std::vec::Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// A DHCP ACK with pad, a subnet mask, options 6, 12 and 15, and End.
    fn ack(opts: &[u8]) -> std::vec::Vec<u8> {
        let mut m = std::vec![0u8; 236];
        m[0] = 2;
        m.extend_from_slice(&MAGIC);
        m.extend_from_slice(opts);
        m
    }

    #[test]
    fn options_are_found_whatever_their_order() {
        let m = ack(&[
            0, 53, 1, 5, 1, 4, 255, 255, 255, 0, 6, 8, 192, 168, 10, 252, 192, 168, 1, 252, 12,
            7, b's', b'e', b'r', b'v', b'e', b'r', b'3', 15, 7, b'g', b'1', b'0', b'.', b'l',
            b'o', 0, 255,
        ]);
        assert_eq!(text(dhcp_option(&m, OPT_HOST_NAME).unwrap()), Some("server3"));
        assert_eq!(text(dhcp_option(&m, OPT_DOMAIN).unwrap()), Some("g10.lo"));
        assert_eq!(first_dns(dhcp_option(&m, OPT_DNS).unwrap()), Some([192, 168, 10, 252]));
        assert_eq!(dhcp_option(&m, 66), None);
    }

    #[test]
    fn malformed_dhcp_reads_as_nothing() {
        assert_eq!(dhcp_option(&[0u8; 100], OPT_HOST_NAME), None);
        let mut bad_magic = ack(&[12, 1, b'a', 255]);
        bad_magic[236] = 0;
        assert_eq!(dhcp_option(&bad_magic, OPT_HOST_NAME), None);
        // An option whose length runs past the end.
        assert_eq!(dhcp_option(&ack(&[12, 40, b'a']), OPT_HOST_NAME), None);
        // Past End is not read.
        assert_eq!(dhcp_option(&ack(&[255, 12, 1, b'a']), OPT_HOST_NAME), None);
        assert_eq!(first_dns(&[0, 0, 0, 0]), None);
    }

    #[test]
    fn only_valid_names_become_hosts() {
        assert_eq!(host_label("server3"), Some("server3"));
        assert_eq!(host_label("server3.g10.lo"), Some("server3"));
        assert_eq!(host_label("dev.g8.lo."), Some("dev"));
        for bad in ["", "-x", "x-", "a b", "a_b", "a..b", "ünï", "a/b"] {
            assert_eq!(host_label(bad), None, "{bad:?}");
        }
        assert_eq!(text(b"server3\0\0"), Some("server3"));
        assert_eq!(text(b"\0"), None);
    }

    #[test]
    fn ptr_query_matches_the_wire() {
        // Byte for byte what was sent to microdns on 2026-09-27 (id 0x5342),
        // behind the TCP length prefix.
        let mut q = [0u8; 64];
        let n = ptr_query([192, 168, 10, 31], 0x5342, &mut q);
        let want = hex("534201000001000000000000023331023130033136380331393207696e2d61646472046172706100000c0001");
        assert_eq!(&q[2..n], &want[..]);
        assert_eq!(u16::from_be_bytes([q[0], q[1]]) as usize, want.len());
        let n = ptr_query([192, 168, 8, 150], 0x5342, &mut q);
        assert_eq!(&q[2..n], &hex("534201000001000000000000033135300138033136380331393207696e2d61646472046172706100000c0001")[..]);
    }

    #[test]
    fn ptr_answers_from_microdns() {
        let mut out = [0u8; 256];
        // One answer.
        let r = hex("534285800001000100000000023331023130033136380331393207696e2d61646472046172706100000c0001c00c000c00010000012c000d046762313003673130026c6f00");
        let n = ptr_answer(&r, 0x5342, &mut out).unwrap();
        assert_eq!(&out[..n], b"gb10.g10.lo");
        // Two answers, the second a compression pointer into the first.
        let r = hex("534285800001000200000000033135300138033136380331393207696e2d61646472046172706100000c0001c00c000c00010000012c000b03646576026738026c6f00c00c000c00010000012c0002c038");
        let n = ptr_answer(&r, 0x5342, &mut out).unwrap();
        assert_eq!(&out[..n], b"dev.g8.lo");
        // NXDOMAIN with an SOA in authority.
        let r = hex("53428583000100000001000003323439023130033136380331393207696e2d61646472046172706100000c0001c010000600010000012c0022036e7331c0100561646d696ec01078c3919b00000e100000038400093a800000012c");
        assert_eq!(ptr_answer(&r, 0x5342, &mut out), None);
    }

    #[test]
    fn ptr_answers_that_are_not_ours_or_loop() {
        let mut out = [0u8; 256];
        let r = hex("534285800001000100000000023331023130033136380331393207696e2d61646472046172706100000c0001c00c000c00010000012c000d046762313003673130026c6f00");
        assert_eq!(ptr_answer(&r, 0x1111, &mut out), None);
        assert_eq!(ptr_answer(&r[..40], 0x5342, &mut out), None);
        // A pointer to itself.
        let mut looped = r.clone();
        let rdata = looped.len() - 13;
        looped[rdata] = 0xc0;
        looped[rdata + 1] = rdata as u8;
        assert_eq!(ptr_answer(&looped, 0x5342, &mut out), None);
    }

    #[test]
    fn a_nic_suffix_is_not_the_machine() {
        assert_eq!(machine_label("server1a"), "server1");
        assert_eq!(machine_label("server1b"), "server1");
        assert_eq!(machine_label("gb10b"), "gb10");
        assert_eq!(machine_label("server12a"), "server12");
        // Already a machine.
        assert_eq!(machine_label("stormblock1"), "stormblock1");
        assert_eq!(machine_label("server1"), "server1");
        assert_eq!(machine_label("forge"), "forge");
        assert_eq!(machine_label("dev"), "dev");
        // Too short to be a name plus a suffix, and not lowercase.
        assert_eq!(machine_label("1a"), "1a");
        assert_eq!(machine_label("server1A"), "server1A");
    }

    #[test]
    fn a_query_and_answer() {
        let mut q = [0u8; 300];
        let n = a_query("pool.ntp.org", 0x1234, &mut q).unwrap();
        assert_eq!(&q[..n], &hex("12340100000100000000000004706f6f6c036e7470036f72670000010001")[..]);
        assert_eq!(a_query("bad name", 1, &mut q), None);
        // A CNAME, then two A records, the shape a resolver gives for the pool.
        let r = hex(concat!(
            "123481800001000300000000",
            "04706f6f6c036e7470036f72670000010001",
            "c00c0005000100000e10000704706f6f6cc00c",
            "c02a00010001000000960004a29fc801",
            "c02a00010001000000960004c0a80101",
        ));
        assert_eq!(a_answer(&r, 0x1234), Some([162, 159, 200, 1]));
        assert_eq!(a_answer(&r, 0x1235), None);
        assert_eq!(a_answer(&r[..60], 0x1234), None);
        // NXDOMAIN.
        let mut nx = r.clone();
        nx[3] = 0x83;
        assert_eq!(a_answer(&nx, 0x1234), None);
    }
}
