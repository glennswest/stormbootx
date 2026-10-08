//! A read-only `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL` over an ESP that `esp.rs`
//! reads (#42).
//!
//! When the firmware's FAT can't load the attached image's bootloader, the
//! bridge (#37) reads `\EFI\BOOT\BOOTX64.EFI` itself and starts it from a
//! buffer. That is all stormuefi needs: it reads its pallets through
//! whole-disk BlockIO. Any other bootloader opens more files on its own ESP
//! through the `SimpleFileSystem` on its `LoadedImage.DeviceHandle`: shim
//! loads `grubx64.efi` and `mmx64.efi`, systemd-boot reads its loader entries.
//! On the firmware that needed the bridge (AMI Aptio 4, a 4096-byte-sector
//! FAT) that protocol is the firmware's own FAT, the one that answered
//! `NOT_FOUND`, or nothing at all.
//!
//! So `install` puts this one where the bootloader will look. The ESP's
//! partition handle (the disk's path and the ESP's `HD()` node) gets it once
//! the firmware's drivers are disconnected from that handle. If the
//! partition driver made no such handle, a new one carries that path. The
//! image is then loaded under that path, so `LoadImage` names the handle as
//! its `DeviceHandle`.
//!
//! Read-only: `Open` for writing, `Write`, `SetInfo` and `Delete` answer
//! `WRITE_PROTECTED` (`Delete` closes the handle and warns, as the
//! specification says). Directories list as the FAT stores them, `.` and
//! `..` included. `withdraw` takes every one back before the disk under it
//! goes (`blockio::withdraw`), because the functions live in this image.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

use uefi::proto::device_path::DevicePath;
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::device_path::media::{PartitionFormat, PartitionSignature};
use uefi::proto::device_path::{DeviceSubType, DeviceType};
use uefi_raw::protocol::block::BlockIoProtocol;
use uefi_raw::protocol::file_system::{
    FileAttribute, FileInfo, FileMode, FileProtocolRevision, FileProtocolV1, FileSystemInfo,
    FileSystemVolumeLabel, SimpleFileSystemProtocol,
};
use uefi_raw::table::boot::{BootServices, InterfaceType};
use uefi_raw::{Char16, Guid, Handle, Status};

use crate::esp;

/// `EFI_DEVICE_PATH_PROTOCOL`.
const DEVICE_PATH_GUID: Guid = uefi_raw::guid!("09576e91-6d3f-11d2-8e39-00a0c969723b");

fn bs() -> Option<&'static BootServices> {
    unsafe { uefi::table::system_table_raw()?.as_ref().boot_services.as_ref() }
}

fn handle_protocol(h: Handle, guid: &Guid) -> Option<*mut c_void> {
    let mut iface = ptr::null_mut();
    let st = unsafe { (bs()?.handle_protocol)(h, guid, &mut iface) };
    (st == Status::SUCCESS && !iface.is_null()).then_some(iface)
}

fn device_path(h: Handle) -> Option<&'static DevicePath> {
    let p = handle_protocol(h, &DEVICE_PATH_GUID)?;
    Some(unsafe { DevicePath::from_ffi_ptr(p as *const _) })
}

/// A disk's BlockIO as an `esp::Disk`: aligned whole-block reads straight
/// into the caller's buffer, anything else through a bounce buffer.
struct RawDisk {
    bio: *const BlockIoProtocol,
    media_id: u32,
    block: u64,
    bounce: Vec<u8>,
}

impl esp::Disk for RawDisk {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        let bs = self.block;
        let len = buf.len() as u64;
        let read = |lba: u64, out: &mut [u8]| unsafe {
            ((*self.bio).read_blocks)(self.bio, self.media_id, lba, out.len(), out.as_mut_ptr() as *mut c_void)
                == Status::SUCCESS
        };
        if offset % bs == 0 && len % bs == 0 {
            return read(offset / bs, buf);
        }
        let first = offset / bs;
        let blocks = (offset + len).div_ceil(bs) - first;
        let mut bounce = core::mem::take(&mut self.bounce);
        bounce.resize((blocks * bs) as usize, 0);
        let ok = read(first, &mut bounce);
        if ok {
            let from = (offset - first * bs) as usize;
            buf.copy_from_slice(&bounce[from..from + buf.len()]);
        }
        self.bounce = bounce;
        ok
    }
}

