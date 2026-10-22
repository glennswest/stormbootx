//! What the firmware inventory (#4, #20, #167) is collected from: the NICs
//! the stack opened, PCI I/O for storage controllers, BlockIO for disks and
//! what each says about itself (`EFI_DISK_INFO`, NVMe pass-thru Identify
//! Controller, `EFI_SCSI_IO` VPD pages), each disk's own GPT or MBR, and the
//! SMBIOS table. All of it read-only. `inventory.rs` decides what goes in,
//! parses and renders it.
//!
//! Worked out once, at the first claim: after the network is up (so every
//! NIC has its driver, the media's included) and **before** `blockio::publish`,
//! so the namespace this boot attaches is never reported as the machine's own
//! disk. A disk an earlier start left published is skipped for the same
//! reason.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::proto::device_path::text::{AllowShortcuts, DisplayOnly};
use uefi::proto::device_path::DevicePath;
use uefi::proto::media::block::BlockIO;
use uefi::{guid, Guid, Identify, Status};
use uefi_raw::protocol::device_path::DevicePathProtocol;

use crate::esp;
use crate::inventory::{self, Controller, Disk, DriveId, Nic, Part};

const PCI_IO: Guid = guid!("4cf5b200-68b8-4ca5-9eec-b23e3f50029a");
const DEVICE_PATH: Guid = guid!("09576e91-6d3f-11d2-8e39-00a0c969723b");
const DISK_INFO: Guid = guid!("d432a67f-14dc-484b-b3bb-3f0291849327");
const NVME_PASS_THRU: Guid = guid!("52c78312-8edc-4233-98f2-1a1aa5e388a5");
const SCSI_IO: Guid = guid!("932f47e6-2362-4002-803e-3cd54b138f85");

/// `EFI_DISK_INFO`'s `Interface` GUIDs: which bus the disk is on.
const DI_IDE: Guid = guid!("5e948fe3-26d3-42b5-af17-610287188dec");
const DI_AHCI: Guid = guid!("9e498932-4abc-45af-a34d-0247787be7c6");
const DI_SCSI: Guid = guid!("08f74baa-ea36-41d9-9521-21a70f8780bc");
const DI_USB: Guid = guid!("cb871572-c11a-47b5-b492-675eafa77727");
const DI_NVME: Guid = guid!("3ab14680-5d3f-4a4d-bcdc-cc380018c7f7");
const DI_UFS: Guid = guid!("4b3029cc-6b98-47fb-bc96-76dcb80441f0");
const DI_SD: Guid = guid!("8deec992-d39c-4a5c-ab6b-986e14242b9d");
const DI_EMMC: Guid = guid!("3b05c32d-7a54-4a12-a24b-9a5b6bba5c4a");

/// `EFI_DISK_INFO_PROTOCOL` (PI spec, vol. 5).
// Fields the firmware reads, or placeholders of a pointer's size.
#[allow(dead_code)]
#[repr(C)]
struct DiskInfoProtocol {
    interface: Guid,
    inquiry: unsafe extern "efiapi" fn(*mut DiskInfoProtocol, *mut c_void, *mut u32) -> Status,
    identify: unsafe extern "efiapi" fn(*mut DiskInfoProtocol, *mut c_void, *mut u32) -> Status,
    sense_data: usize,
    which_ide: usize,
}

/// `EFI_NVM_EXPRESS_PASS_THRU_PROTOCOL` (UEFI 2.5+).
// Fields the firmware reads, or placeholders of a pointer's size.
#[allow(dead_code)]
#[repr(C)]
struct NvmeMode {
    attributes: u32,
    io_align: u32,
    version: u32,
}

// Fields the firmware reads, or placeholders of a pointer's size.
#[allow(dead_code)]
#[repr(C)]
struct NvmePassThru {
    mode: *const NvmeMode,
    pass_thru: unsafe extern "efiapi" fn(*mut NvmePassThru, u32, *mut NvmePacket, *mut c_void) -> Status,
    get_next_namespace: usize,
    build_device_path: usize,
    get_namespace: usize,
}

