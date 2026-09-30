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
use uefi_raw::protocol::network::snp::{NetworkMode, SimpleNetworkProtocol};

use crate::net::{self, handle_protocol, SNP};

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
            "pci" => pci(&args),
            "fec" => fec(&args),
            "reset" | "reboot" => {
                uefi::println!("warm-resetting.");
                uefi::runtime::reset(uefi::runtime::ResetType::WARM, uefi::Status::SUCCESS, None);
            }
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
    uefi::println!("  state             what address each interface leased (stormbootx's own stack)");
    uefi::println!("  dhcp [secs]       wait up to secs (default 10) for every interface's lease");
    uefi::println!("  connect IP PORT   open a TCP connection, the way the attach does");
    uefi::println!("  pci [all]         devices on the bus, driver or no driver");
    uefi::println!("  fec [MODE]        read the ConnectX FEC; with MODE, write it");
    uefi::println!("                    MODE: default | rs | fc | off | autoneg");
    uefi::println!("  reset             warm-reset, so firmware re-reads NV config");
    uefi::println!("  boot              stop reading and continue the boot");
}

/// Read, and on request write, the ConnectX NV FEC override.
///
/// This is the **only** thing in stormbootx that writes FEC, and it is driven
/// by a person typing it. The boot path used to do it on its own and that is
/// what put the R230's card here: a one-shot link sample read a healthy card as
/// all-down and pinned RS on both ports, after which they lit but never linked
/// against a switch on `fec CL108-RS`. Card-side persistent config is an
/// operator decision, not a boot-time guess — see `main.rs` step 2b.
///
/// `fec default` is the way back: value 0, what the card shipped with and what
/// carried these links for 16 h 51 m before anything wrote to them.
fn fec(args: &[&str]) {
    let Some(mode) = args.first() else {
        crate::mlxfec::apply(None);
        uefi::println!("  read-only. `fec default` restores the card's own setting.");
        return;
    };
    let Some(want) = crate::mlxfec::Fec::parse(mode) else {
        uefi::println!("  `{mode}` is not a FEC mode — default | rs | fc | off | autoneg");
        return;
    };
    uefi::println!("  writing FEC {} to every ConnectX port", want.name());
    let sum = crate::mlxfec::apply(Some(want));
    if sum.reset_needed() {
        uefi::println!("  written. `reset` the machine for firmware to re-read NV config.");
    } else {
        uefi::println!("  nothing to write — every port already reads {}.", want.name());
    }
}

