//! stormbootx's own TCP/IP stack: smoltcp directly on each NIC's
//! `EFI_SIMPLE_NETWORK` (#56).
//!
//! Until 0.9 this binary used the firmware's `EFI_TCP4`, which is an optional
//! stack three drivers deep. Some firmware has it, some has it off in setup,
//! and some has it and binds it to nothing. server3 (Supermicro X9, AMI Aptio 4)
//! loaded its NIC driver from the media and then reported `EFI_TCP4 is not
//! present`. Carrying EDK2's own stack was tried on paper and dropped: since
//! edk2-stable202405 its IPv4 drivers refuse to start without `EFI_RNG` (and
//! TcpDxe without `EFI_HASH2`), which firmware of that age does not have. The
//! owner's decision (2026-09-30): **one stack everywhere**. smoltcp runs on the
//! SNP a NIC driver produces, whether the firmware's own, iPXE's or a stormnic
//! driver, and the firmware's TCP4 is never asked for.
//!
//! - **SNP is opened exclusively.** A firmware MNP bound to the same SNP would
//!   also pull frames from it, and a frame it takes never reaches smoltcp.
//!   `EXCLUSIVE` makes the firmware disconnect its MNP/IP4/TCP4 from that NIC.
//!   If the open is refused, the NIC is used shared and the console says so.
//!   `release` gives every NIC back (closes the opens and reconnects them) on
//!   the fall-through, so a later boot option finds the firmware's stack
//!   again.
//! - **DHCP** comes from smoltcp's dhcpv4 socket, on every NIC at once. The raw
//!   reply is kept for `dnsname.rs` (options 12/15/6, #23).
//! - **UDP** is used once per boot, for the SNTP request that sets the clock
//!   and the DNS lookup of its server (#77): `udp_exchange`.
//! - **ARP** and **TCP** are smoltcp's. A connection goes out on the NIC that
//!   reached the target last time, else the best-ranked NIC with a lease: link
//!   up first, then the larger MTU (the storage port is the jumbo one).
//! - **Randomness** (`entropy.rs`). smoltcp draws the TCP ISN and the DHCP
//!   xid from a generator seeded once per NIC, and each connection's local
//!   port is drawn here. Both come from the first source present: the
//!   firmware's `EFI_RNG`, the CPU (RDSEED/RDRAND), or cycle-counter jitter.
//!   Nothing is installed as `EFI_RNG`, so the next stage (the Linux EFI stub
//!   seeds its RNG from that protocol) never finds a weak one left behind.
//! - **Time** is the TSC, calibrated against `Stall` at bring-up. There is no
//!   timer protocol to depend on, and every loop here polls anyway.
//!
//! EFI boot services run on one thread and nothing here is reentrant: a
//! BlockIO read from the chain-loaded stage comes back through `nvme.rs` to
//! this module, never concurrently with it. That is what makes the global
//! below sound.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ptr;

use crate::entropy::{self, Entropy};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{dhcpv4, tcp, udp};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use uefi::boot::{self, SearchType};
use uefi::{Guid, Status, guid};
use uefi_raw::Boolean;
use uefi_raw::protocol::network::snp::{
    InterruptStatus, NetworkMode, NetworkState, ReceiveFlags, SimpleNetworkProtocol,
};

/// EFI_SIMPLE_NETWORK_PROTOCOL: the NIC driver's interface, and the bottom of
/// this stack.
pub const SNP: Guid = guid!("a19832b9-ac25-11d3-9a2d-0090273fc14d");

const OPEN_EXCLUSIVE: u32 = 0x20;

/// Open a protocol by GUID, returning the raw interface.
///
/// The uefi crate's typed wrappers require a `Protocol` impl, which a raw
/// vtable does not have, so this goes through HandleProtocol.
pub fn handle_protocol(handle: uefi_raw::Handle, guid: &Guid) -> Option<*mut core::ffi::c_void> {
    let mut iface = ptr::null_mut();
    let st = unsafe {
        let st_ptr = uefi::table::system_table_raw()?;
        let bs = st_ptr.as_ref().boot_services.as_ref()?;
        (bs.handle_protocol)(handle, guid as *const Guid as *const uefi_raw::Guid, &mut iface)
    };
    (st == Status::SUCCESS && !iface.is_null()).then_some(iface)
}

fn bs() -> Option<&'static uefi_raw::table::boot::BootServices> {
    unsafe { uefi::table::system_table_raw()?.as_ref().boot_services.as_ref() }
}

/// `ConnectController(handle, NULL, NULL, TRUE)`: bind every driver that will
/// bind, recursively. Failures are the normal case (most handles are not
/// controllers) and are not reported.
pub fn connect(handle: uefi_raw::Handle) {
    if let Some(bs) = bs() {
        unsafe {
            let _ = (bs.connect_controller)(handle, ptr::null_mut(), ptr::null(), Boolean::TRUE);
        }
    }
}

