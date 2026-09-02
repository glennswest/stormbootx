// Exercises stormbootx's DNS wire code against a real resolver over TCP.
// Same functions, extracted verbatim; only the socket differs.
mod parser;
use parser::*;
use std::io::{Read, Write};
use std::net::TcpStream;

fn ask(server: &str, name: &str, qtype: u16, id: u16) -> std::io::Result<Vec<u8>> {
    let mut s = TcpStream::connect((server, 53))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let m = build_query(name, qtype, id);
    let mut framed = (m.len() as u16).to_be_bytes().to_vec();
    framed.extend_from_slice(&m);
    s.write_all(&framed)?;
    let mut len = [0u8; 2];
    s.read_exact(&mut len)?;
    let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
    s.read_exact(&mut buf)?;
    Ok(buf)
}

fn main() {
    let server = std::env::args().nth(1).unwrap_or("1.1.1.1".into());
    println!("resolver {server}");
    for (name, qtype, label) in [
        ("_xmpp-server._tcp.jabber.org", 33u16, "SRV"),
        ("_sip._udp.sip.voip.ms", 33, "SRV"),
        ("google.com", 16, "TXT"),
        ("one.one.one.one", 1, "A"),
    ] {
        print!("{label:4} {name:32} ");
        match ask(&server, name, qtype, 7) {
            Err(e) => println!("(unreachable: {e})"),
            Ok(resp) => {
                assert_eq!(u16::from_be_bytes([resp[0], resp[1]]), 7, "id echoed");
                let rcode = resp[3] & 0x0F;
                if rcode != 0 { println!("rcode {rcode}"); continue; }
                let n = records(&resp).len();
                // A resolver that answers rcode 0 with nothing in it is a
                // NODATA, not a parser failure — say so, so nobody chases it.
                if n == 0 {
                    println!("(answer carried no records; nothing to parse)");
                    continue;
                }
                match qtype {
                    33 => match parse_srv(&resp) {
                        Some((t, p)) => {
                            let a = find_a(&resp, &t);
                            println!("-> {t}:{p}  additional-A {a:?}  ({n} records walked)");
                        }
                        // e.g. a NODATA whose only record is the zone's SOA.
                        None => println!("no SRV among the {n} records returned"),
                    },
                    16 => {
                        // Real TXT records carry no nqn=/nsid=, so None/None
                        // here is the parser declining to invent them.
                        let (nqn, nsid) = parse_txt(&resp);
                        println!("{n} records, nqn={nqn:?} nsid={nsid:?} (None/None expected)");
                    }
                    _ => println!("-> {:?} ({n} records walked)", find_a(&resp, name)),
                }
            }
        }
    }

    // A synthetic answer in exactly the shape microdns will serve, including a
    // compressed SRV owner name and the A record in the additional section.
    let msg = synth();
    let (target, port) = parse_srv(&msg).expect("synthetic SRV");
    assert_eq!(target, "forge.g16.lo");
    assert_eq!(port, 4420);
    assert_eq!(find_a(&msg, &target), Some([192, 168, 31, 202]));
    println!("\nsynthetic _nvme-disc._tcp.storm.lo -> {target}:{port} 192.168.31.202  OK");

    let txt = synth_txt();
    let (nqn, nsid) = parse_txt(&txt);
    assert_eq!(nqn.as_deref(), Some("nqn.2026-09.lo.g16:stormcos"));
    assert_eq!(nsid, Some(2));
    println!("synthetic TXT -> nqn={} nsid={}  OK", nqn.unwrap(), nsid.unwrap());

    // Priority/weight selection: lower priority wins, then higher weight.
    let (t, _) = parse_srv(&synth_multi()).expect("multi SRV");
    assert_eq!(t, "best.storm.lo", "lowest priority then highest weight");
    println!("priority/weight selection -> {t}  OK");

    // "." as an SRV target means the service is explicitly not offered.
    assert_eq!(parse_srv(&synth_root()), None);
    println!("root SRV target declined  OK");

    // A compression pointer loop must terminate, not hang a machine that has
    // not booted anything yet.
    let mut loopy = synth();
    let at = loopy.len() - 6;
    loopy[at] = 0xC0;
    loopy[at + 1] = at as u8;
    let _ = records(&loopy);
    let _ = read_name(&loopy, at);
    println!("compression-pointer loop terminated  OK");

    // A truncated message must not panic: this parser runs on bytes from the
    // network before any OS exists, so every prefix of a valid answer is an
    // input it can actually see.
    for cut in 0..msg.len() {
        let _ = records(&msg[..cut]);
        let _ = parse_srv(&msg[..cut]);
        let _ = parse_txt(&msg[..cut]);
    }
    println!("every truncation of a valid answer parsed without panic  OK");

    println!("\nALL PARSER CHECKS PASSED");
}