/// The protocol first, so the interface pointer is the volume's.
#[repr(C)]
struct Volume {
    proto: SimpleFileSystemProtocol,
    fat: esp::Fat,
    disk: RawDisk,
}

/// An open file or directory. The protocol first, so `this` is the file.
#[repr(C)]
struct File {
    proto: FileProtocolV1,
    vol: *mut Volume,
    entry: esp::Entry,
    /// The name it was opened by (its last component); empty for the root.
    name: Vec<u16>,
    /// A file's read position.
    pos: u64,
    hint: esp::Hint,
    /// A directory's listing, from its first `Read`.
    list: Option<esp::DirCursor>,
}

fn status(e: esp::Error) -> Status {
    match e {
        esp::Error::NotFound | esp::Error::NotAFile => Status::NOT_FOUND,
        esp::Error::Io => Status::DEVICE_ERROR,
        _ => Status::VOLUME_CORRUPTED,
    }
}

fn new_file(vol: *mut Volume, entry: esp::Entry, name: Vec<u16>) -> *mut File {
    Box::into_raw(Box::new(File {
        proto: FileProtocolV1 {
            revision: FileProtocolRevision::REVISION_1,
            open,
            close,
            delete,
            read,
            write,
            get_position,
            set_position,
            get_info,
            set_info,
            flush,
        },
        vol,
        entry,
        name,
        pos: 0,
        hint: esp::Hint::default(),
        list: None,
    }))
}

unsafe extern "efiapi" fn open_volume(this: *mut SimpleFileSystemProtocol, root: *mut *mut FileProtocolV1) -> Status {
    if this.is_null() || root.is_null() {
        return Status::INVALID_PARAMETER;
    }
    *root = new_file(this as *mut Volume, esp::Entry::ROOT, Vec::new()) as *mut FileProtocolV1;
    Status::SUCCESS
}

/// A NUL-terminated UCS-2 string, up to a sane length.
unsafe fn ucs2(p: *const Char16) -> Vec<u16> {
    let mut v = Vec::new();
    let mut p = p as *const u16;
    while *p != 0 && v.len() < 4096 {
        v.push(*p);
        p = p.add(1);
    }
    v
}

unsafe extern "efiapi" fn open(
    this: *mut FileProtocolV1,
    new_handle: *mut *mut FileProtocolV1,
    file_name: *const Char16,
    open_mode: FileMode,
    _attributes: FileAttribute,
) -> Status {
    if this.is_null() || new_handle.is_null() || file_name.is_null() || !open_mode.contains(FileMode::READ) {
        return Status::INVALID_PARAMETER;
    }
    if open_mode.intersects(FileMode::WRITE | FileMode::CREATE) {
        return Status::WRITE_PROTECTED;
    }
    let f = &*(this as *mut File);
    let units = ucs2(file_name);
    let path: String = char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    let vol = &mut *f.vol;
    match vol.fat.lookup_in(&mut vol.disk, &f.entry, &path) {
        Ok(entry) => {
            let last = path.rsplit(['\\', '/']).find(|p| !p.is_empty() && *p != ".");
            let name = match last {
                _ if entry == esp::Entry::ROOT => Vec::new(),
                None => f.name.clone(),
                Some(n) => n.encode_utf16().collect(),
            };
            *new_handle = new_file(f.vol, entry, name) as *mut FileProtocolV1;
            Status::SUCCESS
        }
        Err(e) => status(e),
    }
}

unsafe extern "efiapi" fn close(this: *mut FileProtocolV1) -> Status {
    if !this.is_null() {
        drop(Box::from_raw(this as *mut File));
    }
    Status::SUCCESS
}

unsafe extern "efiapi" fn delete(this: *mut FileProtocolV1) -> Status {
    close(this);
    Status::WARN_DELETE_FAILURE
}

/// The FAT `[date, time]` as an `EFI_TIME`, zero when unset.
fn time_bytes(dt: [u16; 2]) -> [u8; 16] {
    let mut t = [0u8; 16];
    let [d, tm] = dt;
    if d != 0 {
        t[0..2].copy_from_slice(&(1980 + (d >> 9)).to_le_bytes());
        t[2] = ((d >> 5) & 0x0F) as u8;
        t[3] = (d & 0x1F) as u8;
        t[4] = (tm >> 11) as u8;
        t[5] = ((tm >> 5) & 0x3F) as u8;
        t[6] = ((tm & 0x1F) * 2) as u8;
    }
    // EFI_UNSPECIFIED_TIMEZONE: FAT keeps local time and says nothing more.
    t[12..14].copy_from_slice(&0x07FFi16.to_le_bytes());
    t
}