/// `ConnectController` over every handle. This is what makes a NIC driver the
/// platform has, but never started, produce its SNP.
pub fn connect_all() {
    if let Ok(handles) = boot::locate_handle_buffer(SearchType::AllHandles) {
        for h in handles.iter() {
            connect(h.as_ptr());
        }
    }
}

fn snp_handles() -> Vec<uefi_raw::Handle> {
    boot::locate_handle_buffer(SearchType::ByProtocol(&SNP))
        .map(|h| h.iter().map(|h| h.as_ptr()).collect())
        .unwrap_or_default()
}

/// The MAC this machine is known by when nothing has named it (#15), and how
/// many NICs it was chosen from.
///
/// Read from every SNP's permanent address, not only the NIC that reaches the
/// engine: identity is about the machine, not about which port is cabled
/// today. The lowest usable one wins (`universal::better_mac`), so the answer
/// does not depend on the order drivers happened to bind in. Only 6-byte
/// hardware addresses count. Call it after `up`, which is what binds a NIC the
/// firmware left unconnected.
pub fn machine_mac() -> Option<([u8; 6], usize)> {
    let handles = snp_handles();
    let mut best = None;
    for &h in handles.iter() {
        let Some(m) = mode_of(h) else { continue };
        if m.hw_address_size != 6 {
            continue;
        }
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&m.permanent_address.0[..6]);
        best = crate::universal::better_mac(best, mac);
    }
    best.map(|m| (m, handles.len()))
}

fn mode_of(handle: uefi_raw::Handle) -> Option<&'static NetworkMode> {
    let snp = handle_protocol(handle, &SNP)? as *const SimpleNetworkProtocol;
    let mode = unsafe { (*snp).mode };
    (!mode.is_null()).then(|| unsafe { &*mode })
}

fn mac_text(mac: &[u8]) -> String {
    let mut s = String::new();
    for (i, b) in mac.iter().take(6).enumerate() {
        if i > 0 {
            s.push(':');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn ip_text(a: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

// ---------------------------------------------------------------- time, seed

fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// The TSC, counted in microseconds from bring-up.
struct Clock {
    base: u64,
    per_ms: u64,
}

impl Clock {
    /// Calibrated against 20 ms of `Stall`. The TSC is invariant on every
    /// server this boots (and under KVM); the error of one calibration only
    /// stretches or shrinks the timeouts a little.
    fn calibrate() -> Self {
        let a = rdtsc();
        boot::stall(core::time::Duration::from_millis(20));
        let b = rdtsc();
        let per_ms = (b.wrapping_sub(a) / 20).max(1);
        Clock { base: b, per_ms }
    }

    fn now(&self) -> Instant {
        let ticks = rdtsc().wrapping_sub(self.base);
        Instant::from_micros((ticks as u128 * 1000 / self.per_ms as u128) as i64)
    }
}

/// A local port from the dynamic range, 49152..=65535.
fn local_port(rng: &mut Entropy) -> u16 {
    49152 + (rng.next_u64() % 16384) as u16
}

// ------------------------------------------------------------ the SNP device

/// Frames handed to `Transmit` and not yet given back by `GetStatus`. The NIC
/// may still be reading them, so they must stay allocated until then.
struct TxQueue {
    snp: *const SimpleNetworkProtocol,
    inflight: Vec<Vec<u8>>,
}

impl TxQueue {
    fn reclaim(&mut self) {
        for _ in 0..64 {
            let mut ist = InterruptStatus::empty();
            let mut done: *mut core::ffi::c_void = ptr::null_mut();
            let st = unsafe { ((*self.snp).get_status)(self.snp, &mut ist, &mut done) };
            if st != Status::SUCCESS || done.is_null() {
                break;
            }
            if let Some(i) = self.inflight.iter().position(|b| b.as_ptr() as *mut _ == done) {
                self.inflight.swap_remove(i);
            }
        }
        // A driver that never hands buffers back must not grow this without
        // bound. What is dropped from the list is leaked, never freed: the NIC
        // may still own it.
        while self.inflight.len() > 1024 {
            core::mem::forget(self.inflight.remove(0));
        }
    }

    fn send(&mut self, frame: Vec<u8>) {
        self.reclaim();
        for _ in 0..2000 {
            let st = unsafe {
                ((*self.snp).transmit)(
                    self.snp,
                    0,
                    frame.len(),
                    frame.as_ptr() as *const _,
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                )
            };
            match st {
                Status::SUCCESS => {
                    self.inflight.push(frame);
                    return;
                }
                // The transmit queue is full: take back what has gone out.
                Status::NOT_READY => {
                    self.reclaim();
                    boot::stall(core::time::Duration::from_micros(5));
                }
                // Dropped, as a wire would. TCP retransmits.
                _ => return,
            }
        }
    }
}

struct SnpDevice {
    snp: *const SimpleNetworkProtocol,
    rx: Vec<u8>,
    tx: TxQueue,
    /// Link MTU plus the media header: smoltcp's MTU for an Ethernet medium.
    mtu: usize,
}

impl SnpDevice {
    fn poll_rx(&mut self) -> Option<usize> {
        let mut header = 0usize;
        let mut size = self.rx.len();
        let mut src: uefi_raw::MacAddress = unsafe { core::mem::zeroed() };
        let mut dst: uefi_raw::MacAddress = unsafe { core::mem::zeroed() };
        let mut proto = 0u16;
        let st = unsafe {
            ((*self.snp).receive)(
                self.snp,
                &mut header,
                &mut size,
                self.rx.as_mut_ptr() as *mut _,
                &mut src,
                &mut dst,
                &mut proto,
            )
        };
        match st {
            Status::SUCCESS => Some(size.min(self.rx.len())),
            Status::BUFFER_TOO_SMALL => {
                // Taken on the next poll.
                self.rx.resize(size.max(self.rx.len() * 2), 0);
                None
            }
            _ => None,
        }
    }
}

struct Rx<'a>(&'a [u8]);
struct Tx<'a>(&'a mut TxQueue);

impl RxToken for Rx<'_> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(self.0)
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0u8; len];
        let r = f(&mut frame);
        self.0.send(frame);
        r
    }
}