// Fields the firmware reads, or placeholders of a pointer's size.
#[allow(dead_code)]
#[repr(C)]
struct NvmePacket {
    /// 100 ns units; 0 would wait forever.
    timeout: u64,
    buffer: *mut c_void,
    length: u32,
    metadata: *mut c_void,
    metadata_length: u32,
    queue_type: u8,
    cmd: *mut NvmeCommand,
    completion: *mut NvmeCompletion,
}

// Fields the firmware reads, or placeholders of a pointer's size.
#[allow(dead_code)]
#[repr(C)]
#[derive(Default)]
struct NvmeCommand {
    /// Opcode in bits 0-7.
    cdw0: u32,
    /// Which of CDW2, 3, 10-15 are valid: CDW10 is bit 2.
    flags: u8,
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
}

// Fields the firmware reads, or placeholders of a pointer's size.
#[allow(dead_code)]
#[repr(C)]
#[derive(Default)]
struct NvmeCompletion {
    dw: [u32; 4],
}

/// `EFI_SCSI_IO_PROTOCOL` (UEFI 2.x, 15.4).
// Fields the firmware reads, or placeholders of a pointer's size.
#[allow(dead_code)]
#[repr(C)]
struct ScsiIoProtocol {
    get_device_type: usize,
    get_device_location: usize,
    reset_bus: usize,
    reset_device: usize,
    execute: unsafe extern "efiapi" fn(*mut ScsiIoProtocol, *mut ScsiPacket, *mut c_void) -> Status,
    io_align: u32,
}

// Fields the firmware reads, or placeholders of a pointer's size.
#[allow(dead_code)]
#[repr(C)]
struct ScsiPacket {
    /// 100 ns units.
    timeout: u64,
    in_data: *mut c_void,
    out_data: *mut c_void,
    sense: *mut c_void,
    cdb: *mut c_void,
    in_length: u32,
    out_length: u32,
    cdb_length: u8,
    direction: u8,
    host_status: u8,
    target_status: u8,
    sense_length: u8,
}

/// Two seconds, in the pass-thru protocols' 100 ns units.
const TWO_SECONDS: u64 = 20_000_000;

/// A 4096-byte buffer on a 4096-byte boundary, which satisfies any
/// `IoAlign` up to a page (the protocols' DMA wants it).
struct PageBuf(Vec<u8>);

