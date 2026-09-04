//! A blocking socket over the firmware's own TCP stack.
//!
//! EFI networking is entirely asynchronous: every operation takes a token
//! carrying an event that completes later, and nothing progresses unless the
//! caller keeps invoking `Poll` to give the driver cycles. Forgetting that is
//! the classic way to write an EFI network client that hangs forever with no
//! error.
//!
//! Everything above this wants ordinary blocking reads and writes — the NVMe
//! state machine is hard enough without an executor underneath it — so each
//! call here issues one token and pumps until it retires.
//!
//! Using EFI_TCP4 rather than SNP is deliberate: the firmware already has a
//! tested TCP/IP stack and a driver for whatever NIC is fitted. Carrying our
//! own would mean ARP, IP, and a TCP state machine in a boot binary, which is
//! what makes iPXE the size it is.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ptr;

use uefi::boot::{self, SearchType};
use uefi::{Guid, Status, guid};
use uefi_raw::protocol::network::ip4_config2::{
    Ip4Config2DataType, Ip4Config2Policy, Ip4Config2Protocol,
};
use uefi_raw::protocol::network::snp::NetworkMode;
use uefi_raw::protocol::network::tcp4::{
    Tcp4AccessPoint, Tcp4CompletionToken, Tcp4ConfigData, Tcp4ConnectionToken, Tcp4FragmentData,
    Tcp4IoToken, Tcp4Option, Tcp4Packet, Tcp4Protocol, Tcp4ReceiveData, Tcp4TransmitData,
};
use uefi_raw::table::boot::{EventType, Tpl};
use uefi_raw::{Boolean, Ipv4Address};

// uefi-raw declares `fragment_table` as a flexible array member ([_; 0]), so
// the real structures cannot be built by value. These are the same layout with
// exactly one fragment, which is all this client ever sends: one contiguous
// buffer per operation.
#[repr(C)]
struct TxData1 {
    push: Boolean,
    urgent: Boolean,
    data_length: u32,
    fragment_count: u32,
    fragment_table: [Tcp4FragmentData; 1],
}

#[repr(C)]
struct RxData1 {
    urgent: Boolean,
    data_length: u32,
    fragment_count: u32,
    fragment_table: [Tcp4FragmentData; 1],
}

pub const TCP4_SERVICE_BINDING: Guid = guid!("00720665-67eb-4a99-baf7-d3c33a1c7cc9");
pub const TCP4: Guid = guid!("65530bc7-a359-410f-b010-5aadc7ec2b62");
/// `EFI_IP4_CONFIG2_PROTOCOL` — where the platform keeps the interface's
/// address policy, and the only place to say "run DHCP".
const IP4_CONFIG2: Guid = guid!("5b446ed1-e30b-4faa-871a-3654eca36080");

/// EFI_SERVICE_BINDING_PROTOCOL — two calls, and uefi-raw does not model it.
#[repr(C)]
pub struct ServiceBinding {
    pub create_child: unsafe extern "efiapi" fn(*mut Self, *mut uefi_raw::Handle) -> Status,
    pub destroy_child: unsafe extern "efiapi" fn(*mut Self, uefi_raw::Handle) -> Status,
}

/// Open a protocol by GUID, returning the raw interface.
///
/// The uefi crate's typed wrappers require a `Protocol` impl, which a raw
/// vtable like the service binding does not have, so go through HandleProtocol.
pub fn handle_protocol(handle: uefi_raw::Handle, guid: &Guid) -> Option<*mut core::ffi::c_void> {
    let mut iface = ptr::null_mut();
    let st = unsafe {
        let st_ptr = uefi::table::system_table_raw()?;
        let bs = st_ptr.as_ref().boot_services.as_ref()?;
        (bs.handle_protocol)(
            handle,
            guid as *const Guid as *const uefi_raw::Guid,
            &mut iface,
        )
    };
    (st == Status::SUCCESS && !iface.is_null()).then_some(iface)
}

