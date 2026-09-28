//! Getting an address ourselves, instead of hoping firmware already did.
//!
//! `EFI_TCP4.Configure` with `use_default_address` needs the platform's IP4
//! driver to *already hold* an address, which means somebody else's DHCP client
//! ran first. On a server that is not a given: the policy may be `STATIC`, the
//! platform may only run DHCP as part of a PXE attempt it was never asked to
//! make, and either way the symptom is `NO_MAPPING` forever with nothing to
//! wait for. Observed on a Dell that reported `tcp4 : available` and then no
//! address after twenty seconds.
//!
//! So run it here. `EFI_DHCP4_PROTOCOL` is a client we drive directly: create a
//! child, `Configure`, `Start`, and read the lease back out of the mode data.
//! The address then goes into `Tcp4ConfigData` as an explicit
//! `station_address`, so nothing downstream depends on the platform's own IP4
//! configuration having been right.
//!
//! **Matched by MAC, not by handle order.** A DHCP4 service binding and a TCP4
//! service binding on the same NIC are different handles, and a server has
//! several of each. `EFI_DHCP4.GetModeData` reports `client_mac_address` and
//! `EFI_TCP4.GetModeData` reports the SNP mode, so the MAC is the one thing
//! that identifies the same wire from both ends.
//!
//! This is an optional protocol stack, which this binary otherwise refuses to
//! depend on — so it is a **fallback**, never the first move. The platform's
//! own address is used when it has one; this runs only when it does not, and a
//! machine whose firmware carries no DHCP4 is exactly as well off as before.

use alloc::vec::Vec;

use uefi::boot::{self, SearchType};
use uefi::{Guid, guid};
use uefi_raw::Status;
use uefi_raw::protocol::network::dhcp4::{Dhcp4ConfigData, Dhcp4ModeData, Dhcp4Protocol, Dhcp4State};

use crate::tcp4::{handle_protocol, ServiceBinding};

const DHCP4: Guid = guid!("8a219718-4ef5-4761-91c8-c0f04bda9e56");
const DHCP4_SERVICE_BINDING: Guid = guid!("9d9a39d8-bd42-4a73-a4d5-8ee94be11380");

/// Replies from leases this binary ran itself, by MAC: `(mac, message)`.
///
/// Kept because asking the firmware for them again is not reliable: on
/// server1 (AMI Aptio 4, UEFI 2.3.1) the address came from `lease_for` and
/// `reply_for`'s fresh child found no reply, so the machine had no DNS server
/// to ask for its name (#26). UEFI boot services run on one thread and nothing
/// here is reentrant, which is what makes the `Sync` below honest.
struct OwnReplies(core::cell::UnsafeCell<Vec<(Vec<u8>, Vec<u8>)>>);
unsafe impl Sync for OwnReplies {}
static OWN_REPLIES: OwnReplies = OwnReplies(core::cell::UnsafeCell::new(Vec::new()));

fn own_reply(mac: &[u8]) -> Option<Vec<u8>> {
    let all = unsafe { &*OWN_REPLIES.0.get() };
    all.iter().find(|(m, _)| m[..] == *mac).map(|(_, r)| r.clone())
}

fn keep_reply(mac: &[u8], reply: Vec<u8>) {
    let all = unsafe { &mut *OWN_REPLIES.0.get() };
    all.retain(|(m, _)| m[..] != *mac);
    all.push((mac.to_vec(), reply));
}

/// The message (header onwards) a `Dhcp4ModeData.reply_packet` points at.
/// `length` counts the message from the header on; the packet struct is
/// packed, so the header starts 8 bytes in.
fn packet_message(packet: *const u8) -> Option<Vec<u8>> {
    if packet.is_null() {
        return None;
    }
    let length = unsafe { core::ptr::read_unaligned(packet.add(4) as *const u32) } as usize;
    (240..=65_536).contains(&length)
        .then(|| unsafe { core::slice::from_raw_parts(packet.add(8), length) }.to_vec())
}

/// What a completed DHCP exchange yielded.
#[derive(Debug, Clone, Copy)]
pub struct Lease {
    pub address: [u8; 4],
    pub subnet_mask: [u8; 4],
    pub router: [u8; 4],
}

/// Run DHCP on the interface whose MAC matches `mac`, and return its lease.
///
/// `mac_len` is the hardware address size the SNP reported — comparing all 32
/// bytes of an `EFI_MAC_ADDRESS` would compare padding that no one promises is
/// zeroed.
pub fn lease_for(mac: &[u8], mac_len: usize) -> Option<Lease> {
    let handles = match boot::locate_handle_buffer(SearchType::ByProtocol(&DHCP4_SERVICE_BINDING)) {
        Ok(h) => h,
        Err(_) => {
            // Say so rather than failing silently: "this firmware has no DHCP
            // client" and "DHCP ran and nobody answered" are different
            // problems and only one of them is ours.
            uefi::println!("      dhcp: firmware carries no EFI_DHCP4");
            return None;
        }
    };
    uefi::println!("      dhcp: {} client(s); running one on this nic", handles.len());
    for h in handles.iter() {
        if let Some(lease) = try_one(h.as_ptr(), mac, mac_len) {
            return Some(lease);
        }
    }
    uefi::println!("      dhcp: no client reached BOUND — nothing answered");
    None
}

