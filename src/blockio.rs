//! Publish the remote namespace as `EFI_BLOCK_IO_PROTOCOL`.
//!
//! This is the point of the whole extension. Once a handle carries BlockIO and
//! a device path, the firmware's own drivers take over: the partition driver
//! reads the GPT and produces a handle per partition, and the FAT driver mounts
//! the ESP. `boot_attached` then loads the ESP's `BOOTX64.EFI` itself, because
//! the boot manager will not boot a disk that appeared mid-boot, and reads the
//! ESP itself when the firmware's FAT can't (#37). Nothing above needs to know
//! the blocks arrive over TCP.
//!
//! The protocol's function pointers are bare `extern "efiapi"` functions with
//! no context argument, so the namespace they act on has to be reachable from
//! a static. That is not a shortcut around ownership: firmware is
//! single-threaded here, there is exactly one namespace per boot, and the
//! alternative is inventing a registry keyed on the protocol pointer.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use core::ptr;

use uefi::boot::{self, LoadImageSource, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::proto::BootPolicy;
use uefi::proto::device_path::DevicePath;
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{Handle, Identify, cstr16, guid};
use uefi::Status;
use uefi_raw::protocol::block::{BlockIoMedia, BlockIoProtocol};
use uefi_raw::Boolean;
use uefi_raw::Guid;

use crate::config::EspReader;
use crate::nvme::Namespace;

/// The one namespace this boot is serving. Set once before the protocol is
/// installed and never replaced.
static mut NAMESPACE: Option<Namespace> = None;

/// Media descriptor, kept alive for as long as the protocol is installed.
static mut MEDIA: BlockIoMedia = BlockIoMedia {
    media_id: 1,
    removable_media: Boolean::FALSE,
    media_present: Boolean::TRUE,
    logical_partition: Boolean::FALSE,
    read_only: Boolean::FALSE,
    write_caching: Boolean::FALSE,
    block_size: 512,
    io_align: 1,
    last_block: 0,
    lowest_aligned_lba: 0,
    logical_blocks_per_physical_block: 1,
    optimal_transfer_length_granularity: 1,
};

#[allow(static_mut_refs)]
unsafe fn namespace() -> Option<&'static mut Namespace> {
    NAMESPACE.as_mut()
}

/// What the reads cost, reported on the console while the image's own
/// bootloader runs (#46). A boot that stalls inside the bootloader's reads
/// otherwise shows only the bootloader's last line, and that can't tell a
/// read that crawls from one that fails or one that was never asked for.
struct ReadLog {
    /// TSC ticks per millisecond, measured once at publish; 0 if unmeasured.
    ticks_per_ms: u64,
    /// Bytes read since the last progress line.
    bytes: u64,
    /// Ticks spent inside reads since the last progress line.
    ticks: u64,
    /// Bytes read in all.
    total: u64,
}

static mut READS: ReadLog = ReadLog { ticks_per_ms: 0, bytes: 0, ticks: 0, total: 0 };

/// A progress line per this many bytes read.
const REPORT_EVERY: u64 = 64 << 20;
/// A single read slower than this is reported on its own.
const SLOW_READ_MS: u64 = 2000;

