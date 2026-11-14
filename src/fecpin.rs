//! The FEC pin at boot (#186; the owner, 2026-11-04: "option 2").
//!
//! `fec_pin = rs` on the media: a ConnectX port whose **NV next-boot value** is
//! not RS gets RS written, and the machine resets to apply it. Since #203 the
//! pin is judged by what the card **runs**, not only what it will load: the
//! owner, 2026-11-13, "make sure there using the right fec! before you try to
//! boot". This reverses the boot path's read-only rule of 2026-09-07 by the
//! owner's choice, and keeps what that day's failure taught:
//!
//! - **No link state reaches this.** The 0.3.4 self-heal believed one early
//!   `media_present` sample. The only input here is the value read from the card
//!   (`connectx::pin_action`).
//! - **At most two resets, ever.** `StormBootFecPin` (NV, #76's GUID) is set
//!   before the write and before each reset, and names the stage: a warm reset
//!   first; if the card still does not run the value, one cold reset; after
//!   that, no more resets and no network boot on the wrong FEC: the boot holds
//!   (`stuck`) with its API up, and a machine with no lease falls through to
//!   the local disk as always. The record clears once a read shows the card
//!   running it. A variable the firmware refuses means nothing is written.
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

use core::sync::atomic::{AtomicBool, Ordering};

use crate::connectx::{self, Choice, Pin, Reset, Source, Stage, Why};
use crate::mlxfec::{self, Fec, PortAt, Summary};
use crate::{config, handoff, ui};

const TEST_CARD: bool = option_env!("STORMBOOTX_FEC_TEST").is_some();
const RECORD_MAX: usize = 512;
const WRITING: &str = "was writing, and never finished";

/// The FEC step's read.
pub fn read() -> Summary {
    if TEST_CARD { test_card::read() } else { mlxfec::apply(None) }
}

