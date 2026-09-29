//! Start an ESP's bootloader without the firmware's FAT driver (#37).
//!
//! `esp.rs` finds and reads `\EFI\BOOT\BOOTX64.EFI` through any `esp::Disk`,
//! and this hands the bytes to `LoadImage` from a buffer with a device path
//! that names where they came from: the disk, the ESP's `HD()` node, and the
//! file. Shared by `blockio.rs` (the attached namespace) and `espprobe`
//! (a local disk under OVMF), so the test and the boot run the same code.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use uefi::boot::{self, LoadImageSource};
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::device_path::media::{PartitionFormat, PartitionSignature};
use uefi::proto::device_path::DevicePath;
use uefi::{cstr16, Guid, Handle};

use crate::esp;

/// The path every bootable ESP carries.
pub const BOOTLOADER: &str = "\\EFI\\BOOT\\BOOTX64.EFI";

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

/// `LoadImage` the bytes. `disk` is the device path of the whole disk they
/// were read from; the image's path is that, the ESP's `HD()` node and the
/// file, so it names where the bytes came from.
pub fn load(disk: &DevicePath, b: &Bootloader) -> Result<Handle, String> {
    let p = &b.partition;
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
        .and_then(|b| b.push(&build::media::FilePath { path_name: cstr16!("\\EFI\\BOOT\\BOOTX64.EFI") }))
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
pub fn start(disk: &DevicePath, b: &Bootloader) -> Result<(), String> {
    let image = load(disk, b)?;
    uefi::println!("boot        : starting {BOOTLOADER} from the attached image (read by stormbootx)");
    match boot::start_image(image) {
        Ok(()) => Err(String::from("the image's bootloader exited")),
        Err(e) => Err(format!("the image's bootloader returned {e:?}")),
    }
}
