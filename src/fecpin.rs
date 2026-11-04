//! The FEC pin at boot (#186; the owner, 2026-11-04: "option 2").
//!
//! `fec_pin = rs` on the media: a ConnectX port whose **NV next-boot value** is
//! not RS gets RS written, and the machine warm-resets once to apply it. This
//! reverses the boot path's read-only rule of 2026-09-07 by the owner's choice,
//! and keeps what that day's failure taught:
//!
//! - **No link state reaches this.** The 0.3.4 self-heal believed one early
//!   `media_present` sample. The only input here is the value read from the card
//!   (`connectx::pin_action`).
//! - **Once.** `StormBootFecPin` (NV, #76's GUID) is set before the write and
//!   stays until a read shows RS. A card that loses the value, or a write that
//!   hangs, is said on the next boot and left alone: never a reset loop. A
//!   variable the firmware refuses means nothing is written.
//! - **Said.** What was written and why is on the console before the reset, and
//!   again on the next boot, whose lines reach the network (#162): the reset
//!   comes before any lease.
//!
//! The ways back: `fec_pin = off` on the media or in `local.conf`, then a `fec =`
//! stick or `fec default` at the failure console. A `fec =` stick is an operator
//! saying something else, so the pin stands aside on it.
//!
//! A warm reset here uses up one of a self-update trial's starts (they are
//! counted first): the next start is the one that reaches the attach.
//!
//! `STORMBOOTX_FEC_TEST` compiles in a pretend card (`tests/fecpin-ovmf.sh`),
//! because QEMU has no ConnectX. No golden sets it: `build-golden.sh` refuses.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use uefi::cstr16;
use uefi::runtime::VariableAttributes;

use crate::connectx::{self, Pin};
use crate::mlxfec::{self, Fec, Summary};
use crate::{config, handoff, ui};

const TEST_CARD: bool = option_env!("STORMBOOTX_FEC_TEST").is_some();
const RECORD_MAX: usize = 512;
const WRITING: &str = "was writing, and never finished";

/// The FEC step's read.
pub fn read() -> Summary {
    if TEST_CARD { test_card::read() } else { mlxfec::apply(None) }
}

fn write() -> Summary {
    if TEST_CARD { test_card::write() } else { mlxfec::apply(Some(Fec::Rs)) }
}

fn record() -> Option<String> {
    let mut buf = [0u8; RECORD_MAX];
    let (data, _) = uefi::runtime::get_variable(cstr16!("StormBootFecPin"), &handoff::VENDOR, &mut buf).ok()?;
    Some(String::from_utf8_lossy(data).into_owned())
}

fn set_record(value: &str) -> Result<(), String> {
    let attrs = VariableAttributes::NON_VOLATILE
        | VariableAttributes::BOOTSERVICE_ACCESS
        | VariableAttributes::RUNTIME_ACCESS;
    // On a char boundary: the text is ASCII but for a value's name, never split.
    let mut end = value.len().min(RECORD_MAX);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    uefi::runtime::set_variable(cstr16!("StormBootFecPin"), &handoff::VENDOR, attrs, &value.as_bytes()[..end])
        .map_err(|e| format!("{:?}", e.status()))
}

fn clear_record() {
    let _ = uefi::runtime::delete_variable(cstr16!("StormBootFecPin"), &handoff::VENDOR);
}

