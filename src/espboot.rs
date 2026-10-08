//! Start an ESP's bootloader without the firmware's FAT driver (#37).
//!
//! `esp.rs` finds and reads `\EFI\BOOT\BOOTX64.EFI` through any `esp::Disk`,
//! and this hands the bytes to `LoadImage` from a buffer with a device path
//! that names where they came from: the disk, the ESP's `HD()` node, and the
//! file. Shared by `blockio.rs` (the attached namespace, and the local disks
//! `auto` looks at, #3) and `espprobe` (a local disk under OVMF), so the test
//! and the boot run the same code.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use uefi::boot::{self, LoadImageSource};
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::device_path::media::{PartitionFormat, PartitionSignature};
use uefi::proto::device_path::DevicePath;
use uefi::proto::media::block::BlockIO;
use uefi::{cstr16, Guid, Handle};

use crate::esp;

/// The path every bootable ESP carries.
pub const BOOTLOADER: &str = "\\EFI\\BOOT\\BOOTX64.EFI";

/// A firmware BlockIO as an `esp::Disk`.
pub struct BlockDisk<'a> {
    pub bio: &'a BlockIO,
    pub media_id: u32,
    pub block: u64,
    pub bounce: Vec<u8>,
}

impl esp::Disk for BlockDisk<'_> {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        let bs = self.block;
        let first = offset / bs;
        let blocks = (offset + buf.len() as u64).div_ceil(bs) - first;
        self.bounce.resize((blocks * bs) as usize, 0);
        if self.bio.read_blocks(self.media_id, first, &mut self.bounce).is_err() {
            return false;
        }
        let from = (offset - first * bs) as usize;
        buf.copy_from_slice(&self.bounce[from..from + buf.len()]);
        true
    }
}

/// A bootloader read off an ESP, and where from.
pub struct Bootloader {
    pub bytes: Vec<u8>,
    pub partition: esp::Partition,
    pub fat_bits: u8,
    pub sector: u32,
}

/// Read `BOOTLOADER` off the first ESP of a disk of `block_size`-byte blocks.
pub fn read<D: esp::Disk>(disk: &mut D, block_size: u32) -> Result<Bootloader, String> {
    let why = |e: esp::Error| String::from(e.describe());
    let partition = esp::find_esp(disk, block_size).map_err(why)?;
    let bs = block_size as u64;
    let mut fat = alloc::boxed::Box::new(
        esp::Fat::mount(disk, partition.first_lba * bs, partition.blocks() * bs).map_err(why)?,
    );
    let entry = fat.lookup(disk, BOOTLOADER).map_err(why)?;
    let mut bytes = vec![0u8; entry.size as usize];
    fat.read_file(disk, &entry, &mut bytes).map_err(why)?;
    Ok(Bootloader { bytes, partition, fat_bits: fat.kind(), sector: fat.sector_size() })
}

/// Whether a disk's first ESP carries `BOOTLOADER`, without reading it: the
/// ESP, its FAT width and the file's size. A zero-byte file is not a
/// bootloader.
pub fn find<D: esp::Disk>(disk: &mut D, block_size: u32) -> Result<(esp::Partition, u8, u32), String> {
    let why = |e: esp::Error| String::from(e.describe());
    let partition = esp::find_esp(disk, block_size).map_err(why)?;
    let bs = block_size as u64;
    let mut fat = alloc::boxed::Box::new(
        esp::Fat::mount(disk, partition.first_lba * bs, partition.blocks() * bs).map_err(why)?,
    );
    let entry = fat.lookup(disk, BOOTLOADER).map_err(why)?;
    if entry.dir || entry.size == 0 {
        return Err(format!("{BOOTLOADER} is empty or a directory"));
    }
    Ok((partition, fat.kind(), entry.size))
}

/// The ESP's own device path: the disk's, and the ESP's `HD()` node.
pub fn partition_path(disk: &DevicePath, p: &esp::Partition) -> Result<alloc::boxed::Box<DevicePath>, String> {
    let mut buf = Vec::new();
    let mut builder = DevicePathBuilder::with_vec(&mut buf);
    for node in disk.node_iter() {
        builder = builder.push(&node).map_err(|e| format!("device path: {e:?}"))?;
    }
    let path = builder
        .push(&build::media::HardDrive {
            partition_number: p.number,
            partition_start: p.first_lba,
            partition_size: p.blocks(),
            partition_signature: PartitionSignature::Guid(Guid::from_bytes(p.guid)),
            partition_format: PartitionFormat::GPT,
        })
        .and_then(|b| b.finalize())
        .map_err(|e| format!("device path: {e:?}"))?;
    Ok(path.to_boxed())
}

/// `LoadImage` the bytes under `esp` (the ESP's device path) and the file,
/// so the image's path names where they came from. When a filesystem
/// carries that path (`espfs.rs`, #42), `LoadImage` makes it the image's
/// `DeviceHandle`, which is where a bootloader opens its other files.
pub fn load(esp: &DevicePath, b: &Bootloader) -> Result<Handle, String> {
    let mut buf = Vec::new();
    let mut builder = DevicePathBuilder::with_vec(&mut buf);
    for node in esp.node_iter() {
        builder = builder.push(&node).map_err(|e| format!("device path: {e:?}"))?;
    }
    let path = builder
        .push(&build::media::FilePath { path_name: cstr16!("\\EFI\\BOOT\\BOOTX64.EFI") })
        .and_then(|b| b.finalize())
        .map_err(|e| format!("device path: {e:?}"))?;

    boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromBuffer { buffer: &b.bytes, file_path: Some(path) },
    )
    .map_err(|e| format!("LoadImage refused the {} bytes read: {e:?}", b.bytes.len()))
}

/// `load`, then `StartImage`. Only returns on failure, or when the started
/// image exits.
pub fn start(esp: &DevicePath, b: &Bootloader) -> Result<(), String> {
    let image = load(esp, b)?;
    uefi::println!("boot        : starting {BOOTLOADER} from the attached image (read by stormbootx)");
    match boot::start_image(image) {
        Ok(()) => Err(String::from("the image's bootloader exited")),
        Err(e) => Err(format!("the image's bootloader returned {e:?}")),
    }
}