/// Every NIC the firmware has a driver for — **not** only those carrying TCP4.
///
/// The distinction is the point. A machine with four ports that shows two TCP4
/// service bindings has two NICs whose upper stack was never bound, and that
/// looks identical to having two NICs unless something counts both.
fn nics() {
    let snp = boot::locate_handle_buffer(SearchType::ByProtocol(&SNP));
    let n_snp = snp.as_ref().map(|h| h.len()).unwrap_or(0);

    // The firmware's TCP4 is not needed (#56): an SNP is all the stack needs.
    uefi::println!("  {n_snp} NIC(s) with a driver (EFI_SIMPLE_NETWORK)");

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

/// What each NIC leased on stormbootx's own stack (#56).
fn state() {
    net::show(0);
}

/// Wait for the leases still outstanding, then show them all.
fn dhcp(args: &[&str]) {
    let secs: u32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(10);
    if let Err(e) = net::up(crate::config::stated_rng()) {
        uefi::println!("  {e}");
        return;
    }
    uefi::println!("  waiting up to {secs} s for leases...");
    net::show(secs);
}

/// Every device on the PCI bus, whether or not firmware has a driver for it.
///
/// This is the command that separates "this machine has two NICs" from "this
/// machine has four NICs and firmware only drives two of them". An SNP handle
/// exists only where a UEFI driver bound; the bus itself does not care, so a
/// card whose option ROM never loaded is invisible everywhere *except* here.
///
/// Read-only, and deliberately its own walk rather than the `enumerate()` the
/// crate offers: enumeration is entitled to assign bus numbers, and a boot path
/// has no business renumbering a live bus to satisfy a diagnostic.
///
/// By default only network and storage controllers are listed, since those are
/// the two this project ever cares about; `pci all` shows everything.
fn pci(args: &[&str]) {
    use uefi::Identify;
    use uefi::proto::pci::PciIoAddress;
    use uefi::proto::pci::root_bridge::PciRootBridgeIo;

    let all = args.first().is_some_and(|a| *a == "all");
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&PciRootBridgeIo::GUID))
    else {
        uefi::println!("  no PCI root bridge — nothing to enumerate");
        return;
    };

    let mut found = 0usize;
    let mut nics = 0usize;
    for h in handles.iter() {
        let Ok(mut bridge) = boot::open_protocol_exclusive::<PciRootBridgeIo>(*h) else {
            continue;
        };
        // Buses are discovered rather than swept: start at 0 and add whatever
        // sits behind each bridge. A blind 0..=255 sweep is 65k config reads
        // through firmware, and most of them address nothing.
        let mut buses: Vec<u8> = alloc::vec![0];
        let mut seen: Vec<u8> = Vec::new();
        while let Some(bus) = buses.pop() {
            if seen.contains(&bus) {
                continue;
            }
            seen.push(bus);
            for dev in 0..32u8 {
                for fun in 0..8u8 {
                    let mut addr = PciIoAddress::new(bus, dev, fun);
                    addr.reg = 0;
                    let Ok(id) = bridge.pci().read_one::<u32>(addr) else { continue };
                    let vendor = (id & 0xFFFF) as u16;
                    if vendor == 0xFFFF || vendor == 0 {
                        // Function 0 absent means the whole device is absent.
                        if fun == 0 {
                            break;
                        }
                        continue;
                    }
                    let device = (id >> 16) as u16;

                    addr.reg = 0x08;
                    let Ok(class_reg) = bridge.pci().read_one::<u32>(addr) else { continue };
                    let class = (class_reg >> 24) as u8;
                    let subclass = (class_reg >> 16) as u8;

                    addr.reg = 0x0C;
                    let header = bridge
                        .pci()
                        .read_one::<u32>(addr)
                        .map(|v| ((v >> 16) & 0xFF) as u8)
                        .unwrap_or(0);

                    // A PCI-to-PCI bridge names the bus behind it at 0x18+1.
                    if (header & 0x7F) == 0x01 {
                        addr.reg = 0x18;
                        if let Ok(v) = bridge.pci().read_one::<u32>(addr) {
                            let secondary = ((v >> 8) & 0xFF) as u8;
                            if secondary != 0 {
                                buses.push(secondary);
                            }
                        }
                    }

                    if class == 0x02 {
                        nics += 1;
                    }
                    if all || class == 0x02 || class == 0x01 {
                        found += 1;
                        uefi::println!(
                            "  {bus:02x}:{dev:02x}.{fun}  {vendor:04x}:{device:04x}  {}",
                            match (class, subclass) {
                                (0x02, _) => "network controller",
                                (0x01, 0x08) => "storage (nvm)",
                                (0x01, _) => "storage controller",
                                _ => "device",
                            }
                        );
                    }

                    // Only a multifunction device has functions past 0.
                    if fun == 0 && (header & 0x80) == 0 {
                        break;
                    }
                }
            }
        }
    }
    if found == 0 {
        uefi::println!("  nothing matched (try `pci all`)");
    } else {
        uefi::println!("  {nics} network controller(s) on the bus");
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
    match net::TcpSocket::connect_within(addr, port, 8) {
        Ok(_) => uefi::println!("  OPEN — the path works and the port is listening"),
        Err(e) => uefi::println!("  FAILED — {e}"),
    }
}