fn new_event() -> Result<uefi_raw::Event, String> {
    unsafe {
        let st = uefi::table::system_table_raw().ok_or("no system table")?;
        let bs = st.as_ref().boot_services.as_ref().ok_or("no boot services")?;
        let mut event: uefi_raw::Event = ptr::null_mut();
        let s = (bs.create_event)(
            EventType::empty(),
            Tpl::APPLICATION,
            None,
            ptr::null_mut(),
            &mut event,
        );
        if s != Status::SUCCESS {
            return Err(format!("CreateEvent failed: {s:?}"));
        }
        Ok(event)
    }
}

fn close_event(event: uefi_raw::Event) {
    unsafe {
        if let Some(st) = uefi::table::system_table_raw() {
            if let Some(bs) = st.as_ref().boot_services.as_ref() {
                let _ = (bs.close_event)(event);
            }
        }
    }
}

fn signalled(event: uefi_raw::Event) -> bool {
    unsafe {
        let Some(st) = uefi::table::system_table_raw() else {
            return false;
        };
        let Some(bs) = st.as_ref().boot_services.as_ref() else {
            return false;
        };
        (bs.check_event)(event) == Status::SUCCESS
    }
}

/// EFI_SIMPLE_NETWORK_PROTOCOL — the bottom of the stack, and the handle the
/// upper layers bind onto.
pub const SNP: Guid = guid!("a19832b9-ac25-11d3-9a2d-0090273fc14d");

/// Is a TCP4 service binding present right now?
pub fn available() -> bool {
    boot::locate_handle_buffer(SearchType::ByProtocol(&TCP4_SERVICE_BINDING))
        .map(|h| !h.is_empty())
        .unwrap_or(false)
}

/// How TCP4 turned out to be reachable, for the console line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// The firmware had already bound its network stack.
    Present,
    /// It had not, and a `ConnectController` pass over the SNP handles made it
    /// appear. This is the case worth naming: the drivers were in the image
    /// all along and nothing had asked for them.
    BoundOnDemand,
    /// A pass over every handle in the system made it appear. Rarer, and
    /// usually means the layered drivers hang off something that is not the
    /// NIC handle.
    BoundAfterFullPass,
    /// It appeared only after waiting and binding again, with the milliseconds
    /// it took. Two things produce this and the number tells them apart: a NIC
    /// driver the platform had not dispatched yet when this ran, and a stack
    /// that binds asynchronously and had not finished. Either way the first
    /// boot option in the order is the one that pays for it.
    BoundAfterWait(u64),
    /// The firmware does not carry an upper network stack.
    Absent,
}

/// Bind whatever layered network drivers the firmware has, then look again.
///
/// A machine can carry the IP4/TCP4 DXE drivers and still show no TCP4 service
/// binding, because nothing has connected them to the NIC handle yet — UEFI
/// binds drivers on demand, and an application that only ever calls
/// `LocateHandleBuffer` never creates that demand. `ConnectController` is what
/// creates it.
///
/// Worth knowing before assuming this is the fix: it is **not** enough on
/// Fedora's OVMF, which ships no upper network stack at all. SNP appears,
/// MNP/IP4/TCP4 do not, and a pass over every handle changes nothing. The
/// hypothesis this tests is present-but-unbound on real firmware, which is a
/// different failure and much the more likely one on an enterprise server.
///
/// Cheap to attempt and only attempted when the alternative is refusing to
/// boot, so it runs before the error path rather than instead of it.
pub fn ensure_available() -> Presence {
    if available() {
        return Presence::Present;
    }

    // The NIC handles first. This is the targeted version of the hypothesis
    // and it avoids binding every unrelated driver in the system to every
    // unrelated handle just to find a network stack.
    if let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&SNP)) {
        for h in handles.iter() {
            connect(h.as_ptr());
        }
        if available() {
            return Presence::BoundOnDemand;
        }
    }

    // Then everything. On firmware whose layered drivers hang off a parent
    // that is not the SNP handle this is what finds them; on firmware that has
    // none it is wasted work on a path that was going to fail anyway.
    if connect_all() {
        return Presence::BoundAfterFullPass;
    }

    // Then wait, and try again.
    //
    // One pass and an immediate answer assumes the platform was ready when we
    // ran, and on a real machine it is not always: observed on a Dell where
    // the *first* boot option reported no TCP4 and the *second*, seconds later
    // in the same boot, found it — the NIC's driver had not been dispatched
    // when the first one ran, so there was nothing for `ConnectController` to
    // bind and no amount of passes would have helped. A stack that binds
    // asynchronously produces the same symptom.
    //
    // Both are cured by waiting, so wait. `WINDOW` is spent only on a machine
    // that is going to fail anyway, and against that: a boot that falls
    // through to the local disk because the network was a moment late is a
    // machine that does not get provisioned, and somebody walks to it.
    const STEP_MS: u64 = 250;
    const WINDOW_MS: u64 = 5_000;
    let mut waited = 0;
    while waited < WINDOW_MS {
        boot::stall(core::time::Duration::from_millis(STEP_MS));
        waited += STEP_MS;
        if available() || connect_all() {
            return Presence::BoundAfterWait(waited);
        }
    }

    Presence::Absent
}

