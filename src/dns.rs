//! Finding the portal by asking, instead of being told.
//!
//! `\stormboot\stormboot.conf` beats rebuilding a binary, but it is still
//! per-stick configuration: a machine carried to another network needs its
//! stick edited before it can boot. Over one afternoon the target here moved
//! host and then changed NQN, and both would have been zero-touch with
//! discovery.
//!
//! The service name is the one from NVMe-oF TP8009's DNS-SD binding:
//!
//! ```text
//!   SRV  _nvme-disc._tcp.<zone>  ->  host:port
//!   TXT  _nvme-disc._tcp.<zone>  ->  nqn=…  nsid=…
//! ```
//!
//! It is the standard name even though stormblock exposes no discovery
//! controller, which is why the TXT record carries the subsystem NQN directly
//! rather than pointing at `nqn.2014-08.org.nvmexpress.discovery`.
//!
//! ## The zone is a constant, and that is the point
//!
//! The obvious design asks DHCP for option 15 and queries
//! `_nvme-disc._tcp.<that domain>`, which means reaching into `EFI_DHCP4` for
//! the reply packet on a boot path that cannot be tested under OVMF.
//!
//! It is also unnecessary. The **DNS server** already comes from DHCP and is
//! already per-network — microdns runs one per network — so a fixed service
//! zone answered differently by each network's resolver gives exactly the
//! property that was wanted: a machine on g16 asks g16's resolver and gets
//! g16's portal; move it and the same question gets the other answer. Nothing
//! on the machine changes. One `_nvme-disc._tcp.storm.lo` record per network
//! zone is the whole configuration.
//!
//! ## What DNS is not allowed to say
//!
//! **Where, never what version.** A version in DNS makes TTLs the rollout
//! control and every bump a zone edit, with no object recording what was
//! intended. Version intent belongs in the BootHost object; see #4.
//!
//! ## Transport
//!
//! DNS over TCP (RFC 7766), which reuses `tcp4.rs` and needs no new EFI
//! protocol — `EFI_UDP4` would be a second dependency for no benefit, and the
//! answers here are small enough that TCP's extra round trip is cheaper than
//! carrying a truncation-and-retry path.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use uefi::boot::{self, SearchType};
use uefi::{Guid, Status, guid};
use uefi_raw::protocol::network::ip4_config2::Ip4Config2DataType;

use crate::tcp4::{handle_protocol, Tcp4Socket};

/// The DNS-SD service name for an NVMe-oF portal (TP8009).
pub const SERVICE: &str = "_nvme-disc._tcp";

/// EFI_IP4_CONFIG2_PROTOCOL, where the firmware keeps what DHCP told it.
const IP4_CONFIG2: Guid = guid!("5b446ed1-e30b-4faa-871a-3654eca36080");

/// Seconds to spend on any one resolver before moving on. Discovery is an
/// optimisation over the compiled defaults; it must not become the slow path.
const RESOLVER_TIMEOUT_SECS: u32 = 5;

const TYPE_A: u16 = 1;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;
const CLASS_IN: u16 = 1;

/// A portal, as DNS described it.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub portal: [u8; 4],
    pub port: u16,
    /// From the TXT record. `None` means DNS said where but not what, and the
    /// caller should keep whatever NQN it already had.
    pub nqn: Option<String>,
    pub nsid: Option<u32>,
    /// The resolver that answered, for the console line.
    pub resolver: [u8; 4],
}

/// The DNS servers the firmware's stack holds, in preference order.
///
/// `EFI_IP4_CONFIG2` is where the IP4 driver records what DHCP handed it, so
/// this needs no DHCP client of its own and no reply-packet parsing. Every
/// interface is asked, because the one that has a lease is not necessarily the
/// first handle enumerated.
pub fn resolvers() -> Vec<[u8; 4]> {
    let mut out: Vec<[u8; 4]> = Vec::new();
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&IP4_CONFIG2)) else {
        return out;
    };

    for h in handles.iter() {
        let Some(p) = handle_protocol(h.as_ptr(), &IP4_CONFIG2) else {
            continue;
        };
        let cfg = p as *mut uefi_raw::protocol::network::ip4_config2::Ip4Config2Protocol;

        // The two-call size dance: ask with a zero-length buffer, get
        // BUFFER_TOO_SMALL and the size, then ask again.
        let mut size: usize = 0;
        let st = unsafe {
            ((*cfg).get_data)(
                cfg,
                Ip4Config2DataType::DNS_SERVER,
                &mut size,
                core::ptr::null_mut(),
            )
        };
        if st != Status::BUFFER_TOO_SMALL || size == 0 || size > 4096 {
            continue;
        }
        let mut buf = vec![0u8; size];
        let st = unsafe {
            ((*cfg).get_data)(
                cfg,
                Ip4Config2DataType::DNS_SERVER,
                &mut size,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
            )
        };
        if st != Status::SUCCESS {
            continue;
        }
        // An array of EFI_IPv4_ADDRESS, which is four bytes with no padding.
        for a in buf.chunks_exact(4) {
            let ip = [a[0], a[1], a[2], a[3]];
            // 0.0.0.0 is an empty slot, not a resolver.
            if ip != [0, 0, 0, 0] && !out.contains(&ip) {
                out.push(ip);
            }
        }
    }
    out
}