fn write(want: &dyn Fn(&PortAt) -> Option<Fec>) -> Summary {
    if TEST_CARD { test_card::write(want) } else { mlxfec::apply_each(true, want) }
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

/// Whether this boot's pin is stuck: a warm and a cold reset did not get the
/// card running the wanted FEC (#203). `run()` holds instead of booting on it,
/// on every pass, since a hold's boot again does not run the pin again.
static STUCK: AtomicBool = AtomicBool::new(false);

pub fn stuck() -> bool {
    STUCK.load(Ordering::Relaxed)
}

/// How far the record says the pin went: `warm: …` / `cold: …`; any other
/// record (an older stormbootx's, or one that never finished) counts as one
/// warm reset tried, so an upgrade cannot reset more than twice either.
fn stage_of(last: Option<&str>) -> Stage {
    match last {
        None => Stage::None,
        Some(t) if t.starts_with("cold: ") => Stage::Cold,
        Some(_) => Stage::Warm,
    }
}

/// A wanted value as a FEC: `rs` (and `rs-544`, the card's one RS value at
/// PAM4 speeds), `fc`, `none`, `default`. `off` and `auto` are `choose`'s.
pub(crate) fn wanted(stated: &str) -> Option<Fec> {
    match stated {
        "rs" | "rs-544" | "rs-528" | "cl91" | "cl108" => Some(Fec::Rs),
        "fc" | "cl74" => Some(Fec::Fc),
        "none" => Some(Fec::Off),
        "default" | "device-default" => Some(Fec::DeviceDefault),
        _ => None,
    }
}

/// A port's own key (#203), lower-cased: `fec_pin.<seg:bus:dev.fn>`, then the
/// same without the segment, then `fec_pin.<mac>`.
fn port_key(text: &str, p: &PortAt) -> Option<String> {
    let short = p.bdf.split_once(':').map(|(_, rest)| rest).unwrap_or(&p.bdf);
    let mut keys = alloc::vec![format!("fec_pin.{}", p.bdf), format!("fec_pin.{short}")];
    if let Some(mac) = &p.mac {
        keys.push(format!("fec_pin.{}", mac.to_ascii_lowercase()));
    }
    keys.iter().find_map(|k| config::field(text, k)).map(|v| v.to_ascii_lowercase())
}

/// Whether the media names any port's own key at all.
fn any_port_key(text: &str) -> bool {
    text.lines().any(|l| l.trim_start().starts_with("fec_pin."))
}

/// One port's wanted FEC (#203).
pub(crate) enum PortWant {
    /// The value, and where it came from, for the console and the API.
    Want(Fec, String),
    /// Left as it is, and why.
    Leave(String),
    /// A value that is not a FEC.
    Bad(String),
}

pub(crate) fn want_of(text: &str, pin: Option<&str>, p: &PortAt) -> PortWant {
    let port = port_key(text, p);
    let table = p.speed.and_then(|s| config::field(text, &format!("fec_table.{}", s.name()))).map(|v| v.to_ascii_lowercase());
    match connectx::choose(pin, port.as_deref(), table.as_deref(), p.speed) {
        Choice::Leave(why) => PortWant::Leave(match why {
            Why::NotPinned => String::from("no fec_pin names it"),
            Why::Off => String::from("fec_pin = off"),
            Why::PortOff => String::from("its own key says off"),
            Why::TableOff(s) => format!("fec_pin = auto, fec_table.{} = off", s.name()),
            Why::NoSpeed => String::from("fec_pin = auto, and the card did not say its speed"),
        }),
        Choice::Want(v, src) => {
            let Some(f) = wanted(v) else {
                return PortWant::Bad(format!("`{v}` for {} is not rs, fc, none, default, auto or off", p.bdf));
            };
            PortWant::Want(
                f,
                match src {
                    Source::Port => format!("its own key, {v}"),
                    Source::Pin => format!("fec_pin = {v}"),
                    Source::Table(s) => format!("fec_pin = auto, fec_table.{} = {v}", s.name()),
                    Source::Ieee(s) => format!("fec_pin = auto, {} by the standard: {v}", s.name()),
                },
            )
        }
    }
}

/// Pin what this boot's `read` found, when the media says to. Does not return
/// when it resets; sets `stuck` when two resets did not get the FEC right.
pub fn at_boot(read: &Summary) {
    let last = record();
    // Keys match in any case: a MAC is written either way.
    let text = config::conf_text().unwrap_or_default().to_ascii_lowercase();
    let stated = config::stated_fec_pin();
    let per_port = any_port_key(&text);
    let file = config::file_of(&["fec_pin"]);
    if !per_port {
        match stated.as_deref() {
            Some("off") | None => {
                if let (Some(last), true) = (&last, stated.is_some()) {
                    clear_record();
                    uefi::println!("fec pin     : off ({file}); forgot the last pin's record: {last}");
                }
                return;
            }
            Some(v) if v != "auto" && wanted(v).is_none() => {
                uefi::println!("fec pin     : `{v}` in {file} is not rs, fc, none, default, auto or off; nothing pinned");
                return;
            }
            Some(_) => {}
        }
    }

    // Each port's wanted value, from its own key, `fec_pin` and the table.
    let verbose = per_port || stated.as_deref() == Some("auto");
    let mut wants: Vec<(&PortAt, Fec, String)> = Vec::new();
    let mut left: Vec<(&PortAt, String)> = Vec::new();
    for p in read.ports.iter().filter(|p| p.port.supported) {
        match want_of(&text, stated.as_deref(), p) {
            PortWant::Want(f, from) => wants.push((p, f, from)),
            PortWant::Leave(why) => left.push((p, why)),
            PortWant::Bad(e) => uefi::println!("fec pin     : {e}; that port is left as is"),
        }
    }
    let label = match (per_port, stated.as_deref()) {
        (false, Some(v)) => String::from(v),
        _ => String::from("per port"),
    };
    let tried = stage_of(last.as_deref());
    let next_differ: Vec<_> = wants.iter().filter(|(p, f, _)| p.port.next != *f).collect();
    let run_differ: Vec<_> = wants.iter().filter(|(p, f, _)| p.port.current != *f).collect();
    uefi::println!(
        "fec pin     : {label} ({file}): {} port(s) read, {} with another next-boot value, {} running another",
        read.ports.len(),
        next_differ.len(),
        run_differ.len()
    );
    for p in read.ports.iter().filter(|p| !p.port.supported) {
        uefi::println!("  {} port {}: the card has no FEC override here; left as is", p.loc, p.port.port);
    }
    if verbose {
        for (p, f, from) in &wants {
            uefi::println!("  {} ({}) wants {} ({from})", p.bdf, p.speed.map_or("speed ?", |s| s.name()), f.name());
        }
        for (p, why) in &left {
            uefi::println!("  {} left as is: {why}", p.bdf);
        }
    }
    // Every port that is pinned to one value names it; several say "the wanted FEC".
    let target = match wants.first() {
        Some((_, f, _)) if wants.iter().all(|(_, g, _)| g == f) => String::from(f.name()),
        _ => String::from("the wanted FEC"),
    };

    let reset = match connectx::pin_action(read.ports.len(), next_differ.len(), run_differ.len(), read.failed, tried) {
        Pin::Nothing => return,
        Pin::Held => {
            clear_record();
            uefi::println!("  the card runs {target}: the last pin held ({})", last.as_deref().unwrap_or(""));
            return;
        }
        Pin::Stuck => {
            STUCK.store(true, Ordering::Relaxed);
            uefi::println!("  a warm and a cold reset did not get it running {target}: {}", last.as_deref().unwrap_or(""));
            for (p, f, _) in &run_differ {
                uefi::println!(
                    "  {} port {}: runs {}, next boot {}, wanted {}",
                    p.loc,
                    p.port.port,
                    p.port.current.name(),
                    p.port.next.name(),
                    f.name()
                );
            }
            uefi::println!("  no more resets, and no network boot on the wrong FEC: holding (a power cycle may load it;");
            uefi::println!("  `fec_pin = off` on the media forgets this)");
            return;
        }
        Pin::Write(r) | Pin::Reset(r) => r,
    };
    let (word, kind) = match reset {
        Reset::Warm => ("warm", uefi::runtime::ResetType::WARM),
        Reset::Cold => ("cold", uefi::runtime::ResetType::COLD),
    };
    for (p, _, _) in &run_differ {
        uefi::println!("  {} port {}: runs {}, next boot {}", p.loc, p.port.port, p.port.current.name(), p.port.next.name());
    }
    uefi::println!("  why: fec_pin = {label} in {file}; only the card's own values were compared, no link state was read");
    // First, so a write or a reset that hangs cannot be tried on every boot.
    let before = format!("{word}: {}", if next_differ.is_empty() { "reset to load the next-boot value" } else { WRITING });
    if let Err(e) = set_record(&before) {
        uefi::println!("  nothing done: the firmware refused StormBootFecPin ({e}), and with no record a reset could loop");
        return;
    }
    if !next_differ.is_empty() {
        let pick = |at: &PortAt| wants.iter().find(|(p, _, _)| p.bdf == at.bdf).map(|(_, f, _)| *f);
        let sum = write(&pick);
        if sum.written == 0 {
            clear_record();
            uefi::println!("  nothing written ({} port(s) failed); the next boot tries again, and this one goes on", sum.failed);
            return;
        }
        let what = sum.wrote.join("; ");
        if let Err(e) = set_record(&format!("{word}: {what}")) {
            uefi::println!("  (could not record what was written: {e}; the next boot says only that a write was started)");
        }
        uefi::println!("  wrote {what}");
    }
    let n = if reset == Reset::Warm { "1 of at most 2" } else { "2 of at most 2" };
    uefi::println!("  {word}-resetting ({n}) so the card runs {target}");
    ui::detail(ui::Step::Nic, &format!("FEC pinned to {target}; {word} reset ({n})"));
    uefi::runtime::reset(kind, uefi::Status::SUCCESS, None);
}

/// A pretend ConnectX for `tests/fecpin-ovmf.sh`, its NV value in a variable so
/// it outlives the reset. `fec_test_card =` says how it behaves: `holds` (runs
/// what was written after any reset), `cold` (only after a cold one), `stuck`
/// (keeps the write, never runs it), `forgets` (a write reads back, and the
/// next boot's read does not have it), `refuses` (the write fails), `rs`
/// (already RS).
mod test_card {
    use super::*;
    use crate::mlxfec::Port;

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
        let mode = mode();
        let next = if mode == "rs" { Fec::Rs } else { stored() };
        let current = match mode.as_str() {
            "stuck" => Fec::DeviceDefault,
            // Runs the NV value only after a cold reset: until then, and only
            // while a warm one is the last the record names, the old value.
            "cold" if stage_of(record().as_deref()) == Stage::Warm => Fec::DeviceDefault,
            _ => next,
        };
        let port = Port { port: 1, supported: true, current, next, stored: true };
        uefi::println!("  84:00.0  test card port 1: current {}, next boot {}", current.name(), next.name());
        let at = PortAt {
            loc: String::from("84:00.0"),
            bdf: String::from("0000:84:00.0"),
            mac: Some(String::from("52:54:00:fe:c0:01")),
            speed: Some(connectx::Speed::G25),
            port,
        };
        Summary { cards: 1, ports: alloc::vec![at], ..Summary::default() }
    }

    pub fn write(want: &dyn Fn(&PortAt) -> Option<Fec>) -> Summary {
        let Some(want) = want(&read_quiet()) else { return Summary { cards: 1, ..Summary::default() } };
        match mode().as_str() {
            "refuses" => Summary { cards: 1, failed: 1, ..Summary::default() },
            "forgets" => wrote(want),
            _ => {
                let attrs = VariableAttributes::NON_VOLATILE
                    | VariableAttributes::BOOTSERVICE_ACCESS
                    | VariableAttributes::RUNTIME_ACCESS;
                let _ = uefi::runtime::set_variable(cstr16!("StormBootFecTestCard"), &handoff::VENDOR, attrs, b"rs");
                wrote(want)
            }
        }
    }

    /// The card as `read` builds it, for `write`'s closure, without its lines.
    fn read_quiet() -> PortAt {
        let port = Port { port: 1, supported: true, current: Fec::DeviceDefault, next: stored(), stored: true };
        PortAt {
            loc: String::from("84:00.0"),
            bdf: String::from("0000:84:00.0"),
            mac: Some(String::from("52:54:00:fe:c0:01")),
            speed: Some(connectx::Speed::G25),
            port,
        }
    }

    fn wrote(want: Fec) -> Summary {
        Summary {
            cards: 1,
            written: 1,
            wrote: alloc::vec![format!("84:00.0 port 1: device-default -> {}", want.name())],
            ..Summary::default()
        }
    }
}
