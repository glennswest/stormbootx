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
    pub fn connect_within(remote: [u8; 4], port: u16, secs: u32) -> Result<Self, String> {
        let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&TCP4_SERVICE_BINDING))
            .map_err(|e| format!("no EFI_TCP4 service binding: {e:?}"))?;
        let sb_handle = *handles.first().ok_or("no TCP4 service binding handles")?;

        let sb = handle_protocol(sb_handle.as_ptr(), &TCP4_SERVICE_BINDING)
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
        sock.configure(remote, port)?;
        sock.do_connect()?;
        Ok(sock)
    }

    /// Configure with whatever address the firmware's stack already holds.
    ///
    /// `NO_MAPPING` means the stack is up but has no address yet — DHCP has
    /// not finished. On a cold boot the lease often lands a second or two
    /// after an application starts, so this is a timing condition to wait out,
    /// not a failure to report.
    fn configure(&mut self, remote: [u8; 4], port: u16) -> Result<(), String> {
        for attempt in 0..40 {
            let mut cfg = Tcp4ConfigData {
                type_of_service: 0,
                time_to_live: 64,
                access_point: Tcp4AccessPoint {
                    use_default_address: Boolean::TRUE,
                    station_address: Ipv4Address([0, 0, 0, 0]),
                    subnet_mask: Ipv4Address([0, 0, 0, 0]),
                    station_port: 0,
                    remote_address: Ipv4Address(remote),
                    remote_port: port,
                    active_flag: Boolean::TRUE,
                },
                control_option: ptr::null_mut::<Tcp4Option>(),
            };
            match unsafe { ((*self.tcp).configure)(self.tcp, &mut cfg) } {
                Status::SUCCESS => return Ok(()),
                Status::NO_MAPPING => {
                    if attempt == 0 {
                        // Do not just wait: NO_MAPPING on an interface whose
                        // policy is STATIC never clears, because nobody ever
                        // started a lease. Ask for one, then wait.
                        match request_dhcp() {
                            0 => uefi::println!(
                                "    waiting for the firmware's DHCP lease..."
                            ),
                            n => uefi::println!(
                                "    no address yet; asked {n} interface(s) to run DHCP"
                            ),
                        }
                    }
                    boot::stall(core::time::Duration::from_millis(500));
                }
                other => return Err(format!("TCP4 Configure failed: {other:?}")),
            }
        }
        Err("no IP address after 20s (NO_MAPPING). The interface was asked to run \
DHCP and still has no address, so either nothing answered the request or the \
port is not on a network that serves one.".to_string())
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