/// `EFI_FILE_INFO` for an entry, as bytes.
fn file_info(vol: &Volume, e: &esp::Entry, name: &[u16]) -> Vec<u8> {
    let size = 80 + (name.len() + 1) * 2;
    let cluster = vol.fat.cluster_size() as u64;
    let file_size = e.size as u64;
    let mut v = Vec::with_capacity(size);
    v.extend_from_slice(&(size as u64).to_le_bytes());
    v.extend_from_slice(&file_size.to_le_bytes());
    v.extend_from_slice(&file_size.div_ceil(cluster).saturating_mul(cluster).to_le_bytes());
    v.extend_from_slice(&time_bytes(e.created));
    v.extend_from_slice(&time_bytes([e.accessed, 0]));
    v.extend_from_slice(&time_bytes(e.modified));
    // FAT's attribute bits are EFI's.
    let attr = (e.attr & 0x37) as u64 | if e.dir { FileAttribute::DIRECTORY.bits() } else { 0 };
    v.extend_from_slice(&attr.to_le_bytes());
    for u in name.iter().chain(&[0]) {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// Copy `bytes` out if they fit, else say how many are needed.
unsafe fn give(bytes: &[u8], size: *mut usize, buffer: *mut c_void) -> Status {
    if *size < bytes.len() {
        *size = bytes.len();
        return Status::BUFFER_TOO_SMALL;
    }
    if buffer.is_null() {
        return Status::INVALID_PARAMETER;
    }
    ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len());
    *size = bytes.len();
    Status::SUCCESS
}

unsafe extern "efiapi" fn read(this: *mut FileProtocolV1, buffer_size: *mut usize, buffer: *mut c_void) -> Status {
    if this.is_null() || buffer_size.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let f = &mut *(this as *mut File);
    let vol = &mut *f.vol;
    if f.entry.dir {
        if f.list.is_none() {
            match vol.fat.open_dir(&f.entry) {
                Ok(c) => f.list = Some(c),
                Err(e) => return status(e),
            }
        }
        let Some(cursor) = f.list.as_mut() else { return Status::DEVICE_ERROR };
        // Look ahead on a copy: a buffer too small must not lose the entry.
        let mut ahead = cursor.clone();
        return match vol.fat.next_entry(&mut vol.disk, &mut ahead) {
            Ok(None) => {
                *buffer_size = 0;
                Status::SUCCESS
            }
            Ok(Some(l)) => {
                let st = give(&file_info(vol, &l.entry, l.name()), buffer_size, buffer);
                if st == Status::SUCCESS {
                    *cursor = ahead;
                }
                st
            }
            Err(e) => status(e),
        };
    }
    if f.pos > f.entry.size as u64 {
        return Status::DEVICE_ERROR;
    }
    if *buffer_size == 0 {
        return Status::SUCCESS;
    }
    if buffer.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let out = core::slice::from_raw_parts_mut(buffer as *mut u8, *buffer_size);
    match vol.fat.read_at(&mut vol.disk, &f.entry, f.pos, out, &mut f.hint) {
        Ok(n) => {
            f.pos += n as u64;
            *buffer_size = n;
            Status::SUCCESS
        }
        Err(e) => status(e),
    }
}

unsafe extern "efiapi" fn write(_this: *mut FileProtocolV1, _size: *mut usize, _buffer: *const c_void) -> Status {
    Status::WRITE_PROTECTED
}

unsafe extern "efiapi" fn get_position(this: *const FileProtocolV1, position: *mut u64) -> Status {
    if this.is_null() || position.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let f = &*(this as *const File);
    if f.entry.dir {
        return Status::UNSUPPORTED;
    }
    *position = f.pos;
    Status::SUCCESS
}

unsafe extern "efiapi" fn set_position(this: *mut FileProtocolV1, position: u64) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let f = &mut *(this as *mut File);
    if f.entry.dir {
        // Only a rewind, which starts the listing again.
        if position != 0 {
            return Status::UNSUPPORTED;
        }
        f.list = None;
        return Status::SUCCESS;
    }
    f.pos = if position == u64::MAX { f.entry.size as u64 } else { position };
    Status::SUCCESS
}