impl Device for SnpDevice {
    type RxToken<'a> = Rx<'a>;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _: Instant) -> Option<(Rx<'_>, Tx<'_>)> {
        let n = self.poll_rx()?;
        Some((Rx(&self.rx[..n]), Tx(&mut self.tx)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = self.mtu;
        caps
    }
}

// ------------------------------------------------------------------ the NICs

/// What a lease gave one NIC.
#[derive(Clone)]
struct Lease {
    address: [u8; 4],
    prefix: u8,
    router: Option<[u8; 4]>,
    /// The DHCP message the lease came from, header onwards (#23).
    reply: Option<Vec<u8>>,
}

struct Nic {
    index: usize,
    handle: uefi_raw::Handle,
    exclusive: bool,
    mac: [u8; 32],
    mac_len: usize,
    link_mtu: u32,
    dev: SnpDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    lease: Option<Lease>,
}

impl Nic {
    fn link_up(&self) -> bool {
        mode_of(self.handle).is_some_and(|m| bool::from(m.media_present))
    }

    fn poll(&mut self, now: Instant) {
        self.iface.poll(now, &mut self.dev, &mut self.sockets);
        let event = match self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll() {
            None => return,
            Some(dhcpv4::Event::Deconfigured) => None,
            Some(dhcpv4::Event::Configured(c)) => Some(Lease {
                address: c.address.address().octets(),
                prefix: c.address.prefix_len(),
                router: c.router.map(|r| r.octets()),
                reply: c.packet.map(|p| p.into_inner().to_vec()),
            }),
        };
        match event {
            Some(l) => {
                let cidr = IpCidr::new(IpAddress::Ipv4(Ipv4Address::from(l.address)), l.prefix);
                self.iface.update_ip_addrs(|a| {
                    a.clear();
                    let _ = a.push(cidr);
                });
                match l.router {
                    Some(r) => {
                        let _ = self.iface.routes_mut().add_default_ipv4_route(Ipv4Address::from(r));
                    }
                    None => {
                        self.iface.routes_mut().remove_default_ipv4_route();
                    }
                }
                let fresh = self.lease.as_ref().is_none_or(|o| o.address != l.address);
                if fresh {
                    uefi::println!(
                        "      nic {}: leased {}/{}{}",
                        self.index,
                        ip_text(l.address),
                        l.prefix,
                        l.router.map(|r| format!(" gw {}", ip_text(r))).unwrap_or_default()
                    );
                }
                self.lease = Some(l);
            }
            None => {
                self.iface.update_ip_addrs(|a| a.clear());
                self.iface.routes_mut().remove_default_ipv4_route();
                if self.lease.take().is_some() {
                    uefi::println!("      nic {}: lease lost", self.index);
                }
            }
        }
    }
}

/// DHCP asks for these: mask, router, DNS, host name, domain (#23), NTP
/// servers (#77).
static DHCP_PARAMS: [u8; 6] = [1, 3, 6, 12, 15, 42];

struct Net {
    nics: Vec<Nic>,
    clock: Clock,
    rng: Entropy,
    /// The NIC that reached a target last: tried first next time.
    preferred: Option<usize>,
}

struct Global(core::cell::UnsafeCell<Option<Net>>);
// Boot services are single-threaded, and nothing here is reentrant (see the
// module comment).
unsafe impl Sync for Global {}
static NET: Global = Global(core::cell::UnsafeCell::new(None));

fn net() -> Result<&'static mut Net, String> {
    unsafe { (*NET.0.get()).as_mut() }.ok_or_else(|| "the network is not up".to_string())
}

