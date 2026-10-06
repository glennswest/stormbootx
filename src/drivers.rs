//! NIC drivers carried on the boot media (#26).
//!
//! A platform can carry the whole upper network stack — SnpDxe, MnpDxe,
//! Ip4Dxe, TcpDxe — and still have no UEFI driver for the NICs it was built
//! with. The Supermicro X9 blades (server1–8) are that platform: their Intel
//! 10G (`IBA XE`) and ConnectX-3 (`FlexBoot`) carry legacy option ROMs only,
//! the one UEFI NIC driver in the firmware (`PRO/1000`) manages no device, and
//! with no `EFI_SIMPLE_NETWORK` handle there is nothing for MNP/IP4/TCP4 to
//! bind to. No setup switch changes that.
//!
//! So the media brings the missing bottom layer: every `*.efi` in
//! `\stormboot\drivers\` on the volume this binary booted from is loaded and
//! started as a driver, and then every handle is connected so the new driver
//! binds its NIC and stormbootx's own TCP/IP (#56) runs on the SNP it
//! produces. What ships there, on the rustnic medium only, is the Rust
//! stormnic drivers (`scripts/build-nic-drivers.sh`); no medium carries iPXE
//! (#91), and the fw medium carries no drivers (#52).
//!
//! **The platform goes first.** Before anything is loaded, one recursive
//! `ConnectController` pass lets the firmware's own drivers claim every NIC
//! they will take. A media driver then only finds the NICs nothing else wanted
//! — it never displaces a native driver on a machine that has one, so the same
//! media is safe to boot anywhere.
//!
//! Nothing here is fatal. A missing directory is the normal case; a driver
//! that will not load or start is reported and skipped, and the boot goes on
//! to find TCP4 or fall through exactly as it would have.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use uefi::boot::{self, LoadImageSource};
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::BootPolicy;
use uefi::CStr16;

/// Where drivers live on the boot media, beside `stormboot.conf`.
pub const DRIVERS_DIR: &str = r"\stormboot\drivers";

/// One driver file and what became of it.
pub struct Loaded {
    pub name: String,
    pub result: Result<(), String>,
}

/// Is this a file to load? `.efi` in any case, since FAT keeps what was
/// written and a tool may have upper-cased it.
fn is_driver(name: &str) -> bool {
    name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".efi")
}

/// Load and start every driver on the media, then connect every handle.
///
/// Returns one entry per file tried, in name order; empty when the media
/// carries no drivers, in which case nothing at all was done.
pub fn load_from_media() -> Vec<Loaded> {
    let names: Vec<String> = crate::config::list_dir(DRIVERS_DIR)
        .into_iter()
        .filter(|n| is_driver(n))
        .collect();
    if names.is_empty() {
        return Vec::new();
    }

    // Each step is announced *before* it runs, with the seconds so far. A
    // driver that hangs in its start or in binding its NIC never returns, and
    // on server1 a hang with the summary printed only at the end left the
    // console silent with nothing to say which driver it was (#26).
    let t0 = now_secs();
    uefi::println!("drivers     : {} on the media in {DRIVERS_DIR}", names.len());

    // The platform's own drivers first (see the module comment).
    uefi::println!("    binding the platform's own drivers first");
    let _ = crate::net::connect_all();

    let volume_dp = crate::config::boot_volume()
        .and_then(|h| crate::blockio::device_path_of(h.as_ptr()));
    let out: Vec<Loaded> = names
        .into_iter()
        .map(|name| {
            uefi::println!("    [{:>3} s] starting {name}", elapsed(t0));
            let result = match volume_dp {
                Some(dp) => load_one(dp, &name),
                None => Err("the boot volume has no device path".into()),
            };
            match &result {
                Ok(()) => uefi::println!("    [{:>3} s] {name} started", elapsed(t0)),
                Err(e) => uefi::println!("    [{:>3} s] {name} not started: {e}", elapsed(t0)),
            }
            Loaded { name, result }
        })
        .collect();

    // Bind what was just registered: each driver to its NIC, and the
    // platform's MNP/IP4/TCP4 to the SNP handles those produce. Recursive, so
    // one pass reaches the children.
    if out.iter().any(|l| l.result.is_ok()) {
        uefi::println!("    [{:>3} s] connecting every handle (drivers bind their NICs)", elapsed(t0));
        let _ = crate::net::connect_all();
        uefi::println!("    [{:>3} s] connected", elapsed(t0));
    }
    out
}

/// Seconds since midnight from the RTC: coarse, but a hang is measured in
/// minutes, and it needs no timer protocol. `None` if the RTC will not say.
fn now_secs() -> Option<u32> {
    uefi::runtime::get_time()
        .ok()
        .map(|t| t.hour() as u32 * 3600 + t.minute() as u32 * 60 + t.second() as u32)
}

fn elapsed(t0: Option<u32>) -> u32 {
    match (t0, now_secs()) {
        // Across midnight the clock wraps; a day is added back.
        (Some(a), Some(b)) => (b + 86_400 - a) % 86_400,
        _ => 0,
    }
}

/// `LoadImage` by device path, then `StartImage`.
///
/// By device path rather than from a buffer so the driver's `LoadedImage`
/// carries a real `DeviceHandle` and `FilePath`: a driver may read its own
/// device path at start-up and refuse to run without one (iPXE's did).
fn load_one(volume: &uefi::proto::device_path::DevicePath, name: &str) -> Result<(), String> {
    let path = format!(r"{DRIVERS_DIR}\{name}");
    let mut buf = [0u16; 256];
    let path16 = CStr16::from_str_with_buf(&path, &mut buf).map_err(|_| "path too long")?;
    let mut fbuf = Vec::new();
    let file_dp = DevicePathBuilder::with_vec(&mut fbuf)
        .push(&build::media::FilePath { path_name: path16 })
        .and_then(|b| b.finalize())
        .map_err(|e| format!("file path build failed: {e:?}"))?;
    let full = volume
        .append_path(file_dp)
        .map_err(|e| format!("append_path failed: {e:?}"))?;

    let image = boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromDevicePath {
            device_path: &full,
            boot_policy: BootPolicy::ExactMatch,
        },
    )
    .map_err(|e| format!("LoadImage: {:?}", e.status()))?;

    // A driver's entry point registers its binding and returns; the image
    // stays resident. One that fails is unloaded so it holds nothing.
    boot::start_image(image).map_err(|e| {
        let _ = boot::unload_image(image);
        format!("StartImage: {:?}", e.status())
    })
}