fn tsc() -> u64 {
    // SAFETY: RDTSC has no preconditions on x86_64.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Calibrate the TSC against `Stall`. Once, at publish.
fn calibrate_reads() {
    let t0 = tsc();
    boot::stall(core::time::Duration::from_millis(10));
    let per_ms = (tsc() - t0) / 10;
    unsafe { (*ptr::addr_of_mut!(READS)).ticks_per_ms = per_ms };
}

/// Account for one read of `len` bytes at `lba` that took `ticks`, and say
/// so when it failed, was slow, or completes another `REPORT_EVERY`.
fn log_read(lba: u64, len: usize, ticks: u64, failed: Option<&str>) {
    let log = unsafe { &mut *ptr::addr_of_mut!(READS) };
    let ms = if log.ticks_per_ms == 0 { 0 } else { ticks / log.ticks_per_ms };
    if let Some(e) = failed {
        uefi::println!("blockio     : read of {len} bytes at lba {lba} failed after {ms} ms: {e}");
        return;
    }
    if ms > SLOW_READ_MS {
        uefi::println!("blockio     : read of {len} bytes at lba {lba} took {ms} ms");
    }
    log.bytes += len as u64;
    log.ticks += ticks;
    log.total += len as u64;
    if log.bytes >= REPORT_EVERY {
        let spent = if log.ticks_per_ms == 0 { 0 } else { log.ticks / log.ticks_per_ms };
        let mib = log.bytes >> 20;
        uefi::println!(
            "blockio     : {} MiB read in all; the last {mib} MiB took {spent} ms ({} MiB/s)",
            log.total >> 20,
            (mib * 1000).checked_div(spent).unwrap_or(0)
        );
        log.bytes = 0;
        log.ticks = 0;
    }
}

unsafe extern "efiapi" fn reset(_this: *mut BlockIoProtocol, _extended: Boolean) -> Status {
    // The connection is established once at attach time. A reset that tore it
    // down and rebuilt it would turn a transient read error into a boot that
    // hangs re-handshaking, so this is deliberately a no-op.
    Status::SUCCESS
}

unsafe extern "efiapi" fn read_blocks(
    _this: *const BlockIoProtocol,
    media_id: u32,
    lba: u64,
    buffer_size: usize,
    buffer: *mut core::ffi::c_void,
) -> Status {
    unsafe {
        if media_id != MEDIA.media_id {
            return Status::MEDIA_CHANGED;
        }
        if buffer.is_null() {
            return Status::INVALID_PARAMETER;
        }
        if buffer_size == 0 {
            return Status::SUCCESS;
        }
        if buffer_size % MEDIA.block_size as usize != 0 {
            return Status::BAD_BUFFER_SIZE;
        }
        let Some(ns) = namespace() else {
            return Status::DEVICE_ERROR;
        };
        let slice = core::slice::from_raw_parts_mut(buffer as *mut u8, buffer_size);
        let t0 = tsc();
        let r = ns.read(lba, slice);
        log_read(lba, buffer_size, tsc().wrapping_sub(t0), r.as_ref().err().map(|e| e.as_str()));
        match r {
            Ok(()) => Status::SUCCESS,
            Err(_) => Status::DEVICE_ERROR,
        }
    }
}

unsafe extern "efiapi" fn write_blocks(
    _this: *mut BlockIoProtocol,
    media_id: u32,
    lba: u64,
    buffer_size: usize,
    buffer: *const core::ffi::c_void,
) -> Status {
    unsafe {
        if media_id != MEDIA.media_id {
            return Status::MEDIA_CHANGED;
        }
        if buffer.is_null() {
            return Status::INVALID_PARAMETER;
        }
        if buffer_size == 0 {
            return Status::SUCCESS;
        }
        if buffer_size % MEDIA.block_size as usize != 0 {
            return Status::BAD_BUFFER_SIZE;
        }
        let Some(ns) = namespace() else {
            return Status::DEVICE_ERROR;
        };
        let slice = core::slice::from_raw_parts(buffer as *const u8, buffer_size);
        match ns.write(lba, slice) {
            Ok(()) => Status::SUCCESS,
            Err(_) => Status::DEVICE_ERROR,
        }
    }
}

unsafe extern "efiapi" fn flush_blocks(_this: *mut BlockIoProtocol) -> Status {
    // Writes are synchronous: `write_blocks` does not return until the
    // controller has completed the command, so there is nothing buffered here
    // to flush.
    Status::SUCCESS
}

/// How many disks this machine could boot on its own.
///
/// Counts non-removable, non-partition block devices — the whole-disk handles,
/// not the GPT partitions the firmware's partition driver produces from them,
/// and not the stick this is running off.
///
/// This exists so the fall-through message can be honest. "Falling through to
/// the local disk" is advice, not an outcome; on a machine with nothing
/// installed it is the wrong thing to print, and an operator reading a serial
/// console needs to know which of the two situations they are in before they
/// start looking at the network.
pub fn local_disks() -> usize {
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&BlockIoProtocol::GUID))
    else {
        return 0;
    };
    handles
        .iter()
        .filter(|h| {
            let Some(p) = crate::net::handle_protocol(h.as_ptr(), &BlockIoProtocol::GUID) else {
                return false;
            };
            let proto = p as *const BlockIoProtocol;
            unsafe {
                let media = (*proto).media;
                if media.is_null() {
                    return false;
                }
                // A logical partition is a view of a disk already counted, and
                // removable media is the stick this booted from (or an empty
                // optical drive, which is worse than useless to fall back to).
                bool::from((*media).media_present)
                    && !bool::from((*media).logical_partition)
                    && !bool::from((*media).removable_media)
            }
        })
        .count()
}

