//! Publish the remote namespace as `EFI_BLOCK_IO_PROTOCOL`.
//!
//! This is the point of the whole extension. Once a handle carries BlockIO,
//! the firmware's own machinery takes over: the partition driver reads the GPT
//! and produces a handle per partition, the FAT driver mounts the ESP, and the
//! boot manager can load an image from a disk that does not exist on this
//! machine. Nothing above needs to know the blocks arrive over TCP.
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

use uefi::boot::{self, LoadImageSource, SearchType};
use uefi::proto::BootPolicy;
use uefi::proto::device_path::DevicePath;
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{Handle, cstr16, guid};
use uefi::Status;
use uefi_raw::protocol::block::{BlockIoMedia, BlockIoProtocol};
use uefi_raw::Boolean;
use uefi_raw::Guid;

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
        match ns.read(lba, slice) {
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
            let Some(p) = crate::tcp4::handle_protocol(h.as_ptr(), &BlockIoProtocol::GUID) else {
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
        let mut buf = alloc::vec![0u8; 32];
        let path = DevicePathBuilder::with_buf(&mut buf)
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
/// Only returns on failure — a started image that itself exits comes back, and
/// that is the caller's cue to fall through to whatever else there is.
pub fn boot_attached(disk: uefi_raw::Handle) -> Result<(), String> {
    let file = cstr16!("\\EFI\\BOOT\\BOOTX64.EFI");
    let mut fbuf = [0u8; 64];
    let file_path = DevicePathBuilder::with_buf(&mut fbuf)
        .push(&build::media::FilePath { path_name: file })
        .and_then(|b| b.finalize())
        .map_err(|e| format!("file path build failed: {e:?}"))?;

    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&SimpleFileSystem::GUID))
        .map_err(|e| format!("no filesystems to boot: {e:?}"))?;

    // Prefer an ESP that is a partition of the disk we just published — its
    // device path carries our vendor GUID. On a diskless target it is the only
    // one; where a local disk also has an ESP, this is what keeps us off it.
    let mut ordered: alloc::vec::Vec<uefi_raw::Handle> =
        handles.iter().map(|h| h.as_ptr()).collect();
    ordered.sort_by_key(|&h| !device_path_has_guid(h, &DISK_DP_GUID));

    let image = boot::image_handle();
    let mut last = String::from("no ESP carried a bootloader");
    for h in ordered {
        let Some(dp) = device_path_of(h) else { continue };
        let full = match dp.append_path(file_path) {
            Ok(f) => f,
            Err(e) => {
                last = format!("append_path failed: {e:?}");
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
                let _ = disk;
                uefi::println!("boot        : starting \\EFI\\BOOT\\BOOTX64.EFI from the image");
                // Returns only if the started image exits — then keep trying,
                // and failing that, fall through.
                match boot::start_image(loaded) {
                    Ok(()) => last = String::from("the image's bootloader exited"),
                    Err(e) => last = format!("the image's bootloader returned {e:?}"),
                }
            }
            Err(e) => last = format!("load of BOOTX64.EFI failed: {e:?}"),
        }
    }
    Err(last)
}

/// A handle's device path, read without an exclusive open (drivers hold it).
fn device_path_of(handle: uefi_raw::Handle) -> Option<&'static DevicePath> {
    let p = crate::tcp4::handle_protocol(handle, &DEVICE_PATH_GUID)?;
    Some(unsafe { DevicePath::from_ffi_ptr(p as *const _) })
}

/// Does a handle's device path contain a vendor node with this GUID?
fn device_path_has_guid(handle: uefi_raw::Handle, g: &Guid) -> bool {
    let Some(dp) = device_path_of(handle) else {
        return false;
    };
    dp.node_iter().any(|node| node.data().len() >= 16 && &node.data()[..16] == g.as_bytes())
}