/// The DHCP reply that bound the interface with MAC `mac`, as the raw
/// BOOTP/DHCP message (header onwards), for its options (#23).
///
/// Whoever ran DHCP — the platform's IP4 configuration or `lease_for` above —
/// the lease belongs to the NIC's DHCP4 *service*, and EDK2's `GetModeData`
/// reports the service's state and selected reply to any child, configured or
/// not. So a fresh child is made only to ask, and destroyed: it was never
/// configured, so destroying it releases nothing. Firmware that reports no
/// reply, or a state other than bound, gives `None`, never a guess.
pub fn reply_for(mac: &[u8], mac_len: usize) -> Option<Vec<u8>> {
    if let Some(r) = (mac_len > 0 && mac_len <= mac.len()).then(|| own_reply(&mac[..mac_len])).flatten() {
        return Some(r);
    }
    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&DHCP4_SERVICE_BINDING)).ok()?;
    for h in handles.iter() {
        let Some(sb) = handle_protocol(h.as_ptr(), &DHCP4_SERVICE_BINDING) else { continue };
        let sb = sb as *mut ServiceBinding;
        let mut child: uefi_raw::Handle = core::ptr::null_mut();
        if unsafe { ((*sb).create_child)(sb, &mut child) } != Status::SUCCESS {
            continue;
        }
        let reply = handle_protocol(child, &DHCP4).and_then(|p| {
            let dhcp = p as *mut Dhcp4Protocol;
            let mut mode: Dhcp4ModeData = unsafe { core::mem::zeroed() };
            let ok = unsafe { ((*dhcp).get_mode_data)(dhcp, &mut mode) } == Status::SUCCESS
                && mac_len > 0
                && mac_len <= mode.client_mac_address.0.len()
                && mode.client_mac_address.0[..mac_len] == mac[..mac_len]
                && matches!(
                    mode.state,
                    Dhcp4State::BOUND | Dhcp4State::RENEWING | Dhcp4State::REBINDING
                )
                && !mode.reply_packet.is_null();
            if !ok {
                return None;
            }
            packet_message(mode.reply_packet as *const u8)
        });
        unsafe { let _ = ((*sb).destroy_child)(sb, child); };
        if reply.is_some() {
            return reply;
        }
    }
    None
}

fn try_one(sb_handle: uefi_raw::Handle, mac: &[u8], mac_len: usize) -> Option<Lease> {
    let sb = handle_protocol(sb_handle, &DHCP4_SERVICE_BINDING)? as *mut ServiceBinding;
    let mut child: uefi_raw::Handle = core::ptr::null_mut();
    if unsafe { ((*sb).create_child)(sb, &mut child) } != Status::SUCCESS {
        return None;
    }

    let Some(p) = handle_protocol(child, &DHCP4) else {
        unsafe { let _ = ((*sb).destroy_child)(sb, child); };
        return None;
    };
    let dhcp = p as *mut Dhcp4Protocol;

    // Is this the wire we are asking about?
    let mut mode: Dhcp4ModeData = unsafe { core::mem::zeroed() };
    if unsafe { ((*dhcp).get_mode_data)(dhcp, &mut mode) } != Status::SUCCESS
        || mac_len == 0
        || mac_len > mode.client_mac_address.0.len()
        || mode.client_mac_address.0[..mac_len] != mac[..mac_len]
    {
        unsafe { let _ = ((*sb).destroy_child)(sb, child); };
        return None;
    }

    // Zeroed config takes the driver's own defaults for try counts and
    // timeouts, which is what we want: firmware knows its own link better than
    // a number invented here would.
    let cfg: Dhcp4ConfigData = unsafe { core::mem::zeroed() };
    let st = unsafe { ((*dhcp).configure)(dhcp, &cfg) };
    if st != Status::SUCCESS && st != Status::ALREADY_STARTED {
        unsafe { let _ = ((*sb).destroy_child)(sb, child); };
        return None;
    }

    // A null completion event makes Start blocking, which is what a boot path
    // wants: there is nothing else to do until this answers.
    let st = unsafe { ((*dhcp).start)(dhcp, core::ptr::null_mut()) };
    if st != Status::SUCCESS && st != Status::ALREADY_STARTED {
        unsafe { let _ = ((*sb).destroy_child)(sb, child); };
        return None;
    }

    let mut mode: Dhcp4ModeData = unsafe { core::mem::zeroed() };
    if unsafe { ((*dhcp).get_mode_data)(dhcp, &mut mode) } != Status::SUCCESS
        || mode.state != Dhcp4State::BOUND
    {
        unsafe { let _ = ((*sb).destroy_child)(sb, child); };
        return None;
    }

    if let Some(r) = packet_message(mode.reply_packet as *const u8) {
        keep_reply(&mac[..mac_len], r);
    }

    // The child is deliberately left alive. Destroying it stops the DHCP
    // instance, and a driver is entitled to release the lease when that
    // happens — which would hand back the address a moment before it is used.
    // It is freed when the firmware tears everything down at ExitBootServices,
    // which on this path is seconds away.
    Some(Lease {
        address: mode.client_address.0,
        subnet_mask: mode.subnet_mask.0,
        router: mode.router_address.0,
    })
}