/// What `up` found, for the console.
pub struct Up {
    /// `nic 0 52:54:00:12:34:56, nic 1 …`
    pub nics: String,
    /// Where the ISN seed and the ports come from.
    pub rng: entropy::Source,
    /// Milliseconds spent waiting for an SNP to appear.
    pub waited_ms: u64,
    /// NICs the firmware would not let go of (opened shared).
    pub shared: usize,
}

/// What to tell an operator when no NIC has a UEFI driver.
pub const NO_SNP_ADVICE: &str = "no EFI_SIMPLE_NETWORK on any handle: no UEFI driver bound a NIC, even \
after connecting every handle and waiting five seconds. stormbootx needs only that (its TCP/IP is \
its own), so the NIC needs a UEFI driver: the firmware's (a NIC set to UEFI or PXE mode in setup) \
or one on the media in \\stormboot\\drivers (see scripts/build-nic-drivers.sh).";

/// Bring the stack up on every NIC: open each SNP, start it, and begin DHCP on
/// all of them. Returns as soon as that is done. Leases arrive while the first
/// connection waits (`TcpSocket::connect_within`).
///
/// A NIC driver the platform has not dispatched yet produces no SNP, and on a
/// real machine it is not always ready when this runs: on a Dell the first
/// boot option found no network and the second, seconds later, did. So with
/// no SNP this binds every handle and waits up to 5 s before giving up.
pub fn up(rng: entropy::Start) -> Result<Up, String> {
    if let Ok(n) = net() {
        return Ok(summary(n, 0));
    }
    let mut handles = snp_handles();
    let mut waited = 0u64;
    if handles.is_empty() {
        connect_all();
        handles = snp_handles();
    }
    while handles.is_empty() && waited < 5_000 {
        boot::stall(core::time::Duration::from_millis(250));
        waited += 250;
        connect_all();
        handles = snp_handles();
    }
    if handles.is_empty() {
        return Err(NO_SNP_ADVICE.into());
    }

    let clock = Clock::calibrate();
    let mut rng = Entropy::new(rng, machine_mac().map(|(m, _)| m));
    let mut nics = Vec::new();
    for (index, &handle) in handles.iter().enumerate() {
        match open_nic(index, handle, &clock, &mut rng) {
            Ok(nic) => {
                uefi::println!(
                    "      nic {index}: mac {}  mtu {}  link {}{}",
                    mac_text(&nic.mac[..nic.mac_len]),
                    nic.link_mtu,
                    if nic.link_up() { "UP" } else { "down/unknown" },
                    if nic.exclusive { "" } else { "  (shared: the firmware kept it)" }
                );
                nics.push(nic);
            }
            Err(e) => uefi::println!("      nic {index}: not used ({e})"),
        }
    }
    if nics.is_empty() {
        return Err(format!("{} NIC(s) have an SNP, and none of them would start", handles.len()));
    }
    let n = unsafe { &mut *NET.0.get() }.insert(Net { nics, clock, rng, preferred: None });
    Ok(summary(n, waited))
}

fn summary(n: &Net, waited_ms: u64) -> Up {
    let nics = n
        .nics
        .iter()
        .map(|c| format!("nic {} {}", c.index, mac_text(&c.mac[..c.mac_len])))
        .collect::<Vec<_>>()
        .join(", ");
    Up {
        nics,
        rng: n.rng.source,
        waited_ms,
        shared: n.nics.iter().filter(|c| !c.exclusive).count(),
    }
}