impl PageBuf {
    fn new() -> Self {
        PageBuf(vec![0u8; 8192])
    }
    fn page(&mut self) -> &mut [u8] {
        let off = self.0.as_ptr().align_offset(4096);
        &mut self.0[off..off + 4096]
    }
}

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
        if smbios.bmc { "a BMC" } else { "no BMC" },
        j.len()
    );
    // The same list on the console, so it reaches the log collector (#162)
    // when nothing else can show it (#167).
    let named = |n: &inventory::Named| {
        [&n.vendor, &n.product].iter().filter_map(|x| x.as_deref()).collect::<Vec<_>>().join(" ")
    };
    let system = named(&smbios.system);
    if !system.is_empty() || smbios.system.serial.is_some() {
        uefi::println!(
            "  system    : {}{}",
            if system.is_empty() { "?" } else { &system },
            smbios.system.serial.as_deref().map(|s| format!("  serial {s}")).unwrap_or_default()
        );
    }
    let board = named(&smbios.board);
    if !board.is_empty() && board != system {
        uefi::println!("  board     : {board}");
    }
    let fw = named(&inventory::Named { product: smbios.firmware.version.clone(), ..smbios.firmware.clone() });
    if !fw.is_empty() {
        uefi::println!(
            "  firmware  : {fw}{}",
            smbios.firmware_date.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()
        );
    }
    for (i, d) in disks.iter().enumerate() {
        uefi::println!("  disk {i:<5}: {}", inventory::disk_line(d));
        if let Some(p) = &d.path {
            uefi::println!("              {p}");
        }
        if let Some(p) = inventory::partitions_line(d) {
            uefi::println!("              {p}");
        }
    }
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
        .map(|(h, mac, link, mtu)| {
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
                mtu,
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
/// stormbootx published. Each with what it says about itself and what its
/// partition table holds (#167).
fn disks() -> Vec<Disk> {
    // A firmware that boots fast connects only what its boot order names, so
    // a drive on another controller has no BlockIO yet. Connect every
    // mass-storage controller (PCI class 01), recursively, so the count is
    // the machine's; one already connected costs nothing.
    for (h, _, _, class) in crate::drivers::pci_functions() {
        if class >> 24 == 0x01 {
            crate::net::connect(h);
        }
    }
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&BlockIO::GUID)) else {
        return Vec::new();
    };
    let medium = crate::config::boot_volume().and_then(|h| crate::blockio::device_path_of(h.as_ptr()));
    let mut out = Vec::new();
    let mut buf = PageBuf::new();
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
        let path = dp.and_then(text);
        let (bus, id) = identify(h.as_ptr(), path.as_deref(), &mut buf);
        let present = m.is_media_present() && m.block_size() >= 512;
        let mut disk = Disk {
            path,
            blocks: if m.is_media_present() { m.last_block() + 1 } else { 0 },
            block_size: m.block_size(),
            removable: m.is_removable_media(),
            medium: is_medium,
            bus,
            id,
            ..Disk::default()
        };
        if present {
            let mut bd = crate::espboot::BlockDisk {
                bio: &bio,
                media_id: m.media_id(),
                block: m.block_size() as u64,
                bounce: Vec::new(),
            };
            table(&mut bd, &mut disk);
        }
        out.push(disk);
    }
    out
}

/// Which bus a disk is on and what it says about itself: `EFI_DISK_INFO`
/// first (ATA IDENTIFY, SCSI or USB INQUIRY, NVMe Identify Namespace), then
/// NVMe Identify Controller through the controller's pass-thru, and SCSI VPD
/// pages through `EFI_SCSI_IO` on the disk's own handle.
fn identify(h: uefi_raw::Handle, path: Option<&str>, buf: &mut PageBuf) -> (Option<&'static str>, DriveId) {
    let mut id = DriveId::default();
    let mut bus = None;
    if let Some(di) = crate::net::handle_protocol(h, &DISK_INFO).map(|p| p as *mut DiskInfoProtocol) {
        let iface = unsafe { (*di).interface };
        const BUSES: [(Guid, &str); 8] = [
            (DI_NVME, "nvme"),
            (DI_AHCI, "sata"),
            (DI_IDE, "ata"),
            (DI_SCSI, "scsi"),
            (DI_USB, "usb"),
            (DI_UFS, "ufs"),
            (DI_SD, "sd"),
            (DI_EMMC, "sd"),
        ];
        bus = BUSES.iter().find(|(g, _)| *g == iface).map(|(_, b)| *b);
        if iface == DI_AHCI || iface == DI_IDE {
            if let Some(b) = disk_info(di, buf, true) {
                id = inventory::ata_identify(b);
            }
        } else if iface == DI_NVME {
            let wwn = disk_info(di, buf, true).and_then(inventory::nvme_namespace_wwn);
            id = DriveId { wwn, media: Some("nvme"), ..DriveId::default() };
        } else if iface == DI_SCSI || iface == DI_USB {
            if let Some(b) = disk_info(di, buf, false) {
                id = inventory::scsi_inquiry(b);
            }
        }
    }
    let path = path.unwrap_or("");
    if bus.is_none() {
        bus = if path.contains("NVMe(") {
            Some("nvme")
        } else if path.contains("Sata(") {
            Some("sata")
        } else if path.contains("Scsi(") {
            Some("scsi")
        } else if path.contains("USB(") || path.contains("UsbClass(") {
            Some("usb")
        } else if pci_of(h).and_then(crate::drivers::pci_info).is_some_and(|(_, (v, _), _)| v == 0x1af4) {
            Some("virtio")
        } else {
            None
        };
    }
    if bus == Some("nvme") {
        if let Some(c) = nvme_identify_controller(h, buf) {
            let c = inventory::nvme_controller(c);
            id = DriveId { wwn: id.wwn.take(), ..c };
        }
    }
    if bus == Some("scsi") {
        scsi_vpd(h, buf, &mut id);
    }
    (bus, id)
}