/// `ConnectController` over every handle, then ask again.
fn connect_all() -> bool {
    if let Ok(handles) = boot::locate_handle_buffer(SearchType::AllHandles) {
        for h in handles.iter() {
            connect(h.as_ptr());
        }
    }
    available()
}

/// Put the network interfaces in the order worth trying them in.
///
/// This is the storage path. The NIC that matters is the one somebody wired for
/// it, and on a server that is not the first handle the firmware happens to
/// enumerate — it is the 25 GbE port with jumbo frames, sitting behind a 1 GbE
/// management port that has no cable in it.
///
/// Ranked by link first (a port with no media is last, not skipped: `SNP` is
/// allowed to not know, and a wrong guess that drops the only working interface
/// is worse than an extra attempt), then by descending MTU. MTU stands in for
/// speed because it is the honest signal available here — 9000 means somebody
/// configured that port for storage, 1500 means they did not — and because it
/// costs nothing: `EFI_TCP4.GetModeData` hands back the SNP mode, so no
/// `EFI_ADAPTER_INFORMATION_PROTOCOL` is needed, which is another optional
/// stack this binary will not depend on.
///
/// Returns `(index, handle, mtu, link_up)`, best first.
fn rank_interfaces(
    handles: &boot::HandleBuffer,
) -> Vec<(usize, uefi_raw::Handle, Option<u32>, bool)> {
    let mut out: Vec<(usize, uefi_raw::Handle, Option<u32>, bool)> = handles
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let (mtu, link, mac) = probe_interface(h.as_ptr());
            // Say what each interface *is*, before anything is tried. Without
            // this a failure to get an address is indistinguishable from a
            // cable in the wrong socket, and both look like "NO_MAPPING".
            uefi::println!(
                "      nic {i}: mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  mtu {}  link {}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                match mtu { Some(m) => m, None => 0 },
                if link { "UP" } else { "down/unknown" }
            );
            (i, h.as_ptr(), mtu, link)
        })
        .collect();
    // Link up before link unknown; then larger MTU first; then enumeration
    // order, so the result is stable when nothing distinguishes two ports.
    out.sort_by(|a, b| {
        b.3.cmp(&a.3)
            .then(b.2.unwrap_or(0).cmp(&a.2.unwrap_or(0)))
            .then(a.0.cmp(&b.0))
    });
    out
}