/// A local disk that can boot on its own, for `auto` (#3): the first whole,
/// present, non-removable disk other than the media this image was loaded
/// from whose GPT has an ESP carrying `\EFI\BOOT\BOOTX64.EFI`. Read with
/// `esp.rs`, so it needs no network and no firmware FAT (Aptio 4's misreads a
/// 4K ESP, #37). The file is found, not read.
///
/// `Ok` names the disk found; `Err` says why none was, per disk, so the
/// console shows why a machine claimed rather than booting what it has.
/// Called before anything is attached, so every BlockIO here is the machine's.
///
/// Any bootloader counts, not only stormuefi: the owner's rule is "a local
/// ESP with BOOTX64.EFI" (#3, 2026-09-30). A disk with another OS on it
/// boots that OS under `auto`; `install` is how it gets replaced.
pub fn local_bootloader() -> Result<String, String> {
    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&BlockIO::GUID))
        .map_err(|e| format!("no block devices ({e:?})"))?;
    // The stick or virtual CD this runs off carries a BOOTX64.EFI too (this
    // binary). Removable media is skipped anyway; this also covers media that
    // says it is fixed. GetProtocol, never exclusive: nothing is closed here
    // that the firmware holds.
    let own = open_get::<LoadedImage>(boot::image_handle())
        .and_then(|li| li.device())
        .and_then(path_of);
    let mut why = String::new();
    let mut n = 0;
    for &h in handles.iter() {
        let Some(bio) = open_get::<BlockIO>(h) else { continue };
        let media = bio.media();
        if !media.is_media_present() || media.is_logical_partition() || media.is_removable_media() {
            continue;
        }
        let dp = path_of(h);
        if let (Some(dp), Some(own)) = (dp, own) {
            if is_prefix(dp, own) {
                continue;
            }
        }
        n += 1;
        let block = media.block_size();
        let size = (media.last_block() + 1).saturating_mul(block as u64);
        let mut disk = crate::espboot::BlockDisk {
            bio: &bio,
            media_id: media.media_id(),
            block: block as u64,
            bounce: alloc::vec::Vec::new(),
        };
        match crate::espboot::find(&mut disk, block) {
            Ok((p, fat, bytes)) => {
                return Ok(format!(
                    "disk {n} ({} GiB, {block}-byte blocks): ESP partition {}, FAT{fat}, {} ({bytes} bytes)",
                    size >> 30,
                    p.number,
                    crate::espboot::BOOTLOADER
                ));
            }
            Err(e) => {
                why.push_str(&format!("\n    disk {n} ({} GiB): {e}", size >> 30));
            }
        }
    }
    if n == 0 {
        Err(String::from("no local disk"))
    } else {
        Err(format!("no local disk carries {}:{why}", crate::espboot::BOOTLOADER))
    }
}

/// `OpenProtocol` with `GET_PROTOCOL`: a look, not a claim.
fn open_get<P: uefi::proto::ProtocolPointer + ?Sized>(h: Handle) -> Option<boot::ScopedProtocol<P>> {
    unsafe {
        boot::open_protocol::<P>(
            OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
    }
    .ok()
}

fn path_of(h: Handle) -> Option<&'static DevicePath> {
    let p = open_get::<DevicePath>(h)?;
    let r: &DevicePath = &p;
    // The path lives as long as the handle does; closing a GetProtocol open
    // does not free it.
    Some(unsafe { &*(r as *const DevicePath) })
}

