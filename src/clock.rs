//! The hardware clock, set from NTP before Linux starts (#77).
//!
//! The X9 blades have no RTC battery, and neither their BIOS nor their BMC
//! sets the host clock from NTP: after every power cut they boot at whatever
//! the RTC reset to. stormbootx already has a network by the time it claims,
//! so it asks one NTP server once and corrects the RTC with UEFI `SetTime`.
//! Linux then starts with the right time, and on the next boot the RTC is
//! close even before the network (until the next power cut).
//!
//! - **Which server.** DHCP option 42 of the lease on the NIC that reached
//!   the engine, else `ntp =` in `stormboot.conf` (`host[:port]`), else
//!   `pool.ntp.org`. A name is looked up with one A query over UDP to the
//!   lease's option 6 server (or `dns =`). `ntp = off` leaves the clock alone.
//! - **Bounded.** Two tries of a second each for the lookup and for the SNTP
//!   request; no lease, no answer or a reply `sntp.rs` does not believe is
//!   one console line and the boot goes on. The clock never blocks a boot.
//! - **UTC in the RTC.** Linux on x86 reads the CMOS fields as UTC (stormcos
//!   keeps the RTC in UTC), so the fields `GetTime` returns are compared with
//!   NTP as UTC, and `SetTime` writes UTC. The time zone and daylight fields
//!   are written back as the firmware reported them: EDK2 stores those in an
//!   NV variable when they change, and nothing here writes NVRAM.
//! - **A second either way is left alone**, so a correct RTC is not written
//!   on every boot.
//! - **Linux is told** in the volatile `StormBootClock` variable (`handoff.rs`):
//!   `synced:<server address>` once the RTC is known to be right (set, or
//!   already within a second), `unsynced` otherwise. stormblock's initramfs
//!   reads it and does not step the clock again after `synced`
//!   (stormblock#253).
//!
//! Runs once per boot, from whichever comes first: after the claim, or the
//! fall-through (a `local` intent, an attach that failed), before the NICs are
//! released.

use alloc::format;
use alloc::string::{String, ToString};
use core::sync::atomic::{AtomicBool, Ordering};

use uefi::runtime::{self, Daylight, Time, TimeParams};

use crate::{config, dnsname, handoff, net, sntp};

/// The server asked when neither the lease nor the media names one.
const DEFAULT_SERVER: &str = "pool.ntp.org";

/// One try: a DNS query or an SNTP request. Two tries each.
const TRY_MS: u64 = 1000;

static DONE: AtomicBool = AtomicBool::new(false);

/// Check the RTC against NTP, set it if it is out by more than a second, say
/// so in one line, and hand the outcome to Linux. Never fails; once per boot.
pub fn sync() {
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let value = match check() {
        Ok(server) => format!("synced:{server}"),
        Err(line) => {
            uefi::println!("clock       : {line}");
            "unsynced".to_string()
        }
    };
    handoff::set_clock(&value);
}

