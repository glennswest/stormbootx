//! NIC drivers carried on the boot media (#26).
//!
//! A platform can carry the whole upper network stack — SnpDxe, MnpDxe,
//! Ip4Dxe, TcpDxe — and still have no UEFI driver for the NICs it was built
//! with. The Supermicro X9 blades (server1–8) are that platform: their Intel
//! 10G (`IBA XE`) and ConnectX-3 (`FlexBoot`) carry legacy option ROMs only,
//! the one UEFI NIC driver in the firmware (`PRO/1000`) manages no device, and
//! with no `EFI_SIMPLE_NETWORK` handle there is nothing for MNP/IP4/TCP4 to
//! bind to. No setup switch changes that.
//!
//! So the media brings the missing bottom layer: every `*.efi` in
//! `\stormboot\drivers\` on the volume this binary booted from is loaded and
//! started as a driver, and then every handle is connected so the new driver
//! binds its NIC and stormbootx's own TCP/IP (#56) runs on the SNP it
//! produces. What ships there, on the rustnic medium only, is the Rust
//! stormnic drivers (`scripts/build-nic-drivers.sh`); no medium carries iPXE
//! (#91), and the fw medium carries no drivers (#52).
//!
//! **The platform goes first.** Before anything is loaded, one recursive
//! `ConnectController` pass lets the firmware's own drivers claim every NIC
//! they will take. A media driver then only finds the NICs nothing else wanted
//! — it never displaces a native driver on a machine that has one, so the same
//! media is safe to boot anywhere.
//!
//! **Unless the media says otherwise, for one family** (#108). Every OVMF
//! (Fedora's, pve's) carries VirtioNetDxe, which binds a VM's virtio-net NIC
//! in that first pass, so stormnic-virtio, which like every stormnic driver
//! declines a function another driver holds, would never run. With
//! `prefer_media_drivers = virtio` in `stormboot.conf` (the rustnic media
//! only), `take_over` disconnects the firmware's driver tree from each
//! virtio-net function, leaf first, and connects the function naming only the
//! media's driver. If that driver does not then hold it, the firmware's is
//! connected again, so a NIC is never left with no driver. Without the key
//! nothing is taken from anyone; virtio IDs exist only in VMs, so on metal the
//! key changes nothing.
//!
//! Nothing here is fatal. A missing directory is the normal case; a driver
//! that will not load or start is reported and skipped, and the boot goes on
//! to find TCP4 or fall through exactly as it would have.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use core::ffi::c_void;
use core::ptr;

use uefi::boot::{self, LoadImageSource, SearchType};
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::BootPolicy;
use uefi::{guid, CStr16, Guid, Status};
use uefi::runtime::{VariableAttributes, VariableVendor};
use uefi_raw::table::boot::BootServices;
use uefi_raw::Boolean;

/// Where drivers live on the boot media, beside `stormboot.conf`.
pub const DRIVERS_DIR: &str = r"\stormboot\drivers";

/// One driver file and what became of it.
pub struct Loaded {
    pub name: String,
    pub result: Result<(), String>,
    /// The started driver's image handle (its DriverBinding is installed on
    /// it), for `take_over`.
    image: Option<uefi::Handle>,
}

/// A family of media driver that may take its NICs from the firmware's own
/// driver, when `prefer_media_drivers` names it (#108): the file on the
/// media and the PCI IDs of the network functions (class 02) it drives.
struct Family {
    name: &'static str,
    file: &'static str,
    ids: &'static [(u16, u16)],
}

/// Only virtio-net today: the one NIC a firmware always has a driver for
/// (every OVMF's VirtioNetDxe) that a stormnic driver also drives. Modern
/// virtio 1.x and transitional; the driver declines a transitional device
/// without the 1.x capabilities, and then the firmware's is given it back.
const FAMILIES: &[Family] = &[Family {
    name: "virtio",
    file: "stormnic-virtio.efi",
    ids: &[(0x1af4, 0x1041), (0x1af4, 0x1000)],
}];

const PCI_IO: Guid = guid!("4cf5b200-68b8-4ca5-9eec-b23e3f50029a");
const BY_CHILD_CONTROLLER: u32 = 0x08;
const BY_DRIVER: u32 = 0x10;

/// Is this a file to load? `.efi` in any case, since FAT keeps what was
/// written and a tool may have upper-cased it.
fn is_driver(name: &str) -> bool {
    name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".efi")
}

