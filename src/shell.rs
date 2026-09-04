//! A console for the machine that will not boot.
//!
//! Firmware is the worst place to debug blind. When an attach fails there is no
//! shell, no `ip link`, no `tcpdump` — the whole picture is whatever the boot
//! path thought to print on its way past. Bring-up on the first real machine
//! spent six boots distinguishing "no DHCP answer" from "no cable" from "wrong
//! NIC" from "an old binary", each one a walk to the console and back.
//!
//! So: on failure, offer a console instead of only a countdown. It answers the
//! questions that were actually asked — what NICs are there, what does the
//! platform think their addresses are, can this machine reach that host — and
//! then falls through to the local disk exactly as it would have.
//!
//! **It is offered, never forced.** A machine that boots unattended must not
//! stop at a prompt because nobody was watching, so the offer is a timed one
//! and silence takes the normal path. That is the same rule as everywhere else
//! here: one provisioning outage must not become a fleet outage.

use alloc::string::String;
use alloc::vec::Vec;

use uefi::boot::{self, SearchType};
use uefi::proto::console::text::Key;
use uefi::{Guid, guid};
use uefi_raw::protocol::network::snp::{NetworkMode, SimpleNetworkProtocol};

use crate::tcp4::{self, handle_protocol};

const SNP: Guid = guid!("a19832b9-ac25-11d3-9a2d-0090273fc14d");
const IP4_CONFIG2: Guid = guid!("5b446ed1-e30b-4faa-871a-3654eca36080");
const TCP4_SERVICE_BINDING: Guid = guid!("00720665-67eb-4a99-baf7-d3c33a1c7cc9");

/// Offer the console for `secs`, and say whether it was taken.
pub fn offer(secs: u32) -> bool {
    uefi::println!();
    uefi::println!("  press c for the network console, or wait {secs}s to continue");
    for _ in 0..(secs * 10) {
        if let Some('c') | Some('C') = read_char() {
            return true;
        }
        boot::stall(core::time::Duration::from_millis(100));
    }
    false
}

fn read_char() -> Option<char> {
    uefi::system::with_stdin(|stdin| match stdin.read_key() {
        Ok(Some(Key::Printable(c))) => Some(char::from(c)),
        _ => None,
    })
}

/// Read one line, echoing as it is typed. Firmware gives us keys, not lines.
fn read_line() -> String {
    let mut buf = String::new();
    loop {
        let Some(c) = read_char() else {
            boot::stall(core::time::Duration::from_millis(20));
            continue;
        };
        match c {
            '\r' | '\n' => {
                uefi::println!();
                return buf;
            }
            '\u{8}' | '\u{7f}' => {
                if buf.pop().is_some() {
                    uefi::print!("\u{8} \u{8}");
                }
            }
            c if (c as u32) >= 0x20 => {
                buf.push(c);
                uefi::print!("{c}");
            }
            _ => {}
        }
    }
}

pub fn run() {
    uefi::println!();
    uefi::println!("stormbootx console. `help` for commands, `boot` to continue.");
    loop {
        uefi::print!("> ");
        let line = read_line();
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else { continue };
        let args: Vec<&str> = parts.collect();
        match cmd {
            "help" | "?" => help(),
            "nics" | "nic" => nics(),
            "state" | "net" => state(),
            "dhcp" => dhcp(&args),
            "connect" | "tcp" => connect(&args),
            "boot" | "continue" | "exit" | "quit" => {
                uefi::println!("continuing.");
                return;
            }
            other => uefi::println!("unknown command `{other}` — try `help`"),
        }
    }
}

fn help() {
    uefi::println!("  nics              every network interface the firmware knows");
    uefi::println!("  state             what address each interface has, and its policy");
    uefi::println!("  dhcp [n]          run DHCP on interface n, or on all of them");
    uefi::println!("  connect IP PORT   open a TCP connection, the way the attach does");
    uefi::println!("  boot              stop reading and continue the boot");
}

