//! Each start's console on the medium, and in the claim (#213).
//!
//! A self-update trial that never reaches the network leaves nothing behind:
//! no lease, so no syslog (`netlog.rs`); no BMC capture on a maintenance
//! boot; and the next start's claim said only `failed:<n>` (C2NR0Q2's serial
//! 8, 2026-11-14). So the boot keeps its own record on the medium it started
//! from:
//!
//! - **`\stormboot\last.log`**: every line of this start so far
//!   (`netlog::history`, cut to [`MAX`] by `logwire::excerpt`), rewritten at
//!   fixed points from the first: after `selfupdate::at_start`, after each
//!   media driver starts, after the drivers, after `net::up`, at every
//!   `netlog::flush` (the name, the claim, the hold, the fall-through), and
//!   before a revert chain-loads. Only when the log grew since the last
//!   write. The previous start's file is read before the first write.
//! - **`\stormboot\trial.log`**: the start that puts a failed set back keeps
//!   the previous start's `last.log` as the failed trial's, before it
//!   chain-loads the set it restored.
//! - **The claim's `agent`** (`registry::agent`) carries `trial_log` when the
//!   boot reports `failed:<n>`, `last_log` (the previous start) and `log`
//!   (this start up to the claim), each JSON-escaped and cut to its share
//!   of stormblock's 16 KiB `agent` (`CLAIM_EXTRA_MAX`, stormblock#177).
//!   [`fit`] drops them before the agent could be refused, never `update`.
//!
//! Never written after an image is attached (#46: nothing on the medium is
//! opened while the firmware reads the attached image) until `withdraw`
//! takes it back ([`seal`], [`unseal`]); never on a read-only medium (an ISO
//! on virtual media) or a RAM disk (UEFI HTTP boot, gone at reset). Nothing
//! here fails a boot or waits; a write that fails is said once.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::selfupdate::Vol;
use crate::{config, logwire, netlog};

/// This start's lines, rewritten through the boot.
pub const LAST_PATH: &str = r"\stormboot\last.log";
/// The last start of the set that failed its trial here.
pub const TRIAL_PATH: &str = r"\stormboot\trial.log";
/// The most either file holds.
pub const MAX: usize = 64 * 1024;

/// Each log's share of the claim's `agent`, escaped.
const TRIAL_JSON: usize = 6 * 1024;
const LAST_JSON: usize = 3 * 1024;
const LOG_JSON: usize = 4 * 1024;
/// The most the whole `agent` object may be; stormblock keeps up to 16 KiB.
pub const AGENT_MAX: usize = 15 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Medium {
    /// `begin` has not run.
    Unknown,
    Writable,
    /// Read-only, a RAM disk, or no file system: nothing is written.
    Off,
}

struct Keep {
    medium: Medium,
    /// An image is attached (#46).
    sealed: bool,
    /// The previous start's `last.log`, and the failed trial's.
    prev: Option<String>,
    trial: Option<String>,
    /// `netlog::kept()` at the last write.
    written: u64,
    said: bool,
}

static mut KEEP: Keep =
    Keep { medium: Medium::Unknown, sealed: false, prev: None, trial: None, written: 0, said: false };

#[allow(static_mut_refs)]
fn keep() -> &'static mut Keep {
    unsafe { &mut KEEP }
}

fn text(b: Vec<u8>) -> String {
    String::from_utf8_lossy(&b).into_owned()
}