/// Load and start every driver on the media, then connect every handle.
///
/// Returns one entry per file tried, in name order; empty when the media
/// carries no drivers, in which case nothing at all was done.
pub fn load_from_media() -> Vec<Loaded> {
    let names: Vec<String> = crate::config::list_dir(DRIVERS_DIR)
        .into_iter()
        .filter(|n| is_driver(n))
        .collect();
    if names.is_empty() {
        return Vec::new();
    }

    // Each step is announced *before* it runs, with the seconds so far. A
    // driver that hangs in its start or in binding its NIC never returns, and
    // on server1 a hang with the summary printed only at the end left the
    // console silent with nothing to say which driver it was (#26).
    let t0 = now_secs();
    uefi::println!("drivers     : {} on the media in {DRIVERS_DIR}", names.len());

    // The stormnic drivers read `StormnicVerbose` once, at their entry point,
    // so it is set before any of them is loaded (#80, #102).
    nic_verbose();

    // The platform's own drivers first (see the module comment).
    uefi::println!("    binding the platform's own drivers first");
    let _ = crate::net::connect_all();

    let volume_dp = crate::config::boot_volume()
        .and_then(|h| crate::blockio::device_path_of(h.as_ptr()));
    let out: Vec<Loaded> = names
        .into_iter()
        .map(|name| {
            uefi::println!("    [{:>3} s] starting {name}", elapsed(t0));
            let started = match volume_dp {
                Some(dp) => load_one(dp, &name),
                None => Err("the boot volume has no device path".into()),
            };
            match &started {
                Ok(_) => uefi::println!("    [{:>3} s] {name} started", elapsed(t0)),
                Err(e) => uefi::println!("    [{:>3} s] {name} not started: {e}", elapsed(t0)),
            }
            let image = started.as_ref().ok().copied();
            Loaded { name, result: started.map(|_| ()), image }
        })
        .collect();

    // Bind what was just registered: each driver to its NIC, and the
    // platform's MNP/IP4/TCP4 to the SNP handles those produce. Recursive, so
    // one pass reaches the children.
    if out.iter().any(|l| l.result.is_ok()) {
        uefi::println!("    [{:>3} s] connecting every handle (drivers bind their NICs)", elapsed(t0));
        let _ = crate::net::connect_all();
        uefi::println!("    [{:>3} s] connected", elapsed(t0));
        take_over(&out, t0);
    }
    out
}

/// `StormnicVerbose`, the switch every stormnic driver reads (vendor GUID
/// `ce1479a2-eab9-4176-b0ad-c909ea5b8e0b`, one byte, non-zero: print the whole
/// bring-up trace). Set from `nic_verbose = true` in `stormboot.conf`,
/// `BOOTSERVICE_ACCESS` only, so it is never written to NVRAM and is gone at
/// the next reset. One an operator set from the shell is left as it is.
const STORMNIC_VENDOR: Guid = guid!("ce1479a2-eab9-4176-b0ad-c909ea5b8e0b");

fn nic_verbose() {
    let name = uefi::cstr16!("StormnicVerbose");
    let vendor = VariableVendor(STORMNIC_VENDOR);
    let mut buf = [0u8; 8];
    let already = matches!(uefi::runtime::get_variable(name, &vendor, &mut buf), Ok((d, _)) if d.first().is_some_and(|&b| b != 0));
    if !crate::config::stated_nic_verbose() {
        if already {
            uefi::println!("drivers     : stormnic drivers verbose (StormnicVerbose was already set)");
        }
        return;
    }
    match uefi::runtime::set_variable(name, &vendor, VariableAttributes::BOOTSERVICE_ACCESS, &[1]) {
        Ok(()) => uefi::println!("drivers     : stormnic drivers verbose (nic_verbose in {}; StormnicVerbose set until reset)", crate::config::CONF_PATH),
        Err(e) => uefi::println!("drivers     : nic_verbose asked for, but StormnicVerbose was refused ({:?}); the drivers stay quiet", e.status()),
    }
}

