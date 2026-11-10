//! Install a release from a stormdist medium, offline (#198).
//!
//! The owner, 2026-11-10: "can stormbootx also boot it, from usb?" (the
//! stormdist release ISO). Decided on #198 (the master for the owner):
//! stormbootx finds the medium, checks it, says how fast it reads, and
//! starts the release's own boot from it; the release's initramfs lays the
//! disk from the medium (stormblock#442). There is one layout writer and it
//! is not here (#166).
//!
//! - **find**, before the network: every present, whole BlockIO (a CD at
//!   2048-byte blocks, a stick written from the ISO at 512) whose ISO 9660
//!   volume carries `RELEASE.toml` and `BOOT.toml` (`distmedia.rs`). Both
//!   signatures are checked against the release key (`distmedia::DIST_KEYS`)
//!   before a byte of either is believed, and `RELEASE.toml`'s SHA-256
//!   against `BOOT.toml`'s `release_sha256`.
//! - **install**: `boot/esp.img` read whole through BlockIO and timed (the
//!   medium's read rate, its bus, and what the release's chunks would take
//!   at that rate), hashed against `esp_sha256`, and its `BOOTX64.EFI` read
//!   out of those checked bytes (`esp.rs`), so the loader that runs is the
//!   one the signature covers. It is `LoadImage`d from that buffer under the
//!   device path of the firmware's own filesystem on the medium's ESP (the
//!   El Torito entry on a CD, the hybrid GPT's partition on a stick), so
//!   that filesystem is its `DeviceHandle`, exactly as when the firmware
//!   boots the medium itself: stormuefi then opens its kernel and initramfs
//!   there.
//!
//! When (`install_source =`, `config::InstallSource`): `local` installs
//! before the network; by default forge first, and the medium only when the
//! network boot gave no image **and** nothing local boots, so an installed
//! node with a release stick left in boots its own disk; `off` never.
//! Every failure here goes on as if there were no medium.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use uefi::boot::{self, LoadImageSource, SearchType};
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::device_path::DevicePath;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{Handle, Identify};

use crate::distmedia::{self, Boot, Layout, Medium, Release};
use crate::espboot::BlockDisk;

/// A test key for `tests/dist-ovmf.sh` only, compiled in when the build sets
/// `STORMBOOTX_DIST_TEST_KEY`. No golden build does (`build-golden.sh`
/// refuses it).
fn test_key() -> Option<[u8; 32]> {
    option_env!("STORMBOOTX_DIST_TEST_KEY").and_then(distmedia::hex32)
}

fn keys() -> Vec<[u8; 32]> {
    let mut k = distmedia::DIST_KEYS.to_vec();
    if let Some(t) = test_key() {
        k.push(t);
    }
    k
}

/// A checked stormdist medium.
pub struct Found {
    disk: Handle,
    layout: Layout,
    boot: Boot,
    release: Release,
    /// Its device path as text, for the console.
    path: String,
    /// `usb`, `cd`, `sata`, … from the device path.
    bus: &'static str,
}

impl Found {
    /// `12.74.6 (full)`, for the console.
    pub fn name(&self) -> String {
        format!(
            "{} ({})",
            self.release.version.as_deref().unwrap_or("?"),
            self.release.kind.as_deref().unwrap_or("?")
        )
    }
}

/// BlockIO as `distmedia::Medium`.
struct Disk<'a>(BlockDisk<'a>);

impl Medium for Disk<'_> {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        // Past the end (a 1 MiB disk has no sector 16) is "not ISO", not a
        // firmware error.
        let end = (self.0.bio.media().last_block() + 1).saturating_mul(self.0.block);
        offset + buf.len() as u64 <= end && crate::esp::Disk::read(&mut self.0, offset, buf)
    }
}

fn read_all(d: &mut Disk, e: distmedia::Extent) -> Option<Vec<u8>> {
    let mut b = vec![0u8; e.len as usize];
    d.read(e.offset, &mut b).then_some(b)
}

/// What bus a device path runs over, from its text.
fn bus_of(path: &str) -> &'static str {
    if path.contains("USB(") || path.contains("UsbClass(") || path.contains("UsbWwid(") {
        "usb"
    } else if path.contains("CDROM(") {
        "cd"
    } else if path.contains("Sata(") {
        "sata"
    } else if path.contains("NVMe(") {
        "nvme"
    } else if path.contains("Scsi(") {
        "scsi"
    } else {
        "disk"
    }
}

