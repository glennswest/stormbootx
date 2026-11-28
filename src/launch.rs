//! `stormbootx-launch` (#229): a writable medium's removable-media
//! bootloader, which nothing renames or rewrites.
//!
//! It reads the boot record (`bootrec.rs`: `\stormboot\boot.a`, `boot.b`)
//! from the volume it was loaded from, and starts the payload the newest
//! valid record names, its SHA-256 checked first; if that one cannot be read
//! or loaded, the older record's, then `\stormboot\stormbootx.efi`. Each
//! payload is loaded from the bytes it checked, under its own file path, so
//! the payload's `LoadedImage` names the volume and the file it came from.
//! The payload's status is this image's.
//!
//! It writes nothing. Counting a trial, putting a release back and writing
//! records are stormbootx's (`selfupdate.rs`); this only has to be there and
//! choose, so a power loss at any step of an update leaves it in place and
//! one of the records whole. Quiet unless it passes over something.

#![cfg_attr(not(test), no_main)]
#![no_std]

extern crate alloc;
#[cfg(test)]
#[macro_use]
extern crate std;

#[allow(dead_code)]
#[path = "bootrec.rs"]
mod bootrec;
#[path = "sha256.rs"]
mod sha256;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use uefi::boot::{self, LoadImageSource};
use uefi::prelude::*;
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::file::{Directory, File, FileAttribute, FileMode, FileType};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::CStr16;

/// The most a payload may be; stormbootx is well under 1 MiB.
const MAX_PAYLOAD: usize = 16 << 20;

fn digest(b: &[u8]) -> [u8; 32] {
    *sha256::digest(b).as_bytes()
}

#[entry]
fn main() -> Status {
    if uefi::helpers::init().is_err() {
        return Status::LOAD_ERROR;
    }
    match launch() {
        Ok(status) => status,
        Err(e) => {
            uefi::println!("launch      : {e}");
            boot::stall(core::time::Duration::from_secs(5));
            Status::LOAD_ERROR
        }
    }
}

/// `stormboot/x.efi` as `\stormboot\x.efi`.
fn efi_path(p: &str) -> String {
    format!("\\{}", p.replace('/', "\\"))
}

/// Up to `limit` bytes of a file; `None` when there is no such file.
fn read(root: &mut Directory, path: &str, limit: usize) -> Result<Option<Vec<u8>>, String> {
    let mut buf = [0u16; 128];
    let p16 = CStr16::from_str_with_buf(&efi_path(path), &mut buf).map_err(|_| format!("{path}: not a usable name"))?;
    let mut f = match root.open(p16, FileMode::Read, FileAttribute::empty()) {
        Ok(h) => match h.into_type() {
            Ok(FileType::Regular(f)) => f,
            Ok(FileType::Dir(_)) => return Err(format!("{path} is a directory")),
            Err(e) => return Err(format!("{path}: {:?}", e.status())),
        },
        Err(e) if e.status() == Status::NOT_FOUND => return Ok(None),
        Err(e) => return Err(format!("open {path}: {:?}", e.status())),
    };
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut chunk).map_err(|e| format!("read {path}: {:?}", e.status()))?;
        if n == 0 {
            break;
        }
        if out.len() + n > limit {
            return Err(format!("{path} is over {limit} bytes"));
        }
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(Some(out))
}

fn launch() -> Result<Status, String> {
    let volume = {
        let li = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
            .map_err(|e| format!("LoadedImage: {:?}", e.status()))?;
        li.device().ok_or("LoadedImage names no volume")?
    };
    let image = {
        let mut fs = boot::open_protocol_exclusive::<SimpleFileSystem>(volume)
            .map_err(|e| format!("the boot volume has no file system ({:?})", e.status()))?;
        let mut root = fs.open_volume().map_err(|e| format!("open_volume: {:?}", e.status()))?;
        let vol_dp = boot::open_protocol_exclusive::<DevicePath>(volume)
            .map_err(|e| format!("the boot volume has no device path ({:?})", e.status()))?;

        let mut raw: [Option<Vec<u8>>; 2] = [None, None];
        for (slot, file) in raw.iter_mut().zip(bootrec::SLOTS) {
            match read(&mut root, file, bootrec::MAX_RECORD + 1) {
                Ok(b) => *slot = b,
                Err(e) => uefi::println!("launch      : {e}; that record is ignored"),
            }
        }
        let mut slots = [None, None];
        for ((slot, bytes), file) in slots.iter_mut().zip(&raw).zip(bootrec::SLOTS) {
            if let Some(b) = bytes {
                *slot = bootrec::parse(b, digest);
                if slot.is_none() {
                    uefi::println!("launch      : {} is not a whole record; ignored", efi_path(file));
                }
            }
        }

        let mut found = None;
        for (path, want) in bootrec::order(&slots).into_iter().flatten() {
            match load(&mut root, &vol_dp, path, want) {
                Ok(image) => {
                    found = Some(image);
                    break;
                }
                Err(e) => uefi::println!("launch      : {e}; trying the next"),
            }
        }
        found.ok_or("no payload to start: nothing on this medium loads; returning to the firmware")?
        // The file system and the volume's device path close here, before
        // the payload opens them for itself.
    };
    Ok(match boot::start_image(image) {
        Ok(()) => Status::SUCCESS,
        Err(e) => e.status(),
    })
}

/// Read `path`, check it against the record's SHA-256, and load it under
/// its own file path on the volume.
fn load(root: &mut Directory, volume: &DevicePath, path: &str, want: Option<[u8; 32]>) -> Result<Handle, String> {
    let bytes = read(root, path, MAX_PAYLOAD)?.ok_or_else(|| format!("{} is not there", efi_path(path)))?;
    if let Some(want) = want {
        if digest(&bytes) != want {
            return Err(format!("{} does not match its record's sha256 ({} bytes)", efi_path(path), bytes.len()));
        }
    }
    let mut nbuf = [0u16; 128];
    let name = CStr16::from_str_with_buf(&efi_path(path), &mut nbuf).map_err(|_| "path too long")?;
    let mut buf = Vec::new();
    let mut builder = DevicePathBuilder::with_vec(&mut buf);
    for node in volume.node_iter() {
        builder = builder.push(&node).map_err(|e| format!("device path: {e:?}"))?;
    }
    let full = builder
        .push(&build::media::FilePath { path_name: name })
        .and_then(|b| b.finalize())
        .map_err(|e| format!("device path: {e:?}"))?;
    boot::load_image(boot::image_handle(), LoadImageSource::FromBuffer { buffer: &bytes, file_path: Some(full) })
        .map_err(|e| format!("{}: LoadImage {:?}", efi_path(path), e.status()))
}