/// Ask every resolver, in order, for the portal serving `zone`.
///
/// Returns `None` rather than an error: not finding a record is the ordinary
/// case on a network that has not published one, and the caller falls back to
/// its configured target. `note` receives one line per resolver so a failed
/// discovery is visible on the console without being fatal.
pub fn discover(zone: &str, note: &mut dyn FnMut(&str)) -> Option<Discovered> {
    let name = format!("{SERVICE}.{zone}");
    for resolver in resolvers() {
        let [a, b, c, d] = resolver;
        match query(resolver, &name) {
            Ok(found) => {
                note(&format!("  {a}.{b}.{c}.{d} answered for {name}"));
                return Some(found);
            }
            Err(e) => note(&format!("  {a}.{b}.{c}.{d}: {e}")),
        }
    }
    None
}

/// One resolver, one connection, three questions.
///
/// SRV, TXT and the target's A record go down the same TCP connection —
/// RFC 7766 exists for exactly this, and three connects to the same resolver
/// would triple the cost of the one thing on the boot path that is allowed to
/// fail.
fn query(resolver: [u8; 4], name: &str) -> Result<Discovered, String> {
    let mut sock = Tcp4Socket::connect_within(resolver, 53, RESOLVER_TIMEOUT_SECS)?;

    let srv = exchange(&mut sock, name, TYPE_SRV, 1)?;
    let (target, port) = parse_srv(&srv).ok_or("no SRV record")?;

    // Prefer the address the resolver already volunteered: a microdns SRV
    // answer normally carries the target's A record in the additional section,
    // and a round trip saved on the boot path is worth the extra parsing.
    let portal = match find_a(&srv, &target) {
        Some(ip) => ip,
        None => {
            let a = exchange(&mut sock, &target, TYPE_A, 2)?;
            find_a(&a, &target).ok_or_else(|| format!("SRV target {target} has no A record"))?
        }
    };

    // TXT is optional by design: DNS answering "where" is the whole
    // requirement, and a zone that does not name an NQN leaves the caller on
    // the one it already had rather than failing to boot.
    let (nqn, nsid) = match exchange(&mut sock, name, TYPE_TXT, 3) {
        Ok(txt) => parse_txt(&txt),
        Err(_) => (None, None),
    };

    Ok(Discovered {
        portal,
        port,
        nqn,
        nsid,
        resolver,
    })
}

/// Send one question and read one answer, framed as RFC 7766 requires.
fn exchange(sock: &mut Tcp4Socket, name: &str, qtype: u16, id: u16) -> Result<Vec<u8>, String> {
    let msg = build_query(name, qtype, id);
    let mut framed = Vec::with_capacity(msg.len() + 2);
    framed.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    framed.extend_from_slice(&msg);
    sock.send(&framed)?;

    let len = sock.read_exact(2)?;
    let len = u16::from_be_bytes([len[0], len[1]]) as usize;
    if len < 12 || len > 16 * 1024 {
        return Err(format!("implausible DNS response length {len}"));
    }
    let resp = sock.read_exact(len)?;

    if u16::from_be_bytes([resp[0], resp[1]]) != id {
        // On a TCP connection to a resolver we chose there is no off-path
        // attacker to worry about, so this is a framing check rather than a
        // security one — but a mismatched id means the stream is out of step
        // and every later answer on it would be wrong.
        return Err("DNS response id does not match the question".to_string());
    }
    let rcode = resp[3] & 0x0F;
    if rcode != 0 {
        return Err(format!("DNS rcode {rcode}"));
    }
    Ok(resp)
}

fn build_query(name: &str, qtype: u16, id: u16) -> Vec<u8> {
    let mut m = Vec::with_capacity(name.len() + 20);
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    m.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    m.extend_from_slice(&[0; 6]); // an/ns/ar counts
    for label in name.split('.').filter(|l| !l.is_empty()) {
        let b = label.as_bytes();
        m.push(b.len().min(63) as u8);
        m.extend_from_slice(&b[..b.len().min(63)]);
    }
    m.push(0);
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&CLASS_IN.to_be_bytes());
    m
}