/// The checked stormdist medium, if one is attached. Prints a line for each
/// medium that is one, checked or refused; ordinary disks say nothing.
pub fn find() -> Option<Found> {
    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&BlockIO::GUID)).ok()?;
    let keys = keys();
    if test_key().is_some() {
        uefi::println!("dist        : a TEST key is compiled in (STORMBOOTX_DIST_TEST_KEY)");
    }
    for &h in handles.iter() {
        let Some(bio) = crate::blockio::open_get::<BlockIO>(h) else { continue };
        let media = bio.media();
        if !media.is_media_present() || media.is_logical_partition() || crate::blockio::is_published(h.as_ptr()) {
            continue;
        }
        let block = media.block_size();
        let mut d = Disk(BlockDisk { bio: &bio, media_id: media.media_id(), block: block as u64, bounce: Vec::new() });
        let layout = match distmedia::layout(&mut d) {
            Ok(l) => l,
            Err(distmedia::Error::NotIso | distmedia::Error::NotRelease) => continue,
            Err(e) => {
                let path = crate::blockio::path_of(h).and_then(crate::hardware::text).unwrap_or_default();
                uefi::println!("dist        : {path}: not used: {}", e.describe());
                continue;
            }
        };
        let path = crate::blockio::path_of(h).and_then(crate::hardware::text).unwrap_or_default();
        let bus = bus_of(&path);
        match check(&mut d, &layout, &keys) {
            Ok((boot, release)) => {
                let f = Found { disk: h, layout, boot, release, path, bus };
                uefi::println!(
                    "dist        : stormdist {} on {} ({}, {block}-byte blocks): BOOT.sig and RELEASE.sig verify, RELEASE.toml matches BOOT.toml",
                    f.name(),
                    f.path,
                    f.bus
                );
                crate::blockio::not_local(h);
                return Some(f);
            }
            Err(why) => uefi::println!("dist        : {path}: a stormdist medium, refused: {why}"),
        }
    }
    None
}

fn check(d: &mut Disk, l: &Layout, keys: &[[u8; 32]]) -> Result<(Boot, Release), String> {
    let get = |d: &mut Disk, e, what: &str| read_all(d, e).ok_or_else(|| format!("{what} could not be read"));
    let verify = |text: &[u8], sig: &[u8], what: &str| -> Result<(), String> {
        let mut s = [0u8; 64];
        s.copy_from_slice(sig);
        let sig = ed25519_compact::Signature::new(s);
        if keys.iter().any(|k| ed25519_compact::PublicKey::new(*k).verify(text, &sig).is_ok()) {
            Ok(())
        } else {
            Err(format!("{what} does not verify against the {} compiled-in release key(s)", keys.len()))
        }
    };
    // The signatures, before a byte of either file is believed.
    let boot_text = get(d, l.boot_toml, "BOOT.toml")?;
    verify(&boot_text, &get(d, l.boot_sig, "BOOT.sig")?, "BOOT.sig")?;
    let release_text = get(d, l.release_toml, "RELEASE.toml")?;
    verify(&release_text, &get(d, l.release_sig, "RELEASE.sig")?, "RELEASE.sig")?;
    let boot = distmedia::parse_boot(core::str::from_utf8(&boot_text).map_err(|_| "BOOT.toml is not UTF-8")?)?;
    if crate::sha256::digest(&release_text).as_bytes() != &boot.release_sha256 {
        return Err(String::from("RELEASE.toml is not the release BOOT.toml names (release_sha256)"));
    }
    let release = distmedia::parse_release(core::str::from_utf8(&release_text).unwrap_or(""));
    Ok((boot, release))
}

/// TSC ticks per microsecond, measured against `Stall`.
fn ticks_per_us() -> u64 {
    let t = crate::entropy::ticks();
    boot::stall(core::time::Duration::from_millis(10));
    ((crate::entropy::ticks() - t) / 10_000).max(1)
}

