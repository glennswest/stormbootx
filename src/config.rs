//! Where to attach, and the volume this binary booted from.
//!
//! Nothing about a portal belongs in a compiled binary. Over one afternoon the
//! target here moved host (dev.g8.lo → forge.g16.lo) and then changed NQN
//! (`…lo.g8:stormcos` → `…lo.g16:stormcos`), and each time a compiled-in value
//! meant a machine that could not boot until someone rebuilt and rewrote a
//! stick. Configuration lives next to the binary instead, so the fix is a text
//! edit on the media.
//!
//! The volume is found through `EFI_LOADED_IMAGE_PROTOCOL`, which hands back
//! the device this image was loaded from. That is exact — no probing for
//! "something that looks like our ESP", no risk of writing to a partition that
//! belongs to somebody else, and it is the same handle the self-update path
//! needs later.
//!
//! Resolution order, first hit wins:
//!
//!   1. `\stormboot\stormboot.conf` on the volume we booted from
//!   2. DNS — `_nvme-disc._tcp.<zone>`, see `dns.rs`
//!   3. compiled-in defaults
//!
//! A `portal` line in the file **pins** the machine: it is the override for a
//! box that must attach somewhere specific, and it disables discovery outright
//! rather than merely outranking it. A stick with no `portal` line discovers,
//! which is the case that makes a machine survive being moved to another
//! network with nothing edited.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use uefi::boot::{self, ScopedProtocol};
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::file::{
    File, FileAttribute, FileInfo, FileMode, FileType, RegularFile,
};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{CStr16, Handle};

/// Path on the ESP holding both the config and the update stamp.
pub const CONF_PATH: &str = r"\stormboot\stormboot.conf";

/// What the extension needs in order to attach.
#[derive(Debug, Clone)]
pub struct Config {
    pub portal: [u8; 4],
    pub port: u16,
    pub nqn: String,
    pub nsid: u32,
    /// Digest of the `BOOTX64.EFI` currently on this media, if it has been
    /// stamped. Absent on a stick written by `dd` and never updated.
    pub stamp: Option<String>,
    /// Where the target came from, for the console line. Worth spelling out:
    /// "the file said so" and "a resolver said so" are different failures when
    /// a machine attaches somewhere unexpected.
    pub source: String,
}

/// What the binary falls back to when nothing else answers.
///
/// A floor, not a configuration. Every field here is something that has
/// already changed under this project once.
pub struct Defaults {
    pub portal: [u8; 4],
    pub port: u16,
    pub nqn: &'static str,
    pub nsid: u32,
    /// The DNS zone holding the `_nvme-disc._tcp` record. Deliberately not a
    /// per-network domain — see the note in `dns.rs`.
    pub zone: &'static str,
}

pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.trim().split('.');
    for slot in out.iter_mut() {
        *slot = parts.next()?.trim().parse::<u8>().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// The handle of the volume this image was loaded from.
///
/// Public because the self-update path writes back to exactly this volume and
/// must not go looking for it a second time by a different route.
pub fn boot_volume() -> Option<Handle> {
    let li = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()).ok()?;
    li.device()
}

fn open_fs(handle: Handle) -> Option<ScopedProtocol<SimpleFileSystem>> {
    boot::open_protocol_exclusive::<SimpleFileSystem>(handle).ok()
}

/// Read a file from the boot volume as text.
pub fn read_file(path: &str) -> Option<String> {
    let handle = boot_volume()?;
    let mut fs = open_fs(handle)?;
    let mut root = fs.open_volume().ok()?;

    let mut buf = [0u16; 256];
    let path16 = CStr16::from_str_with_buf(path, &mut buf).ok()?;
    let file = root
        .open(path16, FileMode::Read, FileAttribute::empty())
        .ok()?;
    let mut file: RegularFile = match file.into_type().ok()? {
        FileType::Regular(f) => f,
        FileType::Dir(_) => return None,
    };

    // Config files here are a few hundred bytes; refuse anything that is not,
    // rather than allocating whatever a corrupt directory entry claims.
    let mut out = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = file.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        if out.len() > 64 * 1024 {
            return None;
        }
    }
    String::from_utf8(out).ok()
}

