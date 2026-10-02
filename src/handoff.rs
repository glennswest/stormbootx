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
//!
//! Vendor GUID `ab361f54-0166-44a4-a088-1ac22e98ab76`, attributes
//! `BOOTSERVICE_ACCESS | RUNTIME_ACCESS` and never `NON_VOLATILE`: they die
//! with the boot that set them, so a stale one can never name the next boot,
//! and nothing is written to NVRAM. A failure is reported and is not fatal:
//! Linux falls back to SMBIOS and will not install over a disk on that guess.

use uefi::runtime::{VariableAttributes, VariableVendor};
use uefi::{cstr16, guid, CStr16};

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