/// `EFI_DISK_INFO`'s Identify (`identify`) or Inquiry data, into `buf`.
fn disk_info(di: *mut DiskInfoProtocol, buf: &mut PageBuf, identify: bool) -> Option<&[u8]> {
    let page = buf.page();
    page.fill(0);
    let mut len = page.len() as u32;
    let st = unsafe {
        let f = if identify { (*di).identify } else { (*di).inquiry };
        f(di, page.as_mut_ptr() as *mut c_void, &mut len)
    };
    (st == Status::SUCCESS && len > 0).then(|| &page[..(len as usize).min(page.len())])
}

/// NVMe Identify Controller (admin opcode 0x06, CNS 1) through the pass-thru
/// on the namespace's controller.
fn nvme_identify_controller(h: uefi_raw::Handle, buf: &mut PageBuf) -> Option<&[u8]> {
    let bs = unsafe { uefi::table::system_table_raw()?.as_ref().boot_services.as_ref()? };
    let mut dp = crate::net::handle_protocol(h, &DEVICE_PATH)? as *const DevicePathProtocol;
    let mut ctrl: uefi_raw::Handle = ptr::null_mut();
    if unsafe { (bs.locate_device_path)(&NVME_PASS_THRU, &mut dp, &mut ctrl) } != Status::SUCCESS || ctrl.is_null() {
        return None;
    }
    let pt = crate::net::handle_protocol(ctrl, &NVME_PASS_THRU)? as *mut NvmePassThru;
    let align = unsafe { (*pt).mode.as_ref().map_or(1, |m| m.io_align) };
    if align > 4096 {
        return None;
    }
    let page = buf.page();
    page.fill(0);
    let mut cmd = NvmeCommand { cdw0: 0x06, flags: 0x04, cdw10: 1, ..NvmeCommand::default() };
    let mut done = NvmeCompletion::default();
    let mut packet = NvmePacket {
        timeout: TWO_SECONDS,
        buffer: page.as_mut_ptr() as *mut c_void,
        length: 4096,
        metadata: ptr::null_mut(),
        metadata_length: 0,
        queue_type: 0,
        cmd: &mut cmd,
        completion: &mut done,
    };
    let st = unsafe { ((*pt).pass_thru)(pt, 0, &mut packet, ptr::null_mut()) };
    (st == Status::SUCCESS).then_some(&page[..])
}

/// INQUIRY with EVPD (opcode 0x12) for pages 0x80 (serial), 0x83 (WWN) and
/// 0xB1 (rotation), through `EFI_SCSI_IO` on the disk's handle.
fn scsi_vpd(h: uefi_raw::Handle, buf: &mut PageBuf, id: &mut DriveId) {
    let Some(io) = crate::net::handle_protocol(h, &SCSI_IO).map(|p| p as *mut ScsiIoProtocol) else { return };
    if unsafe { (*io).io_align } > 64 {
        return;
    }
    for code in [0x80u8, 0x83, 0xB1] {
        // The CDB and sense data at the page's end, the data at its start,
        // all on the boundary the page gives.
        let page = buf.page();
        page.fill(0);
        let (data, rest) = page.split_at_mut(4096 - 128);
        let (cdb, sense) = rest.split_at_mut(64);
        cdb[..6].copy_from_slice(&[0x12, 0x01, code, 0x00, 0xFF, 0x00]);
        let mut packet = ScsiPacket {
            timeout: TWO_SECONDS,
            in_data: data.as_mut_ptr() as *mut c_void,
            out_data: ptr::null_mut(),
            sense: sense.as_mut_ptr() as *mut c_void,
            cdb: cdb.as_mut_ptr() as *mut c_void,
            in_length: 0xFF,
            out_length: 0,
            cdb_length: 6,
            direction: 0,
            host_status: 0,
            target_status: 0,
            sense_length: 64,
        };
        let st = unsafe { ((*io).execute)(io, &mut packet, ptr::null_mut()) };
        if st != Status::SUCCESS || packet.target_status != 0 {
            continue;
        }
        let got = &data[..(packet.in_length as usize).min(0xFF)];
        match code {
            0x80 => id.serial = id.serial.take().or_else(|| inventory::scsi_vpd_serial(got)),
            0x83 => id.wwn = id.wwn.take().or_else(|| inventory::scsi_vpd_wwn(got)),
            _ => id.media = id.media.or_else(|| inventory::scsi_vpd_media(got)),
        }
    }
}