/// Whether every node of `a` starts `b`.
fn is_prefix(a: &DevicePath, b: &DevicePath) -> bool {
    a.node_iter().count() <= b.node_iter().count() && a.node_iter().zip(b.node_iter()).all(|(x, y)| x == y)
}

/// Install BlockIO on a new handle backed by `ns`.
///
/// Returns the handle so the caller can connect drivers to it — without that
/// the partition and filesystem drivers never bind and the disk stays invisible.
pub fn publish(ns: Namespace) -> Result<uefi_raw::Handle, String> {
    let geometry = ns.geometry;
    unsafe {
        MEDIA.block_size = geometry.block_size;
        MEDIA.last_block = geometry.blocks.saturating_sub(1);
        NAMESPACE = Some(ns);
    }
    calibrate_reads();

    let proto = Box::leak(Box::new(BlockIoProtocol {
        revision: 0x0001_0000,
        // Taking the address of a `static mut` needs no `unsafe`; only
        // dereferencing it does, and that happens under the accessors above.
        media: ptr::addr_of!(MEDIA) as *const BlockIoMedia,
        reset,
        read_blocks,
        write_blocks,
        flush_blocks,
    }));

    let mut handle: uefi_raw::Handle = ptr::null_mut();
    let st = unsafe {
        let st_ptr = uefi::table::system_table_raw().ok_or("no system table")?;
        let bs = st_ptr.as_ref().boot_services.as_ref().ok_or("no boot services")?;
        (bs.install_protocol_interface)(
            &mut handle,
            &BlockIoProtocol::GUID as *const Guid,
            uefi_raw::table::boot::InterfaceType::NATIVE_INTERFACE,
            proto as *mut BlockIoProtocol as *mut core::ffi::c_void,
        )
    };
    if st != Status::SUCCESS {
        return Err(format!("InstallProtocolInterface failed: {st:?}"));
    }

    // A device path, or the partition driver will not bind. EDK2's PartitionDxe
    // opens *both* BlockIO and DevicePath on a controller before it will parse
    // a GPT — a handle carrying BlockIO alone is one it skips, so no HD child
    // appears and no ESP with it. OVMF's partition driver is lenient here and
    // this was missed under it; real firmware is not, and the visible symptom
    // is a machine that attaches the image and then drops to setup because
    // there was never anything bootable on the handle.
    //
    // The path is a single vendor node with this project's own GUID: it needs
    // to be a valid, unique path for the partition driver to hang HD() nodes
    // off, and it identifies our disk when chain-loading picks the ESP back
    // out. Leaked, because a protocol interface outlives this call.
    let dp: &'static DevicePath = {
        let mut buf = alloc::vec::Vec::new();
        let path = DevicePathBuilder::with_vec(&mut buf)
            .push(&build::hardware::Vendor {
                vendor_guid: DISK_DP_GUID,
                vendor_defined_data: &[],
            })
            .and_then(|b| b.finalize())
            .map_err(|e| format!("device path build failed: {e:?}"))?;
        Box::leak(path.to_boxed())
    };
    unsafe {
        boot::install_protocol_interface(
            Some(Handle::from_ptr(handle).ok_or("null block handle")?),
            &DEVICE_PATH_GUID,
            dp.as_ffi_ptr() as *const core::ffi::c_void,
        )
        .map_err(|e| format!("InstallProtocolInterface(DevicePath) failed: {e:?}"))?;
    }

    // Bind the partition and filesystem drivers to the new handle, recursively,
    // so the GPT is parsed and the ESP's FAT is mounted.
    unsafe {
        if let Some(st_ptr) = uefi::table::system_table_raw() {
            if let Some(bs) = st_ptr.as_ref().boot_services.as_ref() {
                let _ =
                    (bs.connect_controller)(handle, ptr::null_mut(), ptr::null(), Boolean::TRUE);
            }
        }
    }

    Ok(handle)
}