fn name_bytes(n: &str) -> Vec<u8> {
    let mut v = vec![];
    for l in n.split('.') { v.push(l.len() as u8); v.extend_from_slice(l.as_bytes()); }
    v.push(0); v
}

fn header(an: u16, ar: u16) -> Vec<u8> {
    let mut m = vec![0, 7, 0x81, 0x80, 0, 1];
    m.extend_from_slice(&an.to_be_bytes());
    m.extend_from_slice(&0u16.to_be_bytes());
    m.extend_from_slice(&ar.to_be_bytes());
    m
}

fn rr(m: &mut Vec<u8>, name: &[u8], rtype: u16, rdata: &[u8]) {
    m.extend_from_slice(name);
    m.extend_from_slice(&rtype.to_be_bytes());
    m.extend_from_slice(&1u16.to_be_bytes());
    m.extend_from_slice(&300u32.to_be_bytes());
    m.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    m.extend_from_slice(rdata);
}

fn question(name: &str, qtype: u16, an: u16, ar: u16) -> Vec<u8> {
    let mut m = header(an, ar);
    m.extend_from_slice(&name_bytes(name));
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&1u16.to_be_bytes());
    m
}

fn synth() -> Vec<u8> {
    let mut m = question("_nvme-disc._tcp.storm.lo", 33, 1, 1);
    let mut rd = vec![0, 10, 0, 5];
    rd.extend_from_slice(&4420u16.to_be_bytes());
    rd.extend_from_slice(&name_bytes("forge.g16.lo"));
    rr(&mut m, &[0xC0, 0x0C], 33, &rd);           // compressed owner name
    let target = name_bytes("forge.g16.lo");
    rr(&mut m, &target, 1, &[192, 168, 31, 202]); // additional A
    m
}

fn synth_txt() -> Vec<u8> {
    let mut m = question("_nvme-disc._tcp.storm.lo", 16, 1, 0);
    let mut rd = vec![];
    for s in ["nqn=nqn.2026-09.lo.g16:stormcos", "nsid=2"] {
        rd.push(s.len() as u8); rd.extend_from_slice(s.as_bytes());
    }
    rr(&mut m, &[0xC0, 0x0C], 16, &rd);
    m
}

fn synth_multi() -> Vec<u8> {
    let mut m = question("_nvme-disc._tcp.storm.lo", 33, 3, 0);
    for (prio, weight, host) in [(20u16, 99u16, "far.storm.lo"), (10, 5, "ok.storm.lo"), (10, 50, "best.storm.lo")] {
        let mut rd = prio.to_be_bytes().to_vec();
        rd.extend_from_slice(&weight.to_be_bytes());
        rd.extend_from_slice(&4420u16.to_be_bytes());
        rd.extend_from_slice(&name_bytes(host));
        rr(&mut m, &[0xC0, 0x0C], 33, &rd);
    }
    m
}

fn synth_root() -> Vec<u8> {
    let mut m = question("_nvme-disc._tcp.storm.lo", 33, 1, 0);
    let mut rd = vec![0, 0, 0, 0];
    rd.extend_from_slice(&0u16.to_be_bytes());
    rd.push(0); // the root name
    rr(&mut m, &[0xC0, 0x0C], 33, &rd);
    m
}