/// Read a name at `at`, following compression pointers.
///
/// Returns the name and the offset just past it *in the wire*, which for a
/// pointer is two bytes regardless of how long the name it expands to is.
fn read_name(msg: &[u8], at: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut p = at;
    let mut after = None;
    // Bounded: a compression pointer that points at itself is a hang, and this
    // parser runs before anything has booted.
    for _ in 0..128 {
        let len = *msg.get(p)? as usize;
        if len == 0 {
            p += 1;
            return Some((out, after.unwrap_or(p)));
        }
        if len & 0xC0 == 0xC0 {
            let hi = (len & 0x3F) << 8;
            let lo = *msg.get(p + 1)? as usize;
            after.get_or_insert(p + 2);
            p = hi | lo;
            continue;
        }
        let end = p + 1 + len;
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(core::str::from_utf8(msg.get(p + 1..end)?).ok()?);
        p = end;
    }
    None
}

/// One resource record, as far as this needs to care.
struct Record {
    name: String,
    rtype: u16,
    /// Offsets into the message; the rdata of an SRV holds a compressed name
    /// that can only be expanded against the whole message.
    rdata: core::ops::Range<usize>,
}

/// Walk every answer, authority and additional record.
///
/// All three sections are read: the A record for an SRV target arrives in
/// additionals, and taking it from there is what removes a round trip.
fn records(msg: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    if msg.len() < 12 {
        return out;
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let total = (6..12)
        .step_by(2)
        .map(|i| u16::from_be_bytes([msg[i], msg[i + 1]]) as usize)
        .sum::<usize>();

    let mut p = 12;
    for _ in 0..qd {
        let Some((_, next)) = read_name(msg, p) else {
            return out;
        };
        p = next + 4; // qtype + qclass
    }
    for _ in 0..total {
        let Some((name, next)) = read_name(msg, p) else {
            return out;
        };
        p = next;
        if p + 10 > msg.len() {
            return out;
        }
        let rtype = u16::from_be_bytes([msg[p], msg[p + 1]]);
        let rdlen = u16::from_be_bytes([msg[p + 8], msg[p + 9]]) as usize;
        p += 10;
        if p + rdlen > msg.len() {
            return out;
        }
        out.push(Record {
            name,
            rtype,
            rdata: p..p + rdlen,
        });
        p += rdlen;
    }
    out
}

/// The best SRV in the message: lowest priority, and among those the highest
/// weight. Weighted random selection is what the RFC asks for and this has no
/// randomness to do it with, so the heaviest wins — deterministic, and a fleet
/// that all picks the same portal is the intended behaviour here anyway.
fn parse_srv(msg: &[u8]) -> Option<(String, u16)> {
    let mut best: Option<(u16, u16, String, u16)> = None;
    for r in records(msg) {
        if r.rtype != TYPE_SRV || r.rdata.len() < 7 {
            continue;
        }
        let d = &msg[r.rdata.start..r.rdata.end];
        let priority = u16::from_be_bytes([d[0], d[1]]);
        let weight = u16::from_be_bytes([d[2], d[3]]);
        let port = u16::from_be_bytes([d[4], d[5]]);
        let (target, _) = read_name(msg, r.rdata.start + 6)?;
        if target.is_empty() {
            continue; // "." means the service is explicitly not offered here
        }
        let better = match &best {
            None => true,
            Some((bp, bw, _, _)) => priority < *bp || (priority == *bp && weight > *bw),
        };
        if better {
            best = Some((priority, weight, target, port));
        }
    }
    best.map(|(_, _, target, port)| (target, port))
}

fn find_a(msg: &[u8], name: &str) -> Option<[u8; 4]> {
    records(msg).iter().find_map(|r| {
        (r.rtype == TYPE_A && r.rdata.len() == 4 && r.name.eq_ignore_ascii_case(name))
            .then(|| {
                let d = &msg[r.rdata.start..r.rdata.end];
                [d[0], d[1], d[2], d[3]]
            })
    })
}

/// `nqn=…` and `nsid=…` out of the TXT record's character-strings.
fn parse_txt(msg: &[u8]) -> (Option<String>, Option<u32>) {
    let mut nqn = None;
    let mut nsid = None;
    for r in records(msg) {
        if r.rtype != TYPE_TXT {
            continue;
        }
        // TXT rdata is a run of length-prefixed strings, one key=value each.
        let mut p = r.rdata.start;
        while p < r.rdata.end {
            let len = msg[p] as usize;
            let end = (p + 1 + len).min(r.rdata.end);
            if let Ok(s) = core::str::from_utf8(&msg[p + 1..end]) {
                if let Some(v) = s.trim().strip_prefix("nqn=") {
                    if !v.is_empty() {
                        nqn = Some(v.to_string());
                    }
                } else if let Some(v) = s.trim().strip_prefix("nsid=") {
                    nsid = v.parse().ok();
                }
            }
            p = end;
        }
    }
    (nqn, nsid)
}