/// Disks an earlier start of stormbootx published and never took back (#54).
///
/// Each start's disk carries the same vendor node, so a disk left behind is
/// one `firmware_boot`'s strict match would take for this start's own, and
/// its BlockIO calls into an image the firmware has already unloaded. Since
/// `withdraw` this should always be 0; the start says so when it is not.
pub fn stale() -> usize {
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&BlockIoProtocol::GUID))
    else {
        return 0;
    };
    handles
        .iter()
        .filter(|h| {
            // Whole disks only: the partitions under one carry the same
            // first node, and an HD() node after it.
            device_path_of(h.as_ptr())
                .filter(|dp| dp.node_iter().count() == 1)
                .and_then(|dp| dp.node_iter().next())
                .is_some_and(|n| {
                    n.device_type() == uefi::proto::device_path::DeviceType::HARDWARE
                        && n.sub_type() == uefi::proto::device_path::DeviceSubType::HARDWARE_VENDOR
                        && n.data().get(..16) == Some(&DISK_DP_GUID.to_bytes()[..])
                })
        })
        .count()
}

/// The vendor GUID naming a disk this binary published. Not an architectural
/// constant — just a unique value so the device path is well-formed and so
/// chain-loading can tell our ESP from a local one.
const DISK_DP_GUID: Guid = guid!("6d7a1f2e-9c34-4b8a-b1d0-5e2f7a0c9b41");
/// `EFI_DEVICE_PATH_PROTOCOL`.
const DEVICE_PATH_GUID: Guid = guid!("09576e91-6d3f-11d2-8e39-00a0c969723b");

/// Boot the image just attached, by loading its ESP's bootloader.
///
/// Publishing a block device does **not** make the firmware boot it: a UEFI
/// boot manager boots the entries in `BootOrder`, and a disk that appears
/// while a boot option is *running* is not in that list — so the manager, when
/// this option returns, moves to the next entry and, finding none, drops to
/// setup. That is the machine going to BIOS after "firmware can boot it".
///
/// So do not return to the manager: load `\EFI\BOOT\BOOTX64.EFI` off the
/// attached ESP and start it here. That is the removable-media default path
/// every whole-disk image carries (shim, or a bootloader), and starting it
/// hands the machine to the image's own boot chain exactly as booting the disk
/// from the menu would.
///
/// **Who reads the ESP** (#37). The firmware's own FAT driver first, as on the
/// R230. If it can't load the file, stormbootx reads the ESP itself
/// (`esp.rs`) and loads the bytes. A 4K image carries a 4096-byte-sector FAT,
/// and AMI Aptio 4 mounts one and then answers `NOT_FOUND` (#33).
/// `esp = firmware` or `esp = stormbootx` in `stormboot.conf` picks one of the
/// two alone.
///
/// Only returns on failure — a started image that itself exits comes back, and
/// that is the caller's cue to fall through to whatever else there is.
pub fn boot_attached(disk: uefi_raw::Handle, reader: EspReader) -> Result<(), String> {
    let why = match reader {
        EspReader::Stormbootx => String::from("esp = stormbootx"),
        _ => match firmware_boot(disk)? {
            Loaded::Ran(outcome) => return Err(outcome),
            Loaded::Not(why) if reader == EspReader::Firmware => return Err(why),
            Loaded::Not(why) => why,
        },
    };
    uefi::println!("boot        : the firmware did not load it ({why}); reading the ESP here");
    bridge_boot(disk)
}

/// What the firmware's own attempt came to.
enum Loaded {
    /// The bootloader was started and came back: this, and nothing else runs.
    Ran(String),
    /// Nothing was started: why.
    Not(String),
}