unsafe extern "efiapi" fn get_info(
    this: *mut FileProtocolV1,
    information_type: *const Guid,
    buffer_size: *mut usize,
    buffer: *mut c_void,
) -> Status {
    if this.is_null() || information_type.is_null() || buffer_size.is_null() {
        return Status::INVALID_PARAMETER;
    }
    let f = &*(this as *mut File);
    let vol = &*f.vol;
    let label: Vec<u16> = vol.fat.label().iter().map(|&c| c as u16).collect();
    let ty = *information_type;
    if ty == FileInfo::ID {
        give(&file_info(vol, &f.entry, &f.name), buffer_size, buffer)
    } else if ty == FileSystemInfo::ID {
        let size = 36 + (label.len() + 1) * 2;
        let mut v = Vec::with_capacity(size);
        v.extend_from_slice(&(size as u64).to_le_bytes());
        v.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0]); // read_only, and padding
        v.extend_from_slice(&vol.fat.volume_bytes().to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes()); // free: nothing can be written
        v.extend_from_slice(&vol.fat.cluster_size().to_le_bytes());
        for u in label.iter().chain(&[0]) {
            v.extend_from_slice(&u.to_le_bytes());
        }
        give(&v, buffer_size, buffer)
    } else if ty == FileSystemVolumeLabel::ID {
        let v: Vec<u8> = label.iter().chain(&[0]).flat_map(|u| u.to_le_bytes()).collect();
        give(&v, buffer_size, buffer)
    } else {
        Status::UNSUPPORTED
    }
}

unsafe extern "efiapi" fn set_info(
    _this: *mut FileProtocolV1,
    _information_type: *const Guid,
    _buffer_size: usize,
    _buffer: *const c_void,
) -> Status {
    Status::WRITE_PROTECTED
}

unsafe extern "efiapi" fn flush(_this: *mut FileProtocolV1) -> Status {
    // Nothing is ever written, so there is nothing to flush.
    Status::SUCCESS
}

/// One filesystem this image installed: the handle, our interface, and the
/// device path if the handle was ours too.
struct Installed {
    handle: Handle,
    sfs: *mut c_void,
    path: Option<*mut c_void>,
}

static mut INSTALLED: Vec<Installed> = Vec::new();

/// Whether a device path's last node is the `HD()` of partition `number`.
fn is_partition(dp: &DevicePath, number: u32) -> bool {
    dp.node_iter().last().is_some_and(|n| {
        n.device_type() == DeviceType::MEDIA
            && n.sub_type() == DeviceSubType::MEDIA_HARD_DRIVE
            && n.data().get(..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) == Some(number)
    })
}