/// Give the NICs of each family `prefer_media_drivers` names to the media's
/// driver, taking them from the firmware's (#108; see the module comment).
/// One console line per function.
fn take_over(loaded: &[Loaded], t0: Option<u32>) {
    let prefer = crate::config::stated_prefer_media_drivers();
    for want in &prefer {
        let Some(family) = FAMILIES.iter().find(|f| f.name == want.as_str()) else {
            uefi::println!("    prefer    : no media driver family `{want}` (virtio); ignored");
            continue;
        };
        let image = loaded
            .iter()
            .find(|l| l.name.eq_ignore_ascii_case(family.file))
            .and_then(|l| l.image);
        let Some(image) = image else {
            uefi::println!("    prefer    : {want}, but {} was not started from the media; the firmware keeps its NICs", family.file);
            continue;
        };
        for (pci, at, device) in network_functions(family.ids) {
            let line = take_one(pci, image.as_ptr(), family.file);
            uefi::println!("    [{:>3} s] {at} {:04x}:{:04x}: {line}", elapsed(t0), device.0, device.1);
        }
    }
}

/// Take one PCI function from whichever drivers hold it and give it to
/// `ours`. Returns what happened, for the console.
fn take_one(pci: uefi_raw::Handle, ours: uefi_raw::Handle, file: &str) -> String {
    let Some(bs) = crate::snpwatch::bs() else { return "no boot services".into() };
    let theirs = holders(pci, ours);
    if theirs.is_empty() {
        return if holders(pci, ptr::null_mut()).contains(&ours) {
            format!("{file} drives it already")
        } else {
            // Nothing holds it: an ordinary connect binds ours.
            connect_with(bs, pci, ours);
            match holders(pci, ptr::null_mut()).contains(&ours) {
                true => format!("held by nothing; {file} bound"),
                false => format!("held by nothing, and {file} did not bind"),
            }
        };
    }
    let names = names_of(&theirs);

    // Leaf first, in up to three passes: a network stack bound recursively on
    // top (MNP, IP4, PXE, HTTP boot) comes down from the top. What counts is
    // what is left holding PciIo, not the status: pve's OVMF answers
    // NOT_FOUND when every driver did come off.
    let mut pass = 0;
    while !holders(pci, ours).is_empty() && pass < 3 {
        pass += 1;
        disconnect_tree(bs, pci, 0);
    }
    if !holders(pci, ours).is_empty() {
        crate::net::connect(pci);
        return format!("{names} would not let go after 3 passes; left with it");
    }
    connect_with(bs, pci, ours);
    if holders(pci, ptr::null_mut()).contains(&ours) {
        format!("taken from {names}; {file} drives it")
    } else {
        // Never leave a NIC with no driver: the firmware's binds it again.
        crate::net::connect(pci);
        format!("{file} did not bind; given back to {names}")
    }
}

/// `ConnectController(pci, {ours, NULL}, NULL, FALSE)`: ours and nothing else
/// asked first, and nothing stacked on top (stormbootx opens the SNP itself).
fn connect_with(bs: &BootServices, pci: uefi_raw::Handle, ours: uefi_raw::Handle) {
    let mut list = [ours, ptr::null_mut()];
    unsafe {
        let _ = (bs.connect_controller)(pci, list.as_mut_ptr(), ptr::null(), Boolean::FALSE);
    }
}

/// The agents holding `pci`'s PciIo `BY_DRIVER`, other than `except`.
fn holders(pci: uefi_raw::Handle, except: uefi_raw::Handle) -> Vec<uefi_raw::Handle> {
    let Some(bs) = crate::snpwatch::bs() else { return Vec::new() };
    let mut out = Vec::new();
    for (g, e) in crate::snpwatch::opens(bs, pci) {
        if g == PCI_IO && e.attributes & BY_DRIVER != 0 && e.agent_handle != except && !out.contains(&e.agent_handle) {
            out.push(e.agent_handle);
        }
    }
    out
}

