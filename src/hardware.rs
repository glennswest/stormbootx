//! What the firmware inventory (#4, #20) is collected from: the NICs the
//! stack opened, PCI I/O for storage controllers, BlockIO for disks, and the
//! SMBIOS table. `inventory.rs` decides what goes in and renders it.
//!
//! Worked out once, at the first claim: after the network is up (so every
//! NIC has its driver, the media's included) and **before** `blockio::publish`,
//! so the namespace this boot attaches is never reported as the machine's own
//! disk. A disk an earlier start left published is skipped for the same
//! reason.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr;

use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::proto::device_path::text::{AllowShortcuts, DisplayOnly};
use uefi::proto::device_path::DevicePath;
use uefi::proto::media::block::BlockIO;
use uefi::{guid, Guid, Identify, Status};
use uefi_raw::protocol::device_path::DevicePathProtocol;

use crate::inventory::{self, Controller, Disk, Nic};

const PCI_IO: Guid = guid!("4cf5b200-68b8-4ca5-9eec-b23e3f50029a");
const DEVICE_PATH: Guid = guid!("09576e91-6d3f-11d2-8e39-00a0c969723b");

struct Cached(core::cell::UnsafeCell<Option<String>>);
// Boot services are single-threaded.
unsafe impl Sync for Cached {}
static INVENTORY: Cached = Cached(core::cell::UnsafeCell::new(None));

/// The inventory as the claim's `"inventory"` object, collected on the first
/// call and said on the console then.
pub fn json() -> String {
    let slot = unsafe { &mut *INVENTORY.0.get() };
    if let Some(j) = slot {
        return j.clone();
    }
    let nics = nics();
    let storage = storage();
    let disks = disks();
    let smbios = crate::smbios::table_bytes().map(inventory::parse_smbios).unwrap_or_default();
    let j = inventory::render(&nics, &storage, &disks, &smbios);
    uefi::println!(
        "inventory   : {} NIC(s), {} storage controller(s), {} disk(s), {}; {} bytes with the claim",
        nics.len(),
        storage.len(),
        disks.len(),
        if smbios.bmc { "a BMC (no CPU or memory sent)" } else { "no BMC (CPU and memory sent)" },
        j.len()
    );
    *slot = Some(j.clone());
    j
}

fn text(dp: &DevicePath) -> Option<String> {
    dp.to_string16(DisplayOnly(false), AllowShortcuts(false)).ok().map(|s| format!("{s}"))
}

/// The PCI function a handle's device path runs through, if any.
fn pci_of(h: uefi_raw::Handle) -> Option<uefi_raw::Handle> {
    let bs = unsafe { uefi::table::system_table_raw()?.as_ref().boot_services.as_ref()? };
    let mut dp = crate::net::handle_protocol(h, &DEVICE_PATH)? as *const DevicePathProtocol;
    let mut pci: uefi_raw::Handle = ptr::null_mut();
    let st = unsafe { (bs.locate_device_path)(&PCI_IO, &mut dp, &mut pci) };
    (st == Status::SUCCESS && !pci.is_null()).then_some(pci)
}

fn nics() -> Vec<Nic> {
    crate::net::nic_summary()
        .into_iter()
        .map(|(h, mac, link)| {
            let driver = crate::snpwatch::driver_of(h);
            // Every driver the media carries is a stormnic one (#91).
            let media_driver = driver.as_deref().is_some_and(|d| d.to_ascii_lowercase().contains("stormnic"));
            let pci = pci_of(h).and_then(crate::drivers::pci_info);
            Nic {
                mac,
                link,
                driver,
                media_driver,
                pci: pci.as_ref().map(|(at, _, _)| at.clone()),
                id: pci.map(|(_, id, _)| id),
            }
        })
        .collect()
}

/// Every mass-storage controller (PCI class 01) and the driver bound to it,
/// if any: one with no UEFI driver is exactly what a BMC won't show either.
fn storage() -> Vec<Controller> {
    crate::drivers::pci_functions()
        .into_iter()
        .filter(|(_, _, _, class)| class >> 24 == 0x01)
        .map(|(h, pci, id, class)| Controller {
            pci,
            id,
            class: ((class >> 24) as u8, (class >> 16) as u8, (class >> 8) as u8),
            driver: crate::snpwatch::pci_driver(h),
        })
        .collect()
}

/// Every whole disk with a BlockIO, the boot medium marked; never one a
/// stormbootx published.
fn disks() -> Vec<Disk> {
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&BlockIO::GUID)) else {
        return Vec::new();
    };
    let medium = crate::config::boot_volume().and_then(|h| crate::blockio::device_path_of(h.as_ptr()));
    let mut out = Vec::new();
    for h in handles.iter() {
        if crate::blockio::is_published(h.as_ptr()) {
            continue;
        }
        let Ok(bio) = (unsafe {
            boot::open_protocol::<BlockIO>(
                OpenProtocolParams { handle: *h, agent: boot::image_handle(), controller: None },
                OpenProtocolAttributes::GetProtocol,
            )
        }) else {
            continue;
        };
        let m = bio.media();
        if m.is_logical_partition() {
            continue;
        }
        let dp = crate::blockio::device_path_of(h.as_ptr());
        let is_medium = match (dp, medium) {
            (Some(d), Some(v)) => {
                d.node_iter().count() <= v.node_iter().count() && d.node_iter().zip(v.node_iter()).all(|(a, b)| a == b)
            }
            _ => false,
        };
        out.push(Disk {
            path: dp.and_then(text),
            blocks: if m.is_media_present() { m.last_block() + 1 } else { 0 },
            block_size: m.block_size(),
            removable: m.is_removable_media(),
            medium: is_medium,
        });
    }
    out
}