/// Every NIC the firmware has a driver for — **not** only those carrying TCP4.
///
/// The distinction is the point. A machine with four ports that shows two TCP4
/// service bindings has two NICs whose upper stack was never bound, and that
/// looks identical to having two NICs unless something counts both.
fn nics() {
    let snp = boot::locate_handle_buffer(SearchType::ByProtocol(&SNP));
    let tcp = boot::locate_handle_buffer(SearchType::ByProtocol(&TCP4_SERVICE_BINDING));
    let n_snp = snp.as_ref().map(|h| h.len()).unwrap_or(0);
    let n_tcp = tcp.as_ref().map(|h| h.len()).unwrap_or(0);

    uefi::println!("  {n_snp} NIC(s) with a driver, {n_tcp} with a TCP4 stack");
    if n_snp > n_tcp {
        uefi::println!("  {} carry no TCP4 — their upper stack never bound", n_snp - n_tcp);
    }

    let Ok(handles) = snp else {
        uefi::println!("  no EFI_SIMPLE_NETWORK at all");
        return;
    };
    for (i, h) in handles.iter().enumerate() {
        let Some(p) = handle_protocol(h.as_ptr(), &SNP) else { continue };
        let snp = p as *mut SimpleNetworkProtocol;
        let mode: *mut NetworkMode = unsafe { (*snp).mode };
        if mode.is_null() {
            uefi::println!("  nic {i}: no mode data");
            continue;
        }
        let m = unsafe { &*mode };
        let mac = m.permanent_address.0;
        uefi::println!(
            "  nic {i}: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  mtu {}  link {}  {}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
            m.max_packet_size,
            if bool::from(m.media_present) { "UP" } else { "down" },
            // Stopped is the one worth naming: a NIC the firmware has a driver
            // for but never started carries no traffic and answers nothing.
            match m.state.0 {
                0 => "stopped",
                1 => "started",
                2 => "initialized",
                _ => "?",
            }
        );
    }
}

/// What address each interface actually holds, straight from the platform.
fn state() {
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&IP4_CONFIG2)) else {
        uefi::println!("  no EFI_IP4_CONFIG2 — the platform holds no IPv4 configuration");
        return;
    };
    for (i, h) in handles.iter().enumerate() {
        match tcp4::interface_address(h.as_ptr()) {
            Some((addr, mask, policy)) => {
                let [a, b, c, d] = addr;
                let [m0, m1, m2, m3] = mask;
                let unset = addr == [0, 0, 0, 0];
                uefi::println!(
                    "  if {i}: {a}.{b}.{c}.{d}/{m0}.{m1}.{m2}.{m3}  policy {}{}",
                    if policy == 1 { "dhcp" } else { "static" },
                    if unset { "   (no address — nothing answered)" } else { "" }
                );
            }
            None => uefi::println!("  if {i}: could not read its configuration"),
        }
    }
}

fn dhcp(args: &[&str]) {
    let only: Option<usize> = args.first().and_then(|s| s.parse().ok());
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&SNP)) else {
        uefi::println!("  no interfaces");
        return;
    };
    for (i, h) in handles.iter().enumerate() {
        if only.is_some_and(|n| n != i) {
            continue;
        }
        let Some(p) = handle_protocol(h.as_ptr(), &SNP) else { continue };
        let snp = p as *mut SimpleNetworkProtocol;
        let mode: *mut NetworkMode = unsafe { (*snp).mode };
        if mode.is_null() {
            continue;
        }
        let m = unsafe { &*mode };
        let len = (m.hw_address_size as usize).min(32);
        uefi::println!("  nic {i}: asking...");
        match crate::dhcp4::lease_for(&m.permanent_address.0, len) {
            Some(l) => {
                let [a, b, c, d] = l.address;
                let [g0, g1, g2, g3] = l.router;
                uefi::println!("  nic {i}: leased {a}.{b}.{c}.{d} gw {g0}.{g1}.{g2}.{g3}");
            }
            None => uefi::println!("  nic {i}: no lease"),
        }
    }
}

/// Open a TCP connection, which is the thing the boot actually needs to do.
///
/// More useful than an ICMP echo here: a host can answer ping and still refuse
/// the port, and it is the port that decides whether this machine boots.
fn connect(args: &[&str]) {
    let (Some(ip), Some(port)) = (args.first(), args.get(1)) else {
        uefi::println!("  usage: connect 192.168.31.202 4420");
        return;
    };
    let Some(addr) = crate::config::parse_ipv4(ip) else {
        uefi::println!("  `{ip}` is not an address");
        return;
    };
    let Ok(port) = port.parse::<u16>() else {
        uefi::println!("  `{port}` is not a port");
        return;
    };
    let [a, b, c, d] = addr;
    uefi::println!("  connecting to {a}.{b}.{c}.{d}:{port} ...");
    match tcp4::Tcp4Socket::connect_within(addr, port, 8) {
        Ok(_) => uefi::println!("  OPEN — the path works and the port is listening"),
        Err(e) => uefi::println!("  FAILED — {e}"),
    }
}