/// Read what the last start left, and whether this medium is written. Before
/// `selfupdate::at_start`, which may put a failed set back.
pub fn begin() {
    let k = keep();
    if k.medium != Medium::Unknown {
        return;
    }
    k.medium = Medium::Off;
    let Ok(mut vol) = Vol::mount() else { return };
    k.prev = vol.read(LAST_PATH, MAX).ok().flatten().map(text);
    k.trial = vol.read(TRIAL_PATH, MAX).ok().flatten().map(text);
    let read_only = vol.fs_info().map(|i| i.read_only()).unwrap_or(true);
    drop(vol);
    if read_only {
        uefi::println!("last log    : not kept (the boot medium is read-only)");
    } else if config::boot_from_ram_disk() {
        uefi::println!("last log    : not kept (the boot medium is a RAM disk)");
    } else {
        k.medium = Medium::Writable;
        let prev = match &k.prev {
            Some(p) => format!("the previous start's is {} line(s)", p.lines().count()),
            None => String::from("none from a previous start"),
        };
        uefi::println!("last log    : this start's console is kept in {LAST_PATH} ({prev})");
    }
}

/// The start that puts serial `serial` back after its failed trial: the
/// previous start was that set's last, and its log becomes `trial.log`.
pub fn keep_trial(serial: u64) {
    let k = keep();
    if k.medium != Medium::Writable {
        return;
    }
    let body = match &k.prev {
        Some(p) => p.clone(),
        None => format!("(the last start of serial {serial} left no {LAST_PATH})\n"),
    };
    let r = Vol::mount().and_then(|mut v| v.write(TRIAL_PATH, body.as_bytes()));
    match r {
        Ok(()) => uefi::println!(
            "last log    : serial {serial}'s last start ({} line(s)) kept in {TRIAL_PATH}",
            body.lines().count()
        ),
        Err(e) => uefi::println!("last log    : serial {serial}'s last start not kept in {TRIAL_PATH} ({e})"),
    }
    k.trial = Some(body);
}

/// Write this start's lines to `last.log`, if they grew since the last write.
pub fn save() {
    let k = keep();
    if k.medium != Medium::Writable || k.sealed {
        return;
    }
    let kept = netlog::kept();
    if kept == k.written {
        return;
    }
    let body = logwire::excerpt(&netlog::history(), MAX);
    match Vol::mount().and_then(|mut v| v.write(LAST_PATH, body.as_bytes())) {
        Ok(()) => k.written = kept,
        Err(e) if !k.said => {
            k.said = true;
            uefi::println!("last log    : {LAST_PATH} not written ({e})");
        }
        Err(_) => {}
    }
}

/// An image is attached: nothing on the medium is opened until it is gone.
pub fn seal() {
    keep().sealed = true;
}

/// The attached image is gone (or none was published).
pub fn unseal() {
    keep().sealed = false;
}

/// The previous start's lines, for the API.
pub fn previous() -> Option<&'static str> {
    keep().prev.as_deref()
}

/// The failed trial's lines, for the API.
pub fn trial() -> Option<&'static str> {
    keep().trial.as_deref()
}

/// The claim's log fields (`"trial_log":"…"`, …), most important first:
/// the failed trial's only while this boot reports `failed:<n>`.
pub fn claim_fields(update: Option<&str>) -> Vec<String> {
    let k = keep();
    let mut out = Vec::new();
    let mut add = |name: &str, body: &str, budget: usize| {
        let e = logwire::json_excerpt(body, budget);
        if !e.is_empty() {
            out.push(format!("\"{name}\":\"{e}\""));
        }
    };
    if update.is_some_and(|u| u.starts_with("failed:")) {
        if let Some(t) = &k.trial {
            add("trial_log", t, TRIAL_JSON);
        }
    }
    if let Some(p) = &k.prev {
        add("last_log", p, LAST_JSON);
    }
    add("log", &netlog::history(), LOG_JSON);
    out
}

/// `fields` and as many of `logs` as keep the object within [`AGENT_MAX`],
/// dropping from the end: a log is never why the agent is not kept.
pub fn fit(fields: Vec<String>, mut logs: Vec<String>) -> String {
    loop {
        let all: Vec<&str> = fields.iter().chain(logs.iter()).map(String::as_str).collect();
        let s = format!("{{{}}}", all.join(","));
        if s.len() <= AGENT_MAX || logs.pop().is_none() {
            return s;
        }
    }
}