fn open_nic(index: usize, handle: uefi_raw::Handle, clock: &Clock, rng: &mut Entropy) -> Result<Nic, String> {
    let bs = bs().ok_or("no boot services")?;
    let mut iface: *mut core::ffi::c_void = ptr::null_mut();
    let st = unsafe {
        (bs.open_protocol)(
            handle,
            &SNP as *const Guid as *const uefi_raw::Guid,
            &mut iface,
            boot::image_handle().as_ptr(),
            ptr::null_mut(),
            OPEN_EXCLUSIVE,
        )
    };
    let exclusive = st == Status::SUCCESS && !iface.is_null();
    if !exclusive {
        iface = handle_protocol(handle, &SNP).ok_or("no SNP interface")?;
    }
    let snp = iface as *const SimpleNetworkProtocol;
    let mode = || unsafe { &*(*snp).mode };
    unsafe {
        if (*snp).mode.is_null() {
            return Err("SNP has no mode".into());
        }
        if mode().state == NetworkState::STOPPED {
            let s = ((*snp).start)(snp);
            if s != Status::SUCCESS && s != Status::ALREADY_STARTED {
                return Err(format!("SNP Start: {s:?}"));
            }
        }
        if mode().state == NetworkState::STARTED {
            let s = ((*snp).initialize)(snp, 0, 0);
            if s != Status::SUCCESS {
                return Err(format!("SNP Initialize: {s:?}"));
            }
        }
        let want = ReceiveFlags::UNICAST | ReceiveFlags::BROADCAST;
        let s = ((*snp).receive_filters)(snp, want, ReceiveFlags::empty(), Boolean::FALSE, 0, ptr::null());
        if s != Status::SUCCESS {
            // A driver that cannot filter can usually still be promiscuous.
            let _ = ((*snp).receive_filters)(
                snp,
                want | ReceiveFlags::PROMISCUOUS,
                ReceiveFlags::empty(),
                Boolean::FALSE,
                0,
                ptr::null(),
            );
        }
    }
    let m = mode();
    if m.hw_address_size != 6 || m.media_header_size != 14 {
        return Err(format!(
            "not Ethernet (address {} bytes, header {})",
            m.hw_address_size, m.media_header_size
        ));
    }
    let link_mtu = if (576..=65_535).contains(&m.max_packet_size) { m.max_packet_size } else { 1500 };
    let mtu = link_mtu as usize + 14;
    let mut dev = SnpDevice {
        snp,
        rx: vec![0u8; mtu.max(1514) + 64],
        tx: TxQueue { snp, inflight: Vec::new() },
        mtu,
    };
    let mut cur = [0u8; 6];
    cur.copy_from_slice(&m.current_address.0[..6]);
    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(cur)));
    cfg.random_seed = rng.next_u64();
    let iface = Interface::new(cfg, &mut dev, clock.now());

    let mut sockets = SocketSet::new(Vec::new());
    let mut dhcp = dhcpv4::Socket::new();
    dhcp.set_parameter_request_list(&DHCP_PARAMS);
    // The reply is kept for its options (#23). 'static because the socket is.
    dhcp.set_receive_packet_buffer(alloc::boxed::Box::leak(vec![0u8; 1500].into_boxed_slice()));
    let mut retry = dhcp.get_retry_config();
    // A 25G link takes seconds to train after Initialize, and a DISCOVER sent
    // into a dark link is lost: ask again every 2 s, not every 10.
    retry.discover_timeout = Duration::from_secs(2);
    retry.initial_request_timeout = Duration::from_secs(2);
    dhcp.set_retry_config(retry);
    let dhcp = sockets.add(dhcp);

    let mut mac = [0u8; 32];
    mac.copy_from_slice(&m.permanent_address.0);
    Ok(Nic {
        index,
        handle,
        exclusive,
        mac,
        mac_len: 6,
        link_mtu,
        dev,
        iface,
        sockets,
        dhcp,
        lease: None,
    })
}

impl Net {
    fn poll_all(&mut self) {
        let now = self.clock.now();
        for nic in self.nics.iter_mut() {
            nic.poll(now);
        }
    }

    /// NIC positions in the order worth trying: the one that worked last,
    /// then link up, then the larger MTU, then enumeration order.
    fn order(&self) -> Vec<usize> {
        let mut o: Vec<usize> = (0..self.nics.len()).collect();
        o.sort_by_key(|&i| {
            let n = &self.nics[i];
            (self.preferred != Some(i), !n.link_up(), u32::MAX - n.link_mtu, i)
        });
        o
    }
}

/// Give every NIC back to the firmware: close the exclusive opens and
/// reconnect, so a boot option after this one finds the firmware's own stack.
/// Called on the fall-through. The stack is gone afterwards.
pub fn release() {
    let Some(n) = (unsafe { (*NET.0.get()).take() }) else { return };
    let Some(bs) = bs() else { return };
    for nic in n.nics.iter().filter(|c| c.exclusive) {
        unsafe {
            let _ = (bs.close_protocol)(
                nic.handle,
                &SNP as *const Guid as *const uefi_raw::Guid,
                boot::image_handle().as_ptr(),
                ptr::null_mut(),
            );
        }
        connect(nic.handle);
    }
    // Frames still queued stay allocated: the NICs may yet read them.
    core::mem::forget(n);
}