/// Pull `key = value` out of a flat config file, ignoring comments.
fn field(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Not a let-chain: this crate is edition 2021, matching stormuefi.
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                let v = v.trim().trim_matches('"');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Resolve where to attach: file, then DNS, then the compiled floor.
///
/// Never fails. A stick with no config, no resolver and no records still boots
/// against the compiled defaults, which is what makes a blank `dd`-written
/// stick useful before anyone has edited anything onto it — and, more
/// importantly, what keeps a DNS outage from being a boot outage.
///
/// `note` takes the running commentary, so discovery is visible on the console
/// without any of it being fatal.
pub fn resolve(d: &Defaults, note: &mut dyn FnMut(&str)) -> Config {
    let mut cfg = Config {
        portal: d.portal,
        port: d.port,
        nqn: d.nqn.to_string(),
        nsid: d.nsid,
        stamp: None,
        source: "compiled defaults".to_string(),
    };

    let text = read_file(CONF_PATH);
    let file = text.as_deref();

    // A config that exists but is unreadable in part should still contribute
    // what it does carry: a typo in `nsid` must not silently move the portal
    // back to a value the operator thought they had replaced.
    let pinned = file.and_then(|t| field(t, "portal")).and_then(|s| parse_ipv4(&s));
    let file_port = file.and_then(|t| field(t, "port")).and_then(|s| s.parse().ok());
    let file_nqn = file.and_then(|t| field(t, "nqn"));
    let file_nsid = file.and_then(|t| field(t, "nsid")).and_then(|s| s.parse().ok());
    let zone = file
        .and_then(|t| field(t, "zone"))
        .unwrap_or_else(|| d.zone.to_string());
    // `discover = no` pins a machine to the compiled defaults without having to
    // name a portal that would then also need maintaining.
    let discover_off = file
        .and_then(|t| field(t, "discover"))
        .is_some_and(|v| matches!(v.as_str(), "no" | "false" | "0" | "off"));

    cfg.stamp = file.and_then(|t| field(t, "stamp"));

    if let Some(p) = pinned {
        // Pinned. Nothing is asked and nothing can move this machine.
        cfg.portal = p;
        cfg.source = format!("{CONF_PATH} (pinned)");
    } else if discover_off {
        note("discovery   : disabled by the config file");
    } else {
        note(&format!("discovery   : {}.{zone}", crate::dns::SERVICE));
        match crate::dns::discover(&zone, note) {
            Some(found) => {
                let [a, b, c, dd] = found.resolver;
                cfg.portal = found.portal;
                cfg.port = found.port;
                if let Some(n) = found.nqn {
                    cfg.nqn = n;
                }
                if let Some(n) = found.nsid {
                    cfg.nsid = n;
                }
                cfg.source = format!("DNS via {a}.{b}.{c}.{dd}");
            }
            None => note("  no answer; falling back"),
        }
    }

    // The file outranks discovery field by field, so a zone that publishes a
    // portal can still have its NQN or namespace overridden on one stick
    // without pinning that stick's address as well.
    let overridden = file_port.is_some() || file_nqn.is_some() || file_nsid.is_some();
    if let Some(p) = file_port {
        cfg.port = p;
    }
    if let Some(n) = file_nqn {
        cfg.nqn = n;
    }
    if let Some(n) = file_nsid {
        cfg.nsid = n;
    }
    if overridden && pinned.is_none() {
        cfg.source = format!("{} + {CONF_PATH}", cfg.source);
    }
    cfg
}

/// Render a config file, preserving the attach settings and recording a new
/// stamp. Used by the self-update path after it replaces `BOOTX64.EFI`.
// Unused until #2 lands. Kept rather than deleted because the write-back half
// of the config file is the part that has to be right first time: a stick that
// half-writes its own config is a machine that does not POST into anything.
#[allow(dead_code)]
pub fn render(cfg: &Config, stamp: &str) -> String {
    let [a, b, c, d] = cfg.portal;
    format!(
        "# stormbootx — edit this rather than rebuilding the binary.\n\
         # Written back by the self-update path; the stamp is the digest of\n\
         # the BOOTX64.EFI currently on this media.\n\
         portal = {a}.{b}.{c}.{d}\n\
         port   = {}\n\
         nqn    = {}\n\
         nsid   = {}\n\
         stamp  = {stamp}\n",
        cfg.port, cfg.nqn, cfg.nsid
    )
}

/// Write a file to the boot volume, replacing what is there.
// Also #2. See `render`.
#[allow(dead_code)]
pub fn write_file(path: &str, body: &[u8]) -> Result<(), String> {
    let handle = boot_volume().ok_or("no boot volume (LoadedImage has no device)")?;
    let mut fs = open_fs(handle).ok_or("boot volume has no SimpleFileSystem")?;
    let mut root = fs.open_volume().map_err(|e| format!("open_volume: {e:?}"))?;

    // Create the directory if this is the first write to a stick that was
    // only ever dd'd.
    let mut dbuf = [0u16; 64];
    if let Ok(dir16) = CStr16::from_str_with_buf(r"\stormboot", &mut dbuf) {
        let _ = root.open(dir16, FileMode::CreateReadWrite, FileAttribute::DIRECTORY);
    }

    let mut buf = [0u16; 256];
    let path16 = CStr16::from_str_with_buf(path, &mut buf).map_err(|_| "path too long")?;
    let file = root
        .open(path16, FileMode::CreateReadWrite, FileAttribute::empty())
        .map_err(|e| format!("open for write: {e:?}"))?;
    let mut file: RegularFile = match file.into_type().map_err(|e| format!("{e:?}"))? {
        FileType::Regular(f) => f,
        FileType::Dir(_) => return Err("path is a directory".into()),
    };

    file.set_position(0).ok();
    file.write(body).map_err(|e| format!("write: {e:?}"))?;
    truncate(&mut file, body.len() as u64)?;
    file.flush().map_err(|e| format!("flush: {e:?}"))?;
    Ok(())
}

/// Shorten a file to `len`.
///
/// An update that shrinks a file must not leave the tail of the previous one
/// behind: the leftover still parses, and a stale `stamp` or `portal` line
/// surviving past the value that replaced it is a machine that attaches
/// somewhere nobody chose.
///
/// `FileMode::CREATE_READ_WRITE` on a file that already exists opens it — it
/// does not truncate — and seeking to the new end does not shorten it either.
/// The only thing that does is `SetInfo` with a smaller `FileSize`, and since
/// `FileInfo` has no setters that means rebuilding it around the existing
/// times, attribute and **name**: a `SetInfo` whose `FileName` differs is a
/// rename, so the name has to be carried across unchanged.
fn truncate(file: &mut RegularFile, len: u64) -> Result<(), String> {
    let info = file
        .get_boxed_info::<FileInfo>()
        .map_err(|e| format!("get file info: {e:?}"))?;
    if info.file_size() <= len {
        return Ok(());
    }

    // `FileInfo` requires 8-byte alignment and `new` writes into borrowed
    // storage; an array of u64 is aligned by its type, where a Vec<u8> would
    // not be. 384 bytes is the fixed header plus room for any name that fits
    // on this ESP.
    let mut storage = [0u64; 48];
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            storage.as_mut_ptr().cast::<u8>(),
            core::mem::size_of_val(&storage),
        )
    };
    let shorter = FileInfo::new(
        bytes,
        len,
        0, // physical size is derived from file_size and cannot be set
        *info.create_time(),
        *info.last_access_time(),
        *info.modification_time(),
        info.attribute(),
        info.file_name(),
    )
    .map_err(|e| format!("build file info: {e:?}"))?;

    file.set_info(shorter)
        .map_err(|e| format!("truncate to {len}: {e:?}"))
}

/// Unused today; kept because the update path needs a device path to name the
/// volume in a message an operator can act on.
#[allow(dead_code)]
pub fn boot_volume_path() -> Option<ScopedProtocol<DevicePath>> {
    boot::open_protocol_exclusive::<DevicePath>(boot_volume()?).ok()
}