fn ip_text(a: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

/// The RTC's fields as it reports them, for the console.
fn rtc_text(t: Option<&Time>) -> String {
    match t {
        Some(t) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.year(),
            t.month(),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        None => "an RTC that GetTime cannot read".to_string(),
    }
}

/// The RTC's time as Unix seconds, read as UTC. `None` for an RTC that reads
/// no valid date.
fn rtc_secs(t: &Time) -> Option<u64> {
    sntp::unix(&sntp::Civil {
        year: t.year(),
        month: t.month(),
        day: t.day(),
        hour: t.hour(),
        minute: t.minute(),
        second: t.second(),
    })
}

/// Why the clock was left as it is, with what it reads.
fn left(why: &str) -> String {
    format!("{why}; left at {}", rtc_text(runtime::get_time().ok().as_ref()))
}

/// The address of `host`: itself when it is one, else the first A record
/// from the lease's DNS server (or `dns =`).
fn resolve(host: &str, lease: &[u8]) -> Result<[u8; 4], String> {
    lookup(host, lease).map_err(|e| left(&e))
}

/// `host` as an address: itself when it is one, else the first A record from
/// the lease's option 6 server (or `dns =`), asked twice over UDP. Also the
/// self-update's lookup of stormcentral (#83).
pub fn lookup(host: &str, lease: &[u8]) -> Result<[u8; 4], String> {
    if let Some(a) = config::parse_ipv4(host) {
        return Ok(a);
    }
    let dns = dnsname::dhcp_option(lease, dnsname::OPT_DNS)
        .and_then(dnsname::first_dns)
        .or_else(config::stated_dns)
        .ok_or_else(|| format!("no DNS server to look up {host} (none in the lease, no dns =)"))?;
    let mut last = String::new();
    for _ in 0..2 {
        let id = net::random_u64().unwrap_or(0x5b77) as u16;
        let mut q = [0u8; 300];
        let n = dnsname::a_query(host, id, &mut q).ok_or_else(|| format!("{host:?} is not a DNS name"))?;
        // Any reply to this id ends the wait, an NXDOMAIN included.
        let ours = |m: &[u8]| m.len() >= 12 && m[..2] == id.to_be_bytes() && m[2] & 0x80 != 0;
        match net::udp_exchange(dns, 53, &q[..n], TRY_MS, ours) {
            Ok((m, _)) => {
                return dnsname::a_answer(&m, id)
                    .ok_or_else(|| format!("{host} has no address at DNS server {}", ip_text(dns)))
            }
            Err(e) => last = e,
        }
    }
    Err(format!("could not look up {host}: {last}"))
}

/// The NTP server's address on success, or the console line on failure.
fn check() -> Result<String, String> {
    let stated = config::stated_ntp();
    let setting = match stated.as_deref() {
        None => None,
        Some(v) => match sntp::setting(v) {
            Some(sntp::Setting::Off) => {
                return Err(left(&format!("not checked (ntp = off in {})", config::CONF_PATH)))
            }
            Some(sntp::Setting::Server(h, p)) => Some((h.to_string(), p)),
            None => {
                uefi::println!("clock       : ntp = {v:?} in {} is not host[:port]; ignored", config::CONF_PATH);
                None
            }
        },
    };
    if !net::leased() {
        return Err(left("NTP unreachable (no NIC holds a lease)"));
    }
    let lease = net::leased_reply().unwrap_or_default();
    let from_dhcp = dnsname::dhcp_option(&lease, dnsname::OPT_NTP).and_then(dnsname::first_dns);
    let (server, port) = match (from_dhcp, setting) {
        (Some(a), _) => (a, sntp::PORT),
        (None, Some((h, p))) => (resolve(&h, &lease)?, p),
        (None, None) => (resolve(DEFAULT_SERVER, &lease)?, sntp::PORT),
    };
    let source = if port == sntp::PORT {
        ip_text(server)
    } else {
        format!("{}:{port}", ip_text(server))
    };

    // Two requests at most. A reply to another request is not ours and does
    // not end the wait; any reply to ours does, believed or not.
    let mut last = String::new();
    let mut ntp_ns = None;
    for _ in 0..2 {
        let nonce = net::random_u64().unwrap_or(0x5b77_0000_0000_0077);
        let mut verdict = None;
        let accept = |m: &[u8]| {
            let r = sntp::reply(m, nonce);
            let ours = r != Err(sntp::Reject::NotOurs);
            if ours {
                verdict = Some(r);
            }
            ours
        };
        match net::udp_exchange(server, port, &sntp::request(nonce), TRY_MS, accept) {
            Ok((_, rtt_us)) => match verdict {
                // The server's transmit time plus half the round trip.
                Some(Ok(ns)) => {
                    ntp_ns = Some(ns + rtt_us * 1000 / 2);
                    break;
                }
                Some(Err(r)) => last = r.text().to_string(),
                None => last = "no reply".to_string(),
            },
            Err(e) => last = e,
        }
    }
    let Some(ntp_ns) = ntp_ns else {
        return Err(left(&format!("NTP unreachable at {source} ({last})")));
    };

    let ntp_s = ntp_ns / 1_000_000_000;
    let now = sntp::civil(ntp_s);
    let now_text = String::from_utf8_lossy(&sntp::format(&now)).into_owned();
    let before = runtime::get_time().ok();
    let (delta, needed) = sntp::step(before.as_ref().and_then(rtc_secs), ntp_s);
    if !needed {
        uefi::println!("clock       : {now_text} UTC from {source}; the RTC agrees (within 1 s)");
        return Ok(ip_text(server));
    }

    let (time_zone, daylight) = before
        .map(|t| (t.time_zone(), t.daylight()))
        .unwrap_or((None, Daylight::empty()));
    let params = |time_zone, daylight| TimeParams {
        year: now.year,
        month: now.month,
        day: now.day,
        hour: now.hour,
        minute: now.minute,
        second: now.second,
        nanosecond: (ntp_ns % 1_000_000_000) as u32,
        time_zone,
        daylight,
    };
    let t = Time::new(params(time_zone, daylight))
        .or_else(|_| Time::new(params(None, Daylight::empty())))
        .map_err(|_| left(&format!("NTP said {now_text} UTC, which is not an EFI time")))?;
    // SAFETY: boot services are single-threaded and nothing else is touching
    // the RTC.
    unsafe { runtime::set_time(&t) }
        .map_err(|e| left(&format!("NTP said {now_text} UTC from {source}, and SetTime failed ({:?})", e.status())))?;
    let step = if before.as_ref().and_then(rtc_secs).is_some() {
        format!("step {delta:+} s")
    } else {
        "the RTC read no valid date".to_string()
    };
    uefi::println!(
        "clock       : was {}, set to {now_text} UTC from {source} ({step})",
        rtc_text(before.as_ref())
    );
    Ok(ip_text(server))
}