/// The DHCP reply that gave the NIC with this permanent MAC its lease, header
/// onwards, for its options (#23).
pub fn dhcp_reply(mac: &[u8], mac_len: usize) -> Option<Vec<u8>> {
    let n = net().ok()?;
    let nic = n.nics.iter().find(|c| mac_len > 0 && c.mac[..mac_len] == mac[..mac_len])?;
    nic.lease.as_ref()?.reply.clone()
}

/// The NIC a request goes out on when it is not a TCP connection: the one
/// that reached a target last, else the best-ranked NIC with a lease.
fn leased_nic(n: &Net) -> Option<usize> {
    n.preferred
        .filter(|&i| n.nics[i].lease.is_some())
        .or_else(|| n.order().into_iter().find(|&i| n.nics[i].lease.is_some()))
}

/// Whether any NIC holds a lease. Nothing waits for one here: the clock
/// (#77) is set on a network that is already up, or not at all.
pub fn leased() -> bool {
    net().is_ok_and(|n| n.nics.iter().any(|c| c.lease.is_some()))
}

/// The DHCP reply of the NIC `udp_exchange` would use, for its options
/// (42, 6; #77).
pub fn leased_reply() -> Option<Vec<u8>> {
    let n = net().ok()?;
    let i = leased_nic(n)?;
    n.nics[i].lease.as_ref()?.reply.clone()
}

/// 64 random bits from the stack's generator: an SNTP nonce, a DNS id.
pub fn random_u64() -> Option<u64> {
    Some(net().ok()?.rng.next_u64())
}

/// Send one UDP datagram to `server:port` and wait up to `ms` for a reply from
/// that address and port that `accept` takes. Returns the reply and the
/// microseconds from the send to its arrival (half of it is the SNTP path
/// delay, #77).
///
/// One datagram, no retransmission: the caller decides whether to try again.
/// The first send to an address whose MAC is not known yet waits on ARP, and
/// smoltcp holds the datagram meanwhile.
pub fn udp_exchange(
    server: [u8; 4],
    port: u16,
    request: &[u8],
    ms: u64,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> Result<(Vec<u8>, u64), String> {
    let n = net()?;
    let i = leased_nic(n).ok_or("no NIC holds a lease")?;
    let local = local_port(&mut n.rng);
    let mut s = udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0u8; 4096]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 2], vec![0u8; 1024]),
    );
    s.bind(local).map_err(|e| format!("bind: {e:?}"))?;
    let dest = (IpAddress::Ipv4(Ipv4Address::from(server)), port);
    s.send_slice(request, dest).map_err(|e| format!("send: {e:?}"))?;
    let h = n.nics[i].sockets.add(s);
    let sent = n.clock.now();
    let deadline = sent + Duration::from_millis(ms);
    let mut buf = vec![0u8; 2048];
    let result = loop {
        n.poll_all();
        let now = n.clock.now();
        let sock = n.nics[i].sockets.get_mut::<udp::Socket>(h);
        let mut got = None;
        while let Ok((k, meta)) = sock.recv_slice(&mut buf) {
            let from_server = meta.endpoint.addr == IpAddress::Ipv4(Ipv4Address::from(server))
                && meta.endpoint.port == port;
            if from_server && accept(&buf[..k]) {
                got = Some(buf[..k].to_vec());
                break;
            }
        }
        if let Some(reply) = got {
            break Ok((reply, (now - sent).total_micros()));
        }
        if now >= deadline {
            break Err(format!("no answer from {}:{port} within {ms} ms", ip_text(server)));
        }
    };
    n.nics[i].sockets.remove(h);
    result
}

/// Wait up to `secs` for leases, then print what every NIC holds. The shell's
/// `state` and `dhcp`.
pub fn show(secs: u32) {
    let Ok(n) = net() else {
        uefi::println!("  the stack is not up");
        return;
    };
    let until = n.clock.now() + Duration::from_secs(secs as u64);
    while n.clock.now() < until && n.nics.iter().any(|c| c.lease.is_none()) {
        n.poll_all();
    }
    for c in n.nics.iter() {
        let lease = match &c.lease {
            Some(l) => format!(
                "{}/{}{}",
                ip_text(l.address),
                l.prefix,
                l.router.map(|r| format!(" gw {}", ip_text(r))).unwrap_or_default()
            ),
            None => "no lease — nothing answered DHCP".to_string(),
        };
        uefi::println!(
            "  nic {}: {}  {}  link {}{}",
            c.index,
            mac_text(&c.mac[..c.mac_len]),
            lease,
            if c.link_up() { "UP" } else { "down" },
            if n.preferred == Some(c.index) { "  (reached the last target)" } else { "" }
        );
    }
}

// ------------------------------------------------------------------ sockets