/// Read the ESP, check it, and start the release's bootloader. Returns only
/// when it did not start, or came back.
pub fn install(f: &Found) -> Result<(), String> {
    uefi::println!("dist        : installing stormdist {} from {} ({})", f.name(), f.path, f.bus);
    crate::ui::next(&format!("installing stormdist {} from the {} medium", f.name(), f.bus));

    // 1. The ESP, whole, timed: the medium's read rate.
    let esp = {
        let bio = crate::blockio::open_get::<BlockIO>(f.disk).ok_or("the medium has no BlockIO now")?;
        let media = bio.media();
        let mut d = Disk(BlockDisk { bio: &bio, media_id: media.media_id(), block: media.block_size() as u64, bounce: Vec::new() });
        let per_us = ticks_per_us();
        let mut esp = vec![0u8; f.layout.esp.len as usize];
        let t0 = crate::entropy::ticks();
        const CHUNK: usize = 4 << 20;
        for (i, c) in esp.chunks_mut(CHUNK).enumerate() {
            if !d.read(f.layout.esp.offset + (i * CHUNK) as u64, c) {
                return Err(format!("boot/esp.img could not be read at {} MiB", (i * CHUNK) >> 20));
            }
        }
        let us = (crate::entropy::ticks() - t0) / per_us;
        uefi::println!(
            "dist        : read boot/esp.img: {}",
            distmedia::rate_line(esp.len() as u64, us, f.bus, f.release.bytes)
        );
        esp
    };
    crate::profile::mark("dist read");

    // 2. Is it the ESP the signature covers?
    if crate::sha256::digest(&esp).as_bytes() != &f.boot.esp_sha256 {
        return Err(String::from("boot/esp.img is not the ESP BOOT.toml names (esp_sha256): refused"));
    }
    let loader = {
        let mut m = Mem(&esp);
        let why = |e: crate::esp::Error| format!("the release's ESP: {}", e.describe());
        let mut fat = Box::new(crate::esp::Fat::mount(&mut m, 0, esp.len() as u64).map_err(why)?);
        let entry = fat.lookup(&mut m, crate::espboot::BOOTLOADER).map_err(why)?;
        if entry.dir || entry.size == 0 {
            return Err(format!("the release's ESP has no {}", crate::espboot::BOOTLOADER));
        }
        let mut bytes = vec![0u8; entry.size as usize];
        fat.read_file(&mut m, &entry, &mut bytes).map_err(why)?;
        bytes
    };
    uefi::println!(
        "dist        : boot/esp.img matches BOOT.toml (esp_sha256); {} is {} bytes",
        crate::espboot::BOOTLOADER,
        loader.len()
    );
    crate::profile::mark("dist check");

    // 3. The firmware's filesystem on the medium's ESP: its DeviceHandle.
    let fs_path = esp_filesystem(f)?;
    let mut buf = Vec::new();
    let mut b = DevicePathBuilder::with_vec(&mut buf);
    for node in fs_path.node_iter() {
        b = b.push(&node).map_err(|e| format!("device path: {e:?}"))?;
    }
    let path = b
        .push(&build::media::FilePath { path_name: crate::espboot::BOOTLOADER16 })
        .and_then(|b| b.finalize())
        .map_err(|e| format!("device path: {e:?}"))?;
    let image = boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromBuffer { buffer: &loader, file_path: Some(path) },
    )
    .map_err(|e| format!("LoadImage refused the release's {}: {e:?}", crate::espboot::BOOTLOADER))?;
    crate::profile::mark("dist start");
    crate::profile::print();
    uefi::println!(
        "boot        : starting {} from the stormdist medium (checked by stormbootx); the release installs itself from it",
        crate::espboot::BOOTLOADER
    );
    crate::netlog::flush();
    crate::ui::end_saying("handing over to the release on the stormdist medium");
    match boot::start_image(image) {
        Ok(()) => Err(String::from("the release's bootloader exited")),
        Err(e) => Err(format!("the release's bootloader returned {e:?}")),
    }
}

/// The device path of the filesystem the firmware mounted on the medium's
/// ESP: one node below the medium's whole disk.
fn esp_filesystem(f: &Found) -> Result<Box<DevicePath>, String> {
    let disk = crate::blockio::path_of(f.disk).ok_or("the medium has no device path")?;
    let depth = disk.node_iter().count();
    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&SimpleFileSystem::GUID))
        .map_err(|e| format!("no filesystem at all ({e:?})"))?;
    for h in handles.iter() {
        let Some(dp) = crate::blockio::device_path_of(h.as_ptr()) else { continue };
        if dp.node_iter().count() == depth + 1 && crate::blockio::is_prefix(disk, dp) {
            return Ok(dp.to_boxed());
        }
    }
    Err(String::from(
        "the firmware mounted no filesystem on the medium's ESP, so the release's bootloader would find none of its files",
    ))
}

/// The checked ESP bytes as an `esp::Disk`.
struct Mem<'a>(&'a [u8]);

impl crate::esp::Disk for Mem<'_> {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        let o = offset as usize;
        match self.0.get(o..o + buf.len()) {
            Some(s) => {
                buf.copy_from_slice(s);
                true
            }
            None => false,
        }
    }
}