/// Put a read-only filesystem over the ESP `p` of the whole disk `disk`
/// (`disk_path` is its device path). Returns the handle it is on, whose
/// device path is the one to load the bootloader under, and what was done.
#[allow(static_mut_refs)]
pub fn install(disk: Handle, disk_path: &DevicePath, p: &esp::Partition) -> Result<(Handle, String), String> {
    let bs = bs().ok_or("no boot services")?;
    let bio = handle_protocol(disk, &BlockIoProtocol::GUID).ok_or("the disk has no BlockIO")? as *const BlockIoProtocol;
    let media = unsafe { &*(*bio).media };
    let mut d = RawDisk { bio, media_id: media.media_id, block: media.block_size as u64, bounce: Vec::new() };
    let block = d.block;
    let fat = esp::Fat::mount(&mut d, p.first_lba * block, p.blocks() * block)
        .map_err(|e| format!("could not mount the ESP: {}", e.describe()))?;
    let vol = Box::into_raw(Box::new(Volume {
        proto: SimpleFileSystemProtocol { revision: 0x0001_0000, open_volume },
        fat,
        disk: d,
    }));
    let sfs_guid = SimpleFileSystemProtocol::GUID;

    // The partition driver's handle for this ESP, if it made one.
    let depth = disk_path.node_iter().count();
    let child = uefi::boot::locate_handle_buffer(uefi::boot::SearchType::ByProtocol(&DEVICE_PATH_GUID))
        .ok()
        .and_then(|handles| {
            handles.iter().map(|h| h.as_ptr()).find(|&h| {
                device_path(h).is_some_and(|dp| {
                    dp.node_iter().count() == depth + 1
                        && dp.node_iter().zip(disk_path.node_iter()).all(|(a, b)| a == b)
                        && is_partition(dp, p.number)
                })
            })
        });

    let iface = vol as *mut c_void;
    let (handle, how, path) = match child {
        Some(h) => {
            let mut how = String::from("on the ESP's partition handle");
            if handle_protocol(h, &sfs_guid).is_some() {
                // The firmware's FAT has it (Aptio 4 mounts a 4K FAT and
                // then can't read it): every driver on the partition comes
                // off, DiskIo with it, which nothing here needs.
                unsafe {
                    let _ = (bs.disconnect_controller)(h, ptr::null_mut(), ptr::null_mut());
                }
                if handle_protocol(h, &sfs_guid).is_some() {
                    unsafe { drop(Box::from_raw(vol)) };
                    return Err(String::from(
                        "the firmware's filesystem would not come off the ESP's partition handle",
                    ));
                }
                how.push_str(", the firmware's FAT disconnected from it");
            }
            let mut h = h;
            let st = unsafe { (bs.install_protocol_interface)(&mut h, &sfs_guid, InterfaceType::NATIVE_INTERFACE, iface) };
            if st != Status::SUCCESS {
                unsafe { drop(Box::from_raw(vol)) };
                return Err(format!("InstallProtocolInterface(SimpleFileSystem) failed: {st:?}"));
            }
            (h, how, None)
        }
        None => {
            // No partition handle: a new one, with the path the image will
            // be loaded under.
            let mut pbuf = Vec::new();
            let mut builder = DevicePathBuilder::with_vec(&mut pbuf);
            for node in disk_path.node_iter() {
                builder = builder.push(&node).map_err(|e| format!("device path: {e:?}"))?;
            }
            let dp = builder
                .push(&build::media::HardDrive {
                    partition_number: p.number,
                    partition_start: p.first_lba,
                    partition_size: p.blocks(),
                    partition_signature: PartitionSignature::Guid(uefi::Guid::from_bytes(p.guid)),
                    partition_format: PartitionFormat::GPT,
                })
                .and_then(|b| b.finalize())
                .map_err(|e| format!("device path: {e:?}"))?;
            let dp = Box::leak(dp.to_boxed()).as_ffi_ptr() as *mut c_void;
            let mut h: Handle = ptr::null_mut();
            unsafe {
                let st = (bs.install_protocol_interface)(&mut h, &DEVICE_PATH_GUID, InterfaceType::NATIVE_INTERFACE, dp);
                if st != Status::SUCCESS {
                    drop(Box::from_raw(vol));
                    return Err(format!("InstallProtocolInterface(DevicePath) failed: {st:?}"));
                }
                let st = (bs.install_protocol_interface)(&mut h, &sfs_guid, InterfaceType::NATIVE_INTERFACE, iface);
                if st != Status::SUCCESS {
                    let _ = (bs.uninstall_protocol_interface)(h, &DEVICE_PATH_GUID, dp);
                    drop(Box::from_raw(vol));
                    return Err(format!("InstallProtocolInterface(SimpleFileSystem) failed: {st:?}"));
                }
            }
            (h, String::from("on a new handle (the partition driver made none for the ESP)"), Some(dp))
        }
    };
    unsafe { INSTALLED.push(Installed { handle, sfs: iface, path }) };
    Ok((handle, how))
}

/// Uninstall every filesystem `install` put up: their functions are in this
/// image, which the firmware unloads when it returns. The volumes are leaked,
/// not freed, in case a file the bootloader left open is still about.
#[allow(static_mut_refs)]
pub fn withdraw() -> usize {
    let Some(bs) = bs() else { return 0 };
    let mut n = 0;
    for i in unsafe { INSTALLED.drain(..) } {
        unsafe {
            if (bs.uninstall_protocol_interface)(i.handle, &SimpleFileSystemProtocol::GUID, i.sfs) == Status::SUCCESS {
                n += 1;
            }
            if let Some(dp) = i.path {
                let _ = (bs.uninstall_protocol_interface)(i.handle, &DEVICE_PATH_GUID, dp);
            }
        }
    }
    n
}

/// Whether `h` carries a filesystem this image installed.
#[allow(static_mut_refs)]
pub fn is_ours(h: Handle) -> bool {
    unsafe { INSTALLED.iter().any(|i| i.handle == h && handle_protocol(h, &SimpleFileSystemProtocol::GUID) == Some(i.sfs)) }
}