/// What a disk's partition table holds: the GPT (CRC-checked by `esp.rs`),
/// else an MBR's four primary entries, else `none`; an ESP's `\EFI` listed.
fn table(bd: &mut crate::espboot::BlockDisk, disk: &mut Disk) {
    let bs = disk.block_size;
    let mut entries = [esp::GptEntry::EMPTY; inventory::MAX_PARTS];
    match esp::partitions(bd, bs, &mut entries) {
        Ok(n) => {
            disk.table = Some("gpt");
            disk.partitions_omitted = n.saturating_sub(entries.len());
            for e in &entries[..n.min(entries.len())] {
                let name: String =
                    char::decode_utf16(e.name[..e.name_len].iter().copied()).map(|c| c.unwrap_or('?')).collect();
                let name = String::from(name.trim());
                disk.partitions.push(Part {
                    number: e.partition.number,
                    kind: inventory::partition_kind(&e.type_guid),
                    bytes: e.partition.blocks().saturating_mul(bs as u64),
                    name: (!name.is_empty()).then_some(name),
                    efi: if e.is_esp() { efi_dirs(bd, bs, &e.partition) } else { Vec::new() },
                });
            }
        }
        Err(esp::Error::Io) => {}
        Err(_) => {
            let mut mbr = [0u8; 512];
            if !esp::Disk::read(bd, 0, &mut mbr) {
                return;
            }
            if mbr[510..512] != [0x55, 0xAA] {
                disk.table = Some("none");
                return;
            }
            disk.table = Some("mbr");
            for (i, e) in mbr[446..510].chunks_exact(16).enumerate() {
                let count = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as u64;
                if e[4] == 0 || count == 0 {
                    continue;
                }
                disk.partitions.push(Part {
                    number: i as u32 + 1,
                    kind: inventory::mbr_kind(e[4]),
                    bytes: count * bs as u64,
                    name: None,
                    efi: Vec::new(),
                });
            }
        }
    }
}

/// The directories under an ESP's `\EFI`: `BOOT` and one per installed OS.
fn efi_dirs(bd: &mut crate::espboot::BlockDisk, bs: u32, p: &esp::Partition) -> Vec<String> {
    let mut out = Vec::new();
    let bs = bs as u64;
    let Ok(fat) = esp::Fat::mount(bd, p.first_lba * bs, p.blocks() * bs) else { return out };
    let mut fat = Box::new(fat);
    let Ok(efi) = fat.lookup(bd, "\\EFI") else { return out };
    let Ok(mut cur) = fat.open_dir(&efi) else { return out };
    while let Ok(Some(l)) = fat.next_entry(bd, &mut cur) {
        if !l.entry.dir {
            continue;
        }
        let name: String = char::decode_utf16(l.name().iter().copied()).map(|c| c.unwrap_or('?')).collect();
        if name == "." || name == ".." {
            continue;
        }
        if out.len() == inventory::MAX_EFI_DIRS {
            out.push(String::from("…"));
            break;
        }
        out.push(name);
    }
    out
}