/// A blocking TCP connection. Everything above this wants ordinary blocking
/// reads and writes (the NVMe state machine is hard enough without an
/// executor underneath it), so each call polls the stack until it is done or
/// its time is up.
pub struct TcpSocket {
    nic: usize,
    handle: SocketHandle,
    /// How long one operation may make no progress.
    secs: u32,
}

const RX_BUFFER: usize = 256 * 1024;
const TX_BUFFER: usize = 64 * 1024;

impl TcpSocket {
    /// Connect, waiting up to 30 s on each operation: the NVMe path's budget.
    pub fn connect(remote: [u8; 4], port: u16) -> Result<Self, String> {
        Self::connect_within(remote, port, 30)
    }

    /// Connect within `secs`, which also covers waiting for a lease.
    ///
    /// A server has more than one NIC, and the one with a cable to the target
    /// is not the first handle: a 1 GbE management port sits in front of the
    /// 25 GbE storage port. So every NIC runs DHCP at once, and the connection
    /// is tried on each NIC with a lease in `Net::order`. Each try gets 5 s
    /// while another NIC is left to try. A reset is an answer, not a wrong
    /// wire: the target was reached and nothing listens on that port.
    pub fn connect_within(remote: [u8; 4], port: u16, secs: u32) -> Result<Self, String> {
        if net().is_err() {
            up(crate::config::stated_rng())?;
        }
        let n = net()?;
        let start = n.clock.now();
        let deadline = start + Duration::from_secs(secs as u64);
        let mut tried = vec![false; n.nics.len()];
        let mut attempt: Option<(usize, SocketHandle, Instant)> = None;
        let mut last = String::new();

        loop {
            n.poll_all();
            let now = n.clock.now();
            if let Some((i, h, t0)) = attempt {
                let state = n.nics[i].sockets.get::<tcp::Socket>(h).state();
                match state {
                    tcp::State::Established => {
                        if n.preferred != Some(i) {
                            if n.nics.len() > 1 {
                                uefi::println!(
                                    "    nic {i} ({}) reached {}:{port}",
                                    mac_text(&n.nics[i].mac[..6]),
                                    ip_text(remote)
                                );
                            }
                            n.preferred = Some(i);
                        }
                        return Ok(TcpSocket { nic: i, handle: h, secs });
                    }
                    tcp::State::Closed => {
                        n.nics[i].sockets.remove(h);
                        return Err(format!(
                            "{}:{port} refused the connection (reset)",
                            ip_text(remote)
                        ));
                    }
                    _ => {
                        let others = n.nics.iter().enumerate().any(|(j, c)| {
                            j != i && !tried[j] && c.lease.is_some()
                        });
                        let limit = if others { Duration::from_secs(5) } else { deadline - t0 };
                        if now - t0 >= limit || now >= deadline {
                            n.nics[i].sockets.get_mut::<tcp::Socket>(h).abort();
                            n.nics[i].poll(now);
                            n.nics[i].sockets.remove(h);
                            last = format!("nic {i}: no answer from {}:{port}", ip_text(remote));
                            tried[i] = true;
                            attempt = None;
                        }
                    }
                }
            }
            if now >= deadline {
                break;
            }
            if attempt.is_none() {
                let order = n.order();
                let mut leased = order.iter().filter(|&&i| n.nics[i].lease.is_some()).peekable();
                if leased.peek().is_some() && leased.all(|&i| tried[i]) {
                    // Every NIC with a lease has had its turn: go round again.
                    tried.iter_mut().for_each(|t| *t = false);
                }
                if let Some(&i) = order.iter().find(|&&i| !tried[i] && n.nics[i].lease.is_some()) {
                    let local = local_port(&mut n.rng);
                    let nic = &mut n.nics[i];
                    let mut s = tcp::Socket::new(
                        tcp::SocketBuffer::new(vec![0u8; RX_BUFFER]),
                        tcp::SocketBuffer::new(vec![0u8; TX_BUFFER]),
                    );
                    // PDUs and requests go out whole and at once; ACKs go back
                    // at once, or a 128 KiB read waits on delayed ACKs.
                    s.set_nagle_enabled(false);
                    s.set_ack_delay(None);
                    let dest = (IpAddress::Ipv4(Ipv4Address::from(remote)), port);
                    match s.connect(nic.iface.context(), dest, local) {
                        Ok(()) => {
                            let h = nic.sockets.add(s);
                            attempt = Some((i, h, now));
                        }
                        Err(e) => {
                            last = format!("nic {i}: connect: {e:?}");
                            tried[i] = true;
                        }
                    }
                }
            }
        }

        if let Some((i, h, _)) = attempt {
            n.nics[i].sockets.get_mut::<tcp::Socket>(h).abort();
            n.nics[i].sockets.remove(h);
        }
        let leased = n.nics.iter().filter(|c| c.lease.is_some()).count();
        if leased == 0 {
            return Err(format!(
                "no address after {secs} s: nothing answered DHCP on any of {} NIC(s)",
                n.nics.len()
            ));
        }
        Err(if last.is_empty() {
            format!("{}:{port} did not answer within {secs} s", ip_text(remote))
        } else {
            format!("{last} — after {secs} s, {leased} of {} NIC(s) leased", n.nics.len())
        })
    }

