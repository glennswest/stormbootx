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

use uefi::boot::{self, SearchType};
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

    // Bind the partition and filesystem drivers to the new handle. Installing
    // BlockIO alone leaves a block device nothing has looked at: no GPT is
    // parsed and no ESP appears.
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