/// Pin what this boot's `read` found, when the media says to. Does not return
/// when it wrote something.
pub fn at_boot(read: &Summary) {
    let last = record();
    let stated = config::stated_fec_pin();
    let file = config::file_of(&["fec_pin"]);
    match stated.as_deref() {
        Some("rs") => {}
        Some("off") | None => {
            if let (Some(last), true) = (&last, stated.is_some()) {
                clear_record();
                uefi::println!("fec pin     : off ({file}); forgot the last pin's record: {last}");
            }
            return;
        }
        Some(other) => {
            uefi::println!("fec pin     : `{other}` in {file} is not rs or off; nothing pinned");
            return;
        }
    }

    let differ: Vec<_> = read.ports.iter().filter(|p| p.port.supported && p.port.next != Fec::Rs).collect();
    uefi::println!(
        "fec pin     : rs ({file}): {} port(s) read, {} with another next-boot value",
        read.ports.len(),
        differ.len()
    );
    for p in read.ports.iter().filter(|p| !p.port.supported) {
        uefi::println!("  {} port {}: the card has no FEC override here; left as is", p.loc, p.port.port);
    }
    // Set earlier, not applied: a warm reset that did not reset the card. A
    // line a boot, never another reset.
    for p in read.ports.iter().filter(|p| p.port.supported && p.port.next == Fec::Rs && p.port.current != Fec::Rs) {
        uefi::println!(
            "  {} port {}: still runs {}; rs takes effect when the card resets (a power cycle, if this repeats)",
            p.loc,
            p.port.port,
            p.port.current.name()
        );
    }

    match connectx::pin_action(read.ports.len(), differ.len(), read.failed, last.is_some()) {
        Pin::Nothing => {}
        Pin::Held => {
            clear_record();
            uefi::println!("  the last boot's pin held: {}", last.as_deref().unwrap_or(""));
        }
        Pin::Stuck => {
            uefi::println!("  the last boot's pin did not hold: {}", last.as_deref().unwrap_or(""));
            for p in &differ {
                uefi::println!("  {} port {}: next boot {}", p.loc, p.port.port, p.port.next.name());
            }
            uefi::println!("  not writing again and not resetting: a card that loses the value must not loop the machine.");
            uefi::println!("  `fec_pin = off` on the media forgets this, once the card is looked at.");
        }
        Pin::Write => {
            for p in &differ {
                uefi::println!(
                    "  {} port {}: next boot {}: writing rs",
                    p.loc,
                    p.port.port,
                    p.port.next.name()
                );
            }
            uefi::println!("  why: fec_pin = rs in {file}, and only the card's own NV value was compared; no link state was read");
            // First, so a write that hangs cannot be tried on every boot.
            if let Err(e) = set_record(WRITING) {
                uefi::println!("  nothing written: the firmware refused StormBootFecPin ({e}), and with no record a reset could loop");
                return;
            }
            let sum = write();
            if sum.written == 0 {
                clear_record();
                uefi::println!("  nothing written ({} port(s) failed); the next boot tries again, and this one goes on", sum.failed);
                return;
            }
            let what = sum.wrote.join("; ");
            if let Err(e) = set_record(&what) {
                uefi::println!("  (could not record what was written: {e}; the next boot says only that a write was started)");
            }
            uefi::println!("  wrote {what}");
            uefi::println!("  warm-resetting once so the card loads it");
            ui::detail(ui::Step::Nic, "FEC pinned to rs; resetting once");
            uefi::runtime::reset(uefi::runtime::ResetType::WARM, uefi::Status::SUCCESS, None);
        }
    }
}

/// A pretend ConnectX for `tests/fecpin-ovmf.sh`, its NV value in a variable so
/// it outlives the warm reset. `fec_test_card =` says how it behaves: `holds`,
/// `forgets` (a write reads back, and the next boot's read does not have it),
/// `refuses` (the write fails), `rs` (already RS).
mod test_card {
    use super::*;
    use crate::mlxfec::{Port, PortAt};

    fn stored() -> Fec {
        let mut buf = [0u8; 16];
        match uefi::runtime::get_variable(cstr16!("StormBootFecTestCard"), &handoff::VENDOR, &mut buf) {
            Ok((data, _)) if data == b"rs" => Fec::Rs,
            _ => Fec::DeviceDefault,
        }
    }

    fn mode() -> String {
        config::conf_text()
            .and_then(|t| config::field(&t, "fec_test_card"))
            .unwrap_or_else(|| String::from("holds"))
    }

    pub fn read() -> Summary {
        uefi::println!("  a TEST card is compiled in (STORMBOOTX_FEC_TEST)");
        let value = if mode() == "rs" { Fec::Rs } else { stored() };
        let port = Port { port: 1, supported: true, current: value, next: value };
        uefi::println!("  84:00.0  test card port 1: current {}, next boot {}", value.name(), value.name());
        Summary { cards: 1, ports: alloc::vec![PortAt { loc: String::from("84:00.0"), port }], ..Summary::default() }
    }

    pub fn write() -> Summary {
        match mode().as_str() {
            "refuses" => Summary { cards: 1, failed: 1, ..Summary::default() },
            "forgets" => wrote(),
            _ => {
                let attrs = VariableAttributes::NON_VOLATILE
                    | VariableAttributes::BOOTSERVICE_ACCESS
                    | VariableAttributes::RUNTIME_ACCESS;
                let _ = uefi::runtime::set_variable(cstr16!("StormBootFecTestCard"), &handoff::VENDOR, attrs, b"rs");
                wrote()
            }
        }
    }

    fn wrote() -> Summary {
        Summary {
            cards: 1,
            written: 1,
            wrote: alloc::vec![String::from("84:00.0 port 1: device-default -> rs")],
            ..Summary::default()
        }
    }
}
