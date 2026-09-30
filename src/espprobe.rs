//! espprobe — can this firmware boot a 4096-byte-sector ESP, and can
//! stormbootx's own reader (#37)?
//!
//! A third standalone binary, for the test in `tests/esp-ovmf.sh` and for a
//! bench check of a new server model. It takes every whole disk except the one
//! it booted from and, for each disk whose GPT has an ESP, reports:
//!
//!   1. **firmware**: whether the firmware's own FAT loads `\EFI\BOOT\BOOTX64.EFI`
//!      from it (it is loaded, never started);
//!   2. **stormbootx**: the same file read with `esp.rs` and started from the
//!      buffer by `espboot.rs`. This is the code `blockio::boot_attached`
//!      falls back to.
//!
//! Then it prints `espprobe: PASS` if the started image came back, and powers
//! the machine off, so a VM test ends by itself.

#![no_main]
#![no_std]

extern crate alloc;

#[path = "esp.rs"]
mod esp;
// `start` is stormbootx's; this binary starts the image itself so it can say
// what came back.
#[allow(dead_code)]
#[path = "espboot.rs"]
mod espboot;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use uefi::boot::{self, LoadImageSource, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::prelude::*;
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::BootPolicy;
use uefi::{cstr16, Identify};

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();
    uefi::println!("espprobe {} - who can read a 4K ESP", env!("CARGO_PKG_VERSION"));
    let result = probe();
    match &result {
        Ok(()) => uefi::println!("espprobe: PASS"),
        Err(e) => uefi::println!("espprobe: FAIL: {e}"),
    }
    boot::stall(core::time::Duration::from_secs(2));
    uefi::runtime::reset(uefi::runtime::ResetType::SHUTDOWN, Status::SUCCESS, None)
}

fn probe() -> Result<(), String> {
    let own = {
        let li = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
            .map_err(|e| format!("LoadedImage: {e:?}"))?;
        li.device()
    };
    let own_path = own.and_then(|h| path_of(h)).map(|p| p.to_boxed());
    // Firmware may have connected only the boot device (OVMF's BDS does), so
    // the disk under test has no BlockIO until every controller is connected.
    if let Ok(all) = boot::locate_handle_buffer(SearchType::AllHandles) {
        for &h in all.iter() {
            let _ = boot::connect_controller(h, &[], None, true);
        }
    }
    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&BlockIO::GUID))
        .map_err(|e| format!("no block devices: {e:?}"))?;
    uefi::println!("block       : {} BlockIO handle(s)", handles.len());

    let mut started = false;
    for &h in handles.iter() {
        let Some(dp) = path_of(h) else { continue };
        // Our own stick, or a partition of it: not the disk under test.
        if own_path.as_deref().is_some_and(|own| is_prefix(dp, own)) {
            continue;
        }
        let bio = unsafe {
            boot::open_protocol::<BlockIO>(
                OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None },
                OpenProtocolAttributes::GetProtocol,
            )
        };
        let Ok(bio) = bio else { continue };
        let media = bio.media();
        if !media.is_media_present() || media.is_logical_partition() {
            continue;
        }
        let block = media.block_size();
        uefi::println!("disk        : {} blocks x {block} bytes", media.last_block() + 1);
        let mut disk = espboot::BlockDisk { bio: &bio, media_id: media.media_id(), block: block as u64, bounce: Vec::new() };
        // What `auto` asks of a local disk (#3): found without reading it.
        let found = espboot::find(&mut disk, block);
        match &found {
            Ok((p, fat, bytes)) => uefi::println!(
                "  local     : ESP partition {}, FAT{fat}, {} ({bytes} bytes)",
                p.number,
                espboot::BOOTLOADER
            ),
            Err(e) => uefi::println!("  local     : nothing to boot: {e}"),
        }
        let loader = match espboot::read(&mut disk, block) {
            Ok(l) => l,
            Err(e) => {
                uefi::println!("  stormbootx: no bootloader here: {e}");
                continue;
            }
        };
        // `find` and `read` must agree, or `auto` would claim over a disk that
        // boots (or fall through to one that doesn't).
        if !found.as_ref().is_ok_and(|(p, _, bytes)| p.number == loader.partition.number && *bytes as usize == loader.bytes.len()) {
            return Err(String::from("espboot::find disagrees with espboot::read"));
        }
        uefi::println!(
            "  stormbootx: read {} ({} bytes; partition {}, FAT{} at {}-byte sectors)",
            espboot::BOOTLOADER,
            loader.bytes.len(),
            loader.partition.number,
            loader.fat_bits,
            loader.sector
        );
        uefi::println!("  firmware  : {}", firmware_loads(dp));
        let image = espboot::load(dp, &loader).map_err(|e| format!("stormbootx's load: {e}"))?;
        uefi::println!("  stormbootx: loaded from the buffer; starting it");
        let status = boot::start_image(image);
        uefi::println!("  started   : the image ran and came back ({status:?})");
        started = true;
    }
    if started {
        Ok(())
    } else {
        Err(String::from("no disk's bootloader was started"))
    }
}

/// Whether the firmware's own FAT loads the same file, and if so whether the
/// bytes match. Loaded and unloaded, never started.
fn firmware_loads(disk: &DevicePath) -> String {
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&SimpleFileSystem::GUID)) else {
        return String::from("no filesystem at all");
    };
    let mut fbuf = Vec::new();
    let Ok(file) = DevicePathBuilder::with_vec(&mut fbuf)
        .push(&build::media::FilePath { path_name: cstr16!("\\EFI\\BOOT\\BOOTX64.EFI") })
        .and_then(|b| b.finalize())
    else {
        return String::from("could not build the path");
    };
    for &h in handles.iter() {
        let Some(dp) = path_of(h) else { continue };
        if dp.node_iter().count() <= disk.node_iter().count() || !is_prefix(disk, dp) {
            continue;
        }
        let Ok(full) = dp.append_path(file) else { continue };
        return match boot::load_image(
            boot::image_handle(),
            LoadImageSource::FromDevicePath { device_path: &full, boot_policy: BootPolicy::ExactMatch },
        ) {
            Ok(img) => {
                let _ = boot::unload_image(img);
                String::from("loads it too")
            }
            Err(e) => format!("mounted the ESP but could not load it: {e:?}"),
        };
    }
    String::from("mounted no filesystem on this disk")
}

/// Whether every node of `a` starts `b`.
fn is_prefix(a: &DevicePath, b: &DevicePath) -> bool {
    a.node_iter().count() <= b.node_iter().count() && a.node_iter().zip(b.node_iter()).all(|(x, y)| x == y)
}

fn path_of(h: Handle) -> Option<&'static DevicePath> {
    let p = unsafe {
        boot::open_protocol::<DevicePath>(
            OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
    }
    .ok()?;
    let r: &DevicePath = &p;
    // The path lives as long as the handle does; the scoped open only records
    // a GetProtocol, which closing does not affect.
    Some(unsafe { &*(r as *const DevicePath) })
}