    fn with<R>(&self, f: impl FnOnce(&mut tcp::Socket<'static>, &Clock) -> R) -> Result<R, String> {
        let n = net()?;
        let nic = &mut n.nics[self.nic];
        nic.poll(n.clock.now());
        Ok(f(nic.sockets.get_mut::<tcp::Socket>(self.handle), &n.clock))
    }

    /// Queue all of `data` for sending. Returns once the stack has taken it,
    /// not once it is acknowledged, as `EFI_TCP4.Transmit` did.
    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        let mut off = 0;
        let limit = Duration::from_secs(self.secs as u64);
        let mut last = net()?.clock.now();
        while off < data.len() {
            let (sent, now) = self.with(|s, clock| {
                if !s.may_send() {
                    return (Err("the connection closed while sending".to_string()), clock.now());
                }
                (s.send_slice(&data[off..]).map_err(|e| format!("send: {e:?}")), clock.now())
            })?;
            let k = sent?;
            if k > 0 {
                off += k;
                last = now;
            } else if now - last > limit {
                return Err("transmit timed out".into());
            }
        }
        // Put it on the wire now rather than at the next read.
        self.with(|_, _| ())
    }

    /// Read exactly `n` bytes. Every PDU header and payload length in NVMe/TCP
    /// is known in advance, so this is the primitive that layer wants.
    pub fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut out = vec![0u8; n];
        let mut got = 0;
        let limit = Duration::from_secs(self.secs as u64);
        let mut last = net()?.clock.now();
        while got < n {
            let (r, now) = self.with(|s, clock| {
                let r = if s.can_recv() {
                    s.recv_slice(&mut out[got..]).map_err(|e| format!("receive: {e:?}"))
                } else if !s.may_recv() {
                    Err("connection closed mid-read".to_string())
                } else {
                    Ok(0)
                };
                (r, clock.now())
            })?;
            let k = r?;
            if k > 0 {
                got += k;
                last = now;
            } else if now - last > limit {
                return Err(format!("receive timed out ({got} of {n} bytes)"));
            }
        }
        Ok(out)
    }

    /// Read until the peer closes, or `limit` bytes. Used for one HTTP
    /// response (`Connection: close`); NVMe reads exact lengths instead.
    pub fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 8192];
        let wait = Duration::from_secs(self.secs as u64);
        let mut last = net()?.clock.now();
        while out.len() < limit {
            let (r, now) = self.with(|s, clock| {
                let r = if s.can_recv() {
                    s.recv_slice(&mut buf).ok()
                } else if !s.may_recv() {
                    None
                } else {
                    Some(0)
                };
                (r, clock.now())
            })?;
            match r {
                None => break,
                Some(0) if now - last > wait => break,
                Some(0) => {}
                Some(k) => {
                    out.extend_from_slice(&buf[..k]);
                    last = now;
                }
            }
        }
        Ok(out)
    }

    /// The MTU of the NIC this connection runs over, in bytes, from its SNP
    /// mode (the link MTU, excluding the media header).
    pub fn link_mtu(&self) -> Option<u32> {
        Some(net().ok()?.nics.get(self.nic)?.link_mtu)
    }

    /// Which wire this connection is on: its NIC's permanent MAC (and that
    /// address's length) and the address it leased.
    ///
    /// The DHCP name (#23) is read from *this* NIC's lease, because a server
    /// with a management port and a storage port may hold a different
    /// reservation on each, and the one that reached the engine is the one the
    /// machine is booting as.
    pub fn interface(&self) -> Option<([u8; 32], usize, [u8; 4])> {
        let nic = net().ok()?.nics.get(self.nic)?;
        Some((nic.mac, nic.mac_len, nic.lease.as_ref()?.address))
    }
}

impl Drop for TcpSocket {
    fn drop(&mut self) {
        let Ok(n) = net() else { return };
        let now = n.clock.now();
        let Some(nic) = n.nics.get_mut(self.nic) else { return };
        nic.sockets.get_mut::<tcp::Socket>(self.handle).abort();
        // Once, so the reset goes out.
        nic.poll(now);
        nic.sockets.remove(self.handle);
    }
}