/// The MTU and link state of one interface, without configuring anything.
///
/// A child is created only to ask and destroyed immediately. `GetModeData` on
/// an unconfigured instance is allowed to refuse, which is why both answers are
/// optional rather than assumed.
fn probe_interface(sb_handle: uefi_raw::Handle) -> (Option<u32>, bool, [u8; 6]) {
    let Some(p) = handle_protocol(sb_handle, &TCP4_SERVICE_BINDING) else {
        return (None, false, [0u8; 6]);
    };
    let sb = p as *mut ServiceBinding;
    let mut child: uefi_raw::Handle = ptr::null_mut();
    if unsafe { ((*sb).create_child)(sb, &mut child) } != Status::SUCCESS {
        return (None, false, [0u8; 6]);
    }
    let result = match handle_protocol(child, &TCP4) {
        Some(t) => {
            let tcp = t as *mut Tcp4Protocol;
            let mut snp: NetworkMode = unsafe { core::mem::zeroed() };
            let st = unsafe {
                ((*tcp).get_mode_data)(
                    tcp,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut snp,
                )
            };
            if st == Status::SUCCESS {
                let mtu = (576..=65_535)
                    .contains(&snp.max_packet_size)
                    .then_some(snp.max_packet_size);
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&snp.permanent_address.0[..6]);
                (mtu, bool::from(snp.media_present), mac)
            } else {
                (None, false, [0u8; 6])
            }
        }
        None => (None, false, [0u8; 6]),
    };
    unsafe { let _ = ((*sb).destroy_child)(sb, child); };
    result
}

/// Tell the platform to run DHCP, rather than assuming it already has.
///
/// `configure` asks for `use_default_address`, which needs the IP4 driver to
/// *already hold* an address. Where the platform's policy is `STATIC` and no
/// address was ever set, that never becomes true: `Configure` answers
/// `NO_MAPPING` forever and waiting longer achieves nothing. Observed on a Dell
/// that reported `tcp4 : available` and then `no IP address after 20s`.
///
/// `EFI_IP4_CONFIG2_PROTOCOL` is where the platform keeps that policy, and it
/// is writable. Setting it to `DHCP` starts a lease; the caller's existing
/// retry loop then has something to wait *for* rather than waiting on
/// something nobody started.
///
/// Best-effort across every interface, because the one that matters is the one
/// with a cable in it and this cannot tell which that is. Returns how many
/// interfaces it actually switched, which is worth a console line: zero means
/// every interface was already on DHCP and the problem is elsewhere.
fn request_dhcp() -> usize {
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&IP4_CONFIG2)) else {
        return 0;
    };
    let mut switched = 0;
    for h in handles.iter() {
        let Some(p) = handle_protocol(h.as_ptr(), &IP4_CONFIG2) else {
            continue;
        };
        let cfg = p as *mut Ip4Config2Protocol;
        unsafe {
            // Only switch an interface that is actually on STATIC. Rewriting
            // DHCP over DHCP restarts a lease that may already be in flight.
            let mut policy = Ip4Config2Policy::STATIC;
            let mut size = core::mem::size_of::<Ip4Config2Policy>();
            let got = ((*cfg).get_data)(
                cfg,
                Ip4Config2DataType::POLICY,
                &mut size,
                &mut policy as *mut _ as *mut core::ffi::c_void,
            );
            if got == Status::SUCCESS && policy == Ip4Config2Policy::DHCP {
                continue;
            }
            let dhcp = Ip4Config2Policy::DHCP;
            let st = ((*cfg).set_data)(
                cfg,
                Ip4Config2DataType::POLICY,
                core::mem::size_of::<Ip4Config2Policy>(),
                &dhcp as *const _ as *const core::ffi::c_void,
            );
            if st == Status::SUCCESS {
                switched += 1;
            }
        }
    }
    switched
}

/// `ConnectController(handle, NULL, NULL, TRUE)` — bind every driver that will
/// bind, recursively. Failures are the normal case (most handles are not
/// controllers) and are not worth reporting.
fn connect(handle: uefi_raw::Handle) {
    unsafe {
        if let Some(st) = uefi::table::system_table_raw() {
            if let Some(bs) = st.as_ref().boot_services.as_ref() {
                let _ = (bs.connect_controller)(
                    handle,
                    ptr::null_mut(),
                    ptr::null(),
                    Boolean::TRUE,
                );
            }
        }
    }
}

