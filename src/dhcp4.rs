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

use uefi::boot::{self, SearchType};
use uefi::{Guid, guid};
use uefi_raw::Status;
use uefi_raw::protocol::network::dhcp4::{Dhcp4ConfigData, Dhcp4ModeData, Dhcp4Protocol, Dhcp4State};

use crate::tcp4::{handle_protocol, ServiceBinding};

const DHCP4: Guid = guid!("8a219718-4ef5-4761-91c8-c0f04bda9e56");
const DHCP4_SERVICE_BINDING: Guid = guid!("9d9a39d8-bd42-4a73-a4d5-8ee94be11380");

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
