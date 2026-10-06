//! The name and host NQN handed down to Linux (#76, stormblock#249).
//!
//! The initramfs claims `boothost/<name>` again once the kernel is up. Left
//! to itself it guesses the name from SMBIOS, and on the X9 MicroCloud
//! blades that serial is the chassis's, shared by all eight: server8 booted
//! its own image from stormbootx and then laid its disk from server1's. So
//! stormbootx says what it claimed on, in **volatile** EFI variables the
//! initramfs reads ahead of `rd.stormblock.tag=` and SMBIOS (stormblock
//! `docs/boot-hooks.md`, "Whose image"):
//!
//! | variable | value (ASCII, no NUL) |
//! |---|---|
//! | `StormBootTag` | the name claimed on: the reply's host, else the name claimed |
//! | `StormBootHostNqn` | the host NQN the namespace was attached as |
//! | `StormBootClock` | `synced:<NTP server address>` or `unsynced` (#77, `clock.rs`) |
//! | `StormBootUpdate` | the medium's self-update (#83, `selfupdate.rs`): `serial:<n>`, `trial:<n>:<start>`, `good:<n>` or `failed:<n>` |
//! | `StormBootInstallConfig` (+ `…0`..`…<N-1>`) | the media's `install-config.yaml` (#79): `v1:<length>:<N>:<sha256>`, the bytes in `N` chunks (`installconf.rs`) |
//!
//! Vendor GUID `ab361f54-0166-44a4-a088-1ac22e98ab76`, attributes
//! `BOOTSERVICE_ACCESS | RUNTIME_ACCESS` and never `NON_VOLATILE`: they die
//! with the boot that set them, so a stale one can never name the next boot,
//! and nothing is written to NVRAM. A failure is reported and is not fatal:
//! Linux falls back to SMBIOS and will not install over a disk on that guess.

use uefi::runtime::{VariableAttributes, VariableVendor};
use uefi::{cstr16, guid, CStr16};

use alloc::format;

use crate::installconf;
use crate::universal::handoff_value_ok;

/// stormblock#249's vendor GUID for both variables.
pub const VENDOR: VariableVendor = VariableVendor(guid!("ab361f54-0166-44a4-a088-1ac22e98ab76"));

/// `StormBootTag`, when there is a name to give. `None` sets nothing.
pub fn set_tag(name: Option<&str>) {
    match name {
        Some(n) => set(cstr16!("StormBootTag"), "StormBootTag", n),
        None => uefi::println!("handoff     : no name to hand to Linux; StormBootTag not set"),
    }
}

/// `StormBootHostNqn`.
pub fn set_host_nqn(nqn: &str) {
    set(cstr16!("StormBootHostNqn"), "StormBootHostNqn", nqn);
}

/// `StormBootClock` (#77): whether the RTC was checked against NTP and is
/// right (`synced:<server>`), or not (`unsynced`), so Linux (stormcos#213)
/// knows whether to trust the clock it starts with.
pub fn set_clock(value: &str) {
    set(cstr16!("StormBootClock"), "StormBootClock", value);
}

/// `StormBootUpdate` (#83): what the boot medium's self-update did, for
/// stormcentral to see from Linux. `keep` leaves a value already set this
/// boot alone: a set put back after a failed trial says `failed:<n>` before
/// it chain-loads the previous binary, which must not overwrite it.
pub fn set_update(value: &str, keep: bool) {
    if keep {
        let mut buf = [0u8; 64];
        if uefi::runtime::get_variable(cstr16!("StormBootUpdate"), &VENDOR, &mut buf).is_ok() {
            return;
        }
    }
    set(cstr16!("StormBootUpdate"), "StormBootUpdate", value);
}

/// `StormBootUpdate` as this boot set it, for the claim (#90). `None` when
/// nothing set it (no update state on the medium).
pub fn update_value() -> Option<alloc::string::String> {
    let mut buf = [0u8; 64];
    let (data, _) = uefi::runtime::get_variable(cstr16!("StormBootUpdate"), &VENDOR, &mut buf).ok()?;
    core::str::from_utf8(data).ok().map(alloc::string::String::from)
}

/// `install-config.yaml` from the media (#79, stormcos#82), in chunks under
/// a header set last (`installconf.rs`). Read with the rest of the media,
/// before the network. The console names its size and digest, never its
/// content: it carries the pull secret and the API token.
pub fn set_install_config() {
    let Some(body) = crate::config::read_bytes(installconf::PATH, installconf::MAX) else {
        uefi::println!("install cfg : none on the media ({})", installconf::PATH);
        return;
    };
    match installconf::accept(body.len()) {
        Ok(()) => {}
        Err(installconf::Refused::Empty) => {
            uefi::println!("install cfg : {} is empty; not handed down", installconf::PATH);
            return;
        }
        Err(installconf::Refused::TooLarge(_)) => {
            uefi::println!(
                "install cfg : {} is over {} KiB; not handed down",
                installconf::PATH,
                installconf::MAX / 1024
            );
            return;
        }
    }
    let attrs = VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
    let digest = crate::sha256::digest(&body).to_hex();
    let digest = core::str::from_utf8(&digest).unwrap_or("");
    let n = installconf::chunks(body.len());
    for (i, chunk) in body.chunks(installconf::CHUNK).enumerate() {
        let mut name = [0u8; 40];
        let mut wide = [0u16; 40];
        let Ok(var) = CStr16::from_str_with_buf(installconf::chunk_name(i, &mut name), &mut wide) else {
            return;
        };
        if let Err(e) = uefi::runtime::set_variable(var, &VENDOR, attrs, chunk) {
            uefi::println!(
                "install cfg : not handed down: chunk {i} of {n} refused ({:?}); the firmware's variable store is too small for {} bytes",
                e.status(),
                body.len()
            );
            for j in 0..i {
                let mut name = [0u8; 40];
                let mut wide = [0u16; 40];
                if let Ok(var) = CStr16::from_str_with_buf(installconf::chunk_name(j, &mut name), &mut wide) {
                    let _ = uefi::runtime::delete_variable(var, &VENDOR);
                }
            }
            return;
        }
    }
    let mut hb = [0u8; 128];
    let Some(header) = installconf::header(body.len(), digest, &mut hb) else {
        return;
    };
    let label = format!("{} ({} bytes in {n} chunk(s))", installconf::NAME, body.len());
    set(cstr16!("StormBootInstallConfig"), &label, header);
}

fn set(var: &CStr16, label: &str, value: &str) {
    if !handoff_value_ok(value) {
        uefi::println!("handoff     : {label} not set: {value:?} is not [A-Za-z0-9._:-], Linux would ignore it");
        return;
    }
    let attrs = VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
    match uefi::runtime::set_variable(var, &VENDOR, attrs, value.as_bytes()) {
        Ok(()) => uefi::println!("handoff     : {label} = {value}"),
        Err(e) => uefi::println!("handoff     : {label} not set ({:?})", e.status()),
    }
}