/// What to tell an operator when there is no TCP4 stack to be had.
pub const NO_TCP4_ADVICE: &str = "EFI_TCP4 is not present. Binding the firmware's own drivers did not      produce it, and neither did waiting five seconds and binding again. Enable network boot / the NIC's UEFI PXE stack in setup so      the platform loads its TCP/IP drivers, then run tcp4probe on this model to      confirm. Note that Fedora's OVMF ships no upper network stack at all, so      this is expected under that emulator.";

pub struct Tcp4Socket {
    sb: *mut ServiceBinding,
    child: uefi_raw::Handle,
    tcp: *mut Tcp4Protocol,
    /// Left over from a receive that returned more than the caller wanted.
    pending: Vec<u8>,
    /// How long `pump` waits for one operation, in `POLL_INTERVAL` turns.
    turns: u32,
}

/// How long each `Poll` turn stalls. 500us matches the host client.
const POLL_INTERVAL_US: u64 = 500;

impl Tcp4Socket {
    /// Connect, waiting up to 30s on each operation — the NVMe path's budget.
    pub fn connect(remote: [u8; 4], port: u16) -> Result<Self, String> {
        Self::connect_within(remote, port, 30)
    }

    /// Connect with a per-operation timeout of `secs`.
    ///
    /// Anything on the *discovery* path wants a short one. A DNS server that is
    /// not there must cost a few seconds and then fall through, not thirty:
    /// discovery is an optimisation over the compiled defaults, and a boot path
    /// that spends half a minute proving the network is absent has already
    /// failed at its job. The attach itself keeps the long budget, because by
    /// then there is nothing to fall through to.
    /// Open a socket on whichever interface can actually reach the target.
    ///
    /// **Every** TCP4 service binding is tried, not the first one. A server has
    /// more than one NIC — a 1 GbE management port and a 25 GbE data port, say
    /// — and each carries its own network stack, so `handles.first()` was a
    /// coin flip. Landing on the port with no cable in it produces exactly the
    /// symptom seen on the Dell: `tcp4 : available`, because *some* interface
    /// has a stack, and then `NO_MAPPING` forever, because *that* one has no
    /// link and never will. No amount of waiting fixes a socket on the wrong
    /// NIC.
    ///
    /// Firmware will not say which port has the cable, so this asks the only
    /// question that matters — can you configure? — of each in turn, and takes
    /// the first that answers yes. DHCP is requested on every interface before
    /// the wait starts, so a lease is in flight on all of them while this runs
    /// rather than being started on one after twenty seconds of nothing.
    pub fn connect_within(remote: [u8; 4], port: u16, secs: u32) -> Result<Self, String> {
        let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&TCP4_SERVICE_BINDING))
            .map_err(|e| format!("no EFI_TCP4 service binding: {e:?}"))?;
        if handles.is_empty() {
            return Err("no TCP4 service binding handles".to_string());
        }
        // Order them before trying any: this is the storage path, so the NIC
        // that matters is the fast one. Link state first — a port with no cable
        // is not a candidate at all — then descending MTU, because a jumbo
        // interface is the one somebody configured for storage and a 1500
        // management port is the one they did not. MTU is a proxy for link
        // speed, and it is the right proxy here: it is what the transfer size
        // is derived from, it comes from the SNP mode this code already reads,
        // and it needs no EFI_ADAPTER_INFORMATION_PROTOCOL — another optional
        // stack this binary refuses to depend on.
        let order = rank_interfaces(&handles);
        if handles.len() > 1 {
            uefi::println!(
                "    {} network interfaces; trying the fastest first",
                handles.len()
            );
        }

        // Start a lease everywhere before waiting on anything. An interface
        // whose policy is STATIC never clears NO_MAPPING on its own.
        let switched = request_dhcp();
        if switched > 0 {
            uefi::println!("    asked {switched} interface(s) to run DHCP");
        }

        const STEP_MS: u64 = 500;
        let budget_ms = secs as u64 * 1000;
        let mut waited = 0u64;
        let mut last = String::from("no interface could be configured");

        loop {
            for &(i, handle, mtu, link) in order.iter() {
                match Self::open_on(handle, remote, port, secs) {
                    Ok(mut sock) => {
                        if handles.len() > 1 {
                            let m = match mtu {
                                Some(m) => m,
                                None => 0,
                            };
                            uefi::println!(
                                "    interface {i} answered (MTU {m}, link {})",
                                if link { "up" } else { "unknown" }
                            );
                        }
                        sock.do_connect()?;
                        return Ok(sock);
                    }
                    Err(e) => last = e,
                }
            }
            if waited >= budget_ms {
                return Err(format!(
                    "{last} — after {} s across {} interface(s). An address never \
arrived, so either nothing answers DHCP on the port with the cable in it, or \
the cable is in a port this firmware does not carry a stack for.",
                    budget_ms / 1000,
                    handles.len()
                ));
            }
            boot::stall(core::time::Duration::from_millis(STEP_MS));
            waited += STEP_MS;
        }
    }

    /// One attempt on one service binding. The child is destroyed on failure so
    /// a retry does not leak one per round.
    fn open_on(
        sb_handle: uefi_raw::Handle,
        remote: [u8; 4],
        port: u16,
        secs: u32,
    ) -> Result<Self, String> {
        let sb = handle_protocol(sb_handle, &TCP4_SERVICE_BINDING)
            .ok_or("could not open the TCP4 service binding")?
            as *mut ServiceBinding;

        let mut child: uefi_raw::Handle = ptr::null_mut();
        let st = unsafe { ((*sb).create_child)(sb, &mut child) };
        if st != Status::SUCCESS {
            return Err(format!("TCP4 CreateChild failed: {st:?}"));
        }

        let tcp = match handle_protocol(child, &TCP4) {
            Some(p) => p as *mut Tcp4Protocol,
            None => {
                unsafe { let _ = ((*sb).destroy_child)(sb, child); };
                return Err("EFI_TCP4 missing on the new child".to_string());
            }
        };

        let mut sock = Self {
            sb,
            child,
            tcp,
            pending: Vec::new(),
            turns: (secs as u64 * 1_000_000 / POLL_INTERVAL_US) as u32,
        };
        match sock.configure(remote, port, None) {
            Ok(()) => Ok(sock),
            Err(e) => {
                // The platform has no address on this interface. Rather than
                // wait on a DHCP client that may never have been started, run
                // one here and configure with the result explicitly — see
                // `dhcp4.rs`. Matched by MAC, because a DHCP4 binding and a
                // TCP4 binding on the same NIC are different handles.
                let (mac, mac_len) = sock.hw_address();
                if mac_len > 0 {
                    if let Some(lease) = crate::dhcp4::lease_for(&mac, mac_len) {
                        let [a, b, c, d] = lease.address;
                        let [m0, m1, m2, m3] = lease.subnet_mask;
                        let [g0, g1, g2, g3] = lease.router;
                        uefi::println!(
                            "    dhcp: leased {a}.{b}.{c}.{d}/{m0}.{m1}.{m2}.{m3} \
gw {g0}.{g1}.{g2}.{g3}"
                        );
                        return match sock.configure(remote, port, Some(lease)) {
                            Ok(()) => Ok(sock),
                            Err(e2) => Err(e2),
                        };
                    }
                }
                Err(e) // Drop destroys the child.
            }
        }
    }

    /// This interface's hardware address, and how many bytes of it are real.
    fn hw_address(&self) -> ([u8; 32], usize) {
        let mut snp: NetworkMode = unsafe { core::mem::zeroed() };
        let st = unsafe {
            ((*self.tcp).get_mode_data)(
                self.tcp,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut snp,
            )
        };
        if st != Status::SUCCESS {
            return ([0u8; 32], 0);
        }
        let len = snp.hw_address_size as usize;
        (snp.permanent_address.0, len.min(32))
    }

    /// Configure with whatever address this interface's stack already holds.
    ///
    /// One pass, no waiting: the caller owns the timing, because waiting here
    /// would spend the whole budget on the first interface and never reach the
    /// one with the cable in it.
    fn configure(
        &mut self,
        remote: [u8; 4],
        port: u16,
        lease: Option<crate::dhcp4::Lease>,
    ) -> Result<(), String> {
        // With a lease of our own the address is stated outright, so nothing
        // downstream depends on the platform's IP4 configuration having been
        // set up by somebody else.
        let (default, station, mask) = match lease {
            Some(l) => (Boolean::FALSE, l.address, l.subnet_mask),
            None => (Boolean::TRUE, [0, 0, 0, 0], [0, 0, 0, 0]),
        };
        let mut cfg = Tcp4ConfigData {
            type_of_service: 0,
            time_to_live: 64,
            access_point: Tcp4AccessPoint {
                use_default_address: default,
                station_address: Ipv4Address(station),
                subnet_mask: Ipv4Address(mask),
                station_port: 0,
                remote_address: Ipv4Address(remote),
                remote_port: port,
                active_flag: Boolean::TRUE,
            },
            control_option: ptr::null_mut::<Tcp4Option>(),
        };
        match unsafe { ((*self.tcp).configure)(self.tcp, &mut cfg) } {
            Status::SUCCESS => Ok(()),
            Status::NO_MAPPING => Err("no address on this interface (NO_MAPPING)".to_string()),
            other => Err(format!("TCP4 Configure failed: {other:?}")),
        }
    }

    fn do_connect(&mut self) -> Result<(), String> {
        let event = new_event()?;
        let mut token = Tcp4ConnectionToken {
            completion_token: Tcp4CompletionToken {
                event,
                status: Status::SUCCESS,
            },
        };
        let st = unsafe { ((*self.tcp).connect)(self.tcp, &mut token) };
        if st != Status::SUCCESS {
            close_event(event);
            return Err(format!("TCP4 Connect rejected: {st:?}"));
        }
        let r = self.pump(&token.completion_token, "connect");
        close_event(event);
        r
    }

    /// The MTU of the interface this connection is running over, in bytes.
    ///
    /// This is the number the NVMe layer needs in order to stop guessing: the
    /// size of one command is the size of the C2HData PDU that comes back, and
    /// whether that fits a frame is the whole question. `CHUNK` was a constant
    /// that had to be hand-edited every time the portal moved between a 1500
    /// and a 9000 network, which is a bug waiting for someone to forget.
    ///
    /// Read out of `SnpModeData` rather than `Ip4ModeData`, for two reasons:
    /// SNP's `max_packet_size` is the link MTU excluding the media header,
    /// which is exactly the budget the frame arithmetic wants, and asking for
    /// `Ip4ModeData` makes the IP4 driver allocate a route table, a group
    /// table and an ICMP type list from the boot-services pool that the caller
    /// then owns. There is nothing in them we need.
    ///
    /// `None` means the stack would not say, and the caller should assume
    /// nothing — not that the path is small.
    pub fn link_mtu(&self) -> Option<u32> {
        // Zeroed rather than uninitialised: firmware that ignores the argument
        // must leave us reading zero, which the plausibility check rejects,
        // rather than reading a stack MTU that was never written.
        let mut snp: NetworkMode = unsafe { core::mem::zeroed() };
        let st = unsafe {
            ((*self.tcp).get_mode_data)(
                self.tcp,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut snp,
            )
        };
        if st != Status::SUCCESS {
            return None;
        }
        // Ethernet is 1500 and jumbo is 9000; anything outside this range is a
        // field that was not filled in, not a network.
        (576..=65_535).contains(&snp.max_packet_size).then_some(snp.max_packet_size)
    }

    /// Drive the stack until a token retires.
    ///
    /// `Poll` is what gives the TCP driver cycles; without it nothing ever
    /// completes and this waits forever.
    fn pump(&self, token: &Tcp4CompletionToken, what: &str) -> Result<(), String> {
        for _ in 0..self.turns {
            unsafe { let _ = ((*self.tcp).poll)(self.tcp); };
            if signalled(token.event) {
                return if token.status == Status::SUCCESS {
                    Ok(())
                } else {
                    Err(format!("{what} failed: {:?}", token.status))
                };
            }
            boot::stall(core::time::Duration::from_micros(POLL_INTERVAL_US));
        }
        Err(format!("{what} timed out"))
    }

    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }
        let event = new_event()?;
        let mut tx = TxData1 {
            push: Boolean::TRUE,
            urgent: Boolean::FALSE,
            data_length: data.len() as u32,
            fragment_count: 1,
            fragment_table: [Tcp4FragmentData {
                fragment_length: data.len() as u32,
                fragment_buf: data.as_ptr() as *mut u8,
            }],
        };
        let mut token = Tcp4IoToken {
            completion_token: Tcp4CompletionToken {
                event,
                status: Status::SUCCESS,
            },
            packet: Tcp4Packet {
                tx_data: &mut tx as *mut TxData1 as *mut Tcp4TransmitData,
            },
        };
        let st = unsafe { ((*self.tcp).transmit)(self.tcp, &mut token) };
        if st != Status::SUCCESS {
            close_event(event);
            return Err(format!("TCP4 Transmit rejected: {st:?}"));
        }
        let r = self.pump(&token.completion_token, "transmit");
        close_event(event);
        r
    }

    /// One receive into a fresh buffer. Returns what the stack handed over,
    /// which may be less than asked for.
    fn recv_some(&mut self, want: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; want.clamp(1, 65536)];
        let event = new_event()?;
        let mut rx = RxData1 {
            urgent: Boolean::FALSE,
            data_length: buf.len() as u32,
            fragment_count: 1,
            fragment_table: [Tcp4FragmentData {
                fragment_length: buf.len() as u32,
                fragment_buf: buf.as_mut_ptr(),
            }],
        };
        let mut token = Tcp4IoToken {
            completion_token: Tcp4CompletionToken {
                event,
                status: Status::SUCCESS,
            },
            packet: Tcp4Packet {
                rx_data: &mut rx as *mut RxData1 as *mut Tcp4ReceiveData,
            },
        };
        let st = unsafe { ((*self.tcp).receive)(self.tcp, &mut token) };
        if st != Status::SUCCESS {
            close_event(event);
            return Err(format!("TCP4 Receive rejected: {st:?}"));
        }
        let r = self.pump(&token.completion_token, "receive");
        close_event(event);
        r?;
        let n = (rx.data_length as usize).min(buf.len());
        buf.truncate(n);
        Ok(buf)
    }

    /// Read exactly `n` bytes. Every PDU header and payload length in NVMe/TCP
    /// is known in advance, so this is the primitive that layer wants.
    pub fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(n);
        if !self.pending.is_empty() {
            let take = self.pending.len().min(n);
            out.extend_from_slice(&self.pending[..take]);
            self.pending.drain(..take);
        }
        while out.len() < n {
            let chunk = self.recv_some(n - out.len())?;
            if chunk.is_empty() {
                return Err("connection closed mid-read".to_string());
            }
            out.extend_from_slice(&chunk);
        }
        if out.len() > n {
            self.pending.extend_from_slice(&out[n..]);
            out.truncate(n);
        }
        Ok(out)
    }

    /// Read until the peer closes. Used for one HTTP response; NVMe reads
    /// exact lengths instead.
    pub fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, String> {
        let mut out = core::mem::take(&mut self.pending);
        loop {
            match self.recv_some(8192) {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => {
                    out.extend_from_slice(&chunk);
                    if out.len() >= limit {
                        break;
                    }
                }
                // A closed connection arrives as an error status, which is the
                // normal end of a `Connection: close` response.
                Err(_) => break,
            }
        }
        Ok(out)
    }
}

impl Drop for Tcp4Socket {
    fn drop(&mut self) {
        unsafe {
            let _ = ((*self.tcp).configure)(self.tcp, ptr::null_mut());
            let _ = ((*self.sb).destroy_child)(self.sb, self.child);
        }
    }
}