fn names_of(agents: &[uefi_raw::Handle]) -> String {
    let mut names: Vec<String> = Vec::new();
    for &a in agents {
        let n = crate::snpwatch::name_of(a).unwrap_or_else(|| format!("driver handle {a:p}"));
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names.join(" + ")
}

/// Disconnect every driver on `h`'s subtree, children (the handles opened
/// `BY_CHILD_CONTROLLER` on its protocols) first.
fn disconnect_tree(bs: &BootServices, h: uefi_raw::Handle, depth: usize) {
    if depth > 8 {
        return;
    }
    let mut children: Vec<uefi_raw::Handle> = Vec::new();
    for (_, e) in crate::snpwatch::opens(bs, h) {
        if e.attributes & BY_CHILD_CONTROLLER != 0 && e.controller_handle != h && !children.contains(&e.controller_handle) {
            children.push(e.controller_handle);
        }
    }
    for c in children {
        disconnect_tree(bs, c, depth + 1);
    }
    unsafe {
        let _ = (bs.disconnect_controller)(h, ptr::null_mut(), ptr::null_mut());
    }
}

/// The start of `EFI_PCI_IO_PROTOCOL`, as far as `GetLocation` (UEFI 2.10
/// §14.4). Only `Pci.Read` and `GetLocation` are called; the rest are
/// placeholders of a function pointer's size.
#[repr(C)]
struct PciIoHead {
    _poll: [usize; 2],
    _mem: [usize; 2],
    _io: [usize; 2],
    pci_read: unsafe extern "efiapi" fn(*mut PciIoHead, u32, u32, usize, *mut c_void) -> Status,
    _pci_write: usize,
    _copy_map_unmap_alloc_free_flush: [usize; 6],
    get_location: unsafe extern "efiapi" fn(*mut PciIoHead, *mut usize, *mut usize, *mut usize, *mut usize) -> Status,
}

/// The network functions (class 02) with one of `ids`: (handle,
/// `seg:bus:dev.fn`, (vendor, device)).
fn network_functions(ids: &[(u16, u16)]) -> Vec<(uefi_raw::Handle, String, (u16, u16))> {
    let mut out = Vec::new();
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&PCI_IO)) else { return out };
    for h in handles.iter().map(|h| h.as_ptr()) {
        let Some(io) = crate::net::handle_protocol(h, &PCI_IO) else { continue };
        let io = io as *mut PciIoHead;
        let (mut id, mut class) = (0u32, 0u32);
        // Width 2: 32-bit config reads.
        let ok = unsafe {
            ((*io).pci_read)(io, 2, 0, 1, &mut id as *mut u32 as *mut c_void) == Status::SUCCESS
                && ((*io).pci_read)(io, 2, 8, 1, &mut class as *mut u32 as *mut c_void) == Status::SUCCESS
        };
        let dev = (id as u16, (id >> 16) as u16);
        if !ok || class >> 24 != 2 || !ids.contains(&dev) {
            continue;
        }
        let (mut seg, mut bus, mut d, mut f) = (0usize, 0usize, 0usize, 0usize);
        let at = match unsafe { ((*io).get_location)(io, &mut seg, &mut bus, &mut d, &mut f) } {
            Status::SUCCESS => format!("{seg:04x}:{bus:02x}:{d:02x}.{f}"),
            _ => String::from("pci ?"),
        };
        out.push((h, at, dev));
    }
    out
}

/// Seconds since midnight from the RTC: coarse, but a hang is measured in
/// minutes, and it needs no timer protocol. `None` if the RTC will not say.
fn now_secs() -> Option<u32> {
    uefi::runtime::get_time()
        .ok()
        .map(|t| t.hour() as u32 * 3600 + t.minute() as u32 * 60 + t.second() as u32)
}

fn elapsed(t0: Option<u32>) -> u32 {
    match (t0, now_secs()) {
        // Across midnight the clock wraps; a day is added back.
        (Some(a), Some(b)) => (b + 86_400 - a) % 86_400,
        _ => 0,
    }
}

/// `LoadImage` by device path, then `StartImage`.
///
/// By device path rather than from a buffer so the driver's `LoadedImage`
/// carries a real `DeviceHandle` and `FilePath`: a driver may read its own
/// device path at start-up and refuse to run without one (iPXE's did).
fn load_one(volume: &uefi::proto::device_path::DevicePath, name: &str) -> Result<uefi::Handle, String> {
    let path = format!(r"{DRIVERS_DIR}\{name}");
    let mut buf = [0u16; 256];
    let path16 = CStr16::from_str_with_buf(&path, &mut buf).map_err(|_| "path too long")?;
    let mut fbuf = Vec::new();
    let file_dp = DevicePathBuilder::with_vec(&mut fbuf)
        .push(&build::media::FilePath { path_name: path16 })
        .and_then(|b| b.finalize())
        .map_err(|e| format!("file path build failed: {e:?}"))?;
    let full = volume
        .append_path(file_dp)
        .map_err(|e| format!("append_path failed: {e:?}"))?;

    let image = boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromDevicePath {
            device_path: &full,
            boot_policy: BootPolicy::ExactMatch,
        },
    )
    .map_err(|e| format!("LoadImage: {:?}", e.status()))?;

    // A driver's entry point registers its binding and returns; the image
    // stays resident. One that fails is unloaded so it holds nothing.
    boot::start_image(image).map_err(|e| {
        let _ = boot::unload_image(image);
        format!("StartImage: {:?}", e.status())
    })?;
    Ok(image)
}