/// Load and start the bootloader through the firmware's filesystem drivers.
fn firmware_boot(disk: uefi_raw::Handle) -> Result<Loaded, String> {
    // The disk we published — its device path is the single vendor node we
    // installed. An ESP belongs to *this* disk only if its own device path
    // begins with that exact node; the partition driver appends an HD() node
    // after it. Anything else is a local disk, and a boot agent must never
    // boot one of those: booting whatever OS the machine already had is how a
    // provisioning boot silently launched a stale Windows install instead of
    // the image it just attached.
    let our_dp = device_path_of(disk).ok_or("attached disk has no device path")?;
    let our_first = our_dp
        .node_iter()
        .next()
        .ok_or("attached disk device path is empty")?;

    let file = cstr16!("\\EFI\\BOOT\\BOOTX64.EFI");
    let mut fbuf = alloc::vec::Vec::new();
    let file_path = DevicePathBuilder::with_vec(&mut fbuf)
        .push(&build::media::FilePath { path_name: file })
        .and_then(|b| b.finalize())
        .map_err(|e| format!("file path build failed: {e:?}"))?;

    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&SimpleFileSystem::GUID))
    else {
        return Ok(Loaded::Not(String::from("no filesystem on it")));
    };

    let image = boot::image_handle();
    let mut last = None;
    for h in handles.iter() {
        let Some(dp) = device_path_of(h.as_ptr()) else { continue };
        // Strict: only an ESP whose first node is our disk's node. No fallback
        // to any other filesystem, ever.
        match dp.node_iter().next() {
            Some(first) if first == our_first => {}
            _ => continue,
        }

        let full = match dp.append_path(file_path) {
            Ok(f) => f,
            Err(e) => {
                last = Some(format!("append_path failed: {e:?}"));
                continue;
            }
        };
        match boot::load_image(
            image,
            LoadImageSource::FromDevicePath {
                device_path: &full,
                boot_policy: BootPolicy::ExactMatch,
            },
        ) {
            Ok(loaded) => {
                uefi::println!("boot        : starting \\EFI\\BOOT\\BOOTX64.EFI from the attached image");
                return Ok(Loaded::Ran(match boot::start_image(loaded) {
                    Ok(()) => String::from("the image's bootloader exited"),
                    Err(e) => format!("the image's bootloader returned {e:?}"),
                }));
            }
            Err(e) => last = Some(format!("load of BOOTX64.EFI failed: {e:?}")),
        }
    }
    Ok(Loaded::Not(last.unwrap_or_else(|| {
        String::from("the firmware mounted no filesystem on the attached image")
    })))
}

/// Read the ESP with `esp.rs` and start its bootloader from the buffer.
fn bridge_boot(disk: uefi_raw::Handle) -> Result<(), String> {
    let our_dp = device_path_of(disk).ok_or("attached disk has no device path")?;
    // The namespace is borrowed only while reading: the bootloader started
    // below reads the disk through `read_blocks`, which borrows it again.
    let loader = {
        let ns = unsafe { namespace() }.ok_or("no namespace is attached")?;
        let block_size = ns.geometry.block_size;
        let mut d = NsDisk { ns, bounce: alloc::vec::Vec::new() };
        crate::espboot::read(&mut d, block_size)
            .map_err(|e| format!("stormbootx could not read the ESP either: {e}"))?
    };
    uefi::println!(
        "boot        : {} is {} bytes (partition {}, FAT{} at {}-byte sectors)",
        crate::espboot::BOOTLOADER,
        loader.bytes.len(),
        loader.partition.number,
        loader.fat_bits,
        loader.sector
    );
    crate::espboot::start(our_dp, &loader)
}

/// The attached namespace as an `esp::Disk`: any byte range, read as whole
/// blocks, straight into the caller's buffer when it is block-aligned.
struct NsDisk<'a> {
    ns: &'a mut Namespace,
    bounce: alloc::vec::Vec<u8>,
}

impl crate::esp::Disk for NsDisk<'_> {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        let bs = self.ns.geometry.block_size as u64;
        let len = buf.len() as u64;
        if offset % bs == 0 && len % bs == 0 {
            return self.ns.read(offset / bs, buf).is_ok();
        }
        let first = offset / bs;
        let blocks = (offset + len).div_ceil(bs) - first;
        let n = (blocks * bs) as usize;
        self.bounce.resize(n, 0);
        if self.ns.read(first, &mut self.bounce[..n]).is_err() {
            return false;
        }
        let from = (offset - first * bs) as usize;
        buf.copy_from_slice(&self.bounce[from..from + buf.len()]);
        true
    }
}

/// A handle's device path, read without an exclusive open (drivers hold it).
pub(crate) fn device_path_of(handle: uefi_raw::Handle) -> Option<&'static DevicePath> {
    let p = crate::net::handle_protocol(handle, &DEVICE_PATH_GUID)?;
    Some(unsafe { DevicePath::from_ffi_ptr(p as *const _) })
}

