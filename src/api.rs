//! stormbootx's own API, and holding in it when a boot fails (#167).
//!
//! The owner (2026-10-21): "more important to have api control". A machine
//! whose BMC answers nothing (server8) is still reachable while stormbootx
//! runs, so stormbootx answers HTTP on every NIC from the moment its stack
//! is up (`net.rs` serves the sockets from every poll, `answer` here says
//! what to send):
//!
//! | request | answer |
//! |---|---|
//! | `GET /api/v1/status` | what it is doing, its names, the last command and its result |
//! | `GET /api/v1/inventory` | the firmware inventory (`inventory.rs`, `"v":2`), drives first |
//! | `GET /api/v1/log` | the console lines so far (`netlog.rs` keeps them) |
//! | `POST /api/v1/command` | a signed command (`command.rs`): 202 when taken |
//!
//! Reads are open, as the log already goes to the group in the clear and the
//! inventory with every claim. A command counts only by its Ed25519
//! signature: `manifest::RELEASE_KEYS` (stormcentral's), and `command_key =`
//! on the media (the laptop app's). Its host must be one of this machine's
//! names and its id above `StormBootCommandId` (non-volatile), which is
//! raised before the command runs, so no command is done twice.
//!
//! The engine's host record carries the same object (stormblock#494): it is
//! read once a boot before the intent, and every 5 s while holding, for when
//! the API can't be reached (another subnet, NAT). A command from there is
//! reported back with `…/command/done`.
//!
//! A command is taken at fixed points, never from inside a poll: before the
//! intent is read, before the chain-load, and in the hold. **Holding**:
//! stormbootx stays up, its panel and log saying so, the API answering,
//! until a command or the time `hold =` gives. A failed network boot holds
//! for `hold =` seconds (`forever`, `off`; also `stormbootx_hold:` in
//! install-config.yaml), retrying the boot every `hold_retry =` minutes; one
//! with nothing local to boot holds until a command. The binary's default is
//! `off`, as before; the golden media say `hold = 120`. A boot path still
//! never needs the network to boot without it: a hold ends in the
//! fall-through, as every failure did.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::UnsafeCell;

use uefi::runtime::{ResetType, VariableAttributes, VariableVendor};
use uefi::{cstr16, Status};

use crate::command::{self, Signed, Verb};
use crate::{config, handoff, hardware, manifest, net, netlog, registry, ui};

/// The port when the media names none.
pub const DEFAULT_PORT: u16 = 9180;
/// How often a hold reads the engine's record.
const RECORD_EVERY_MS: u64 = 5_000;
/// How often a hold sends the log.
const FLUSH_EVERY_MS: u64 = 10_000;

/// Where a command came from, for its report.
#[derive(Clone, Copy, PartialEq)]
enum Source {
    Api([u8; 4]),
    Record,
}

/// The engine, once the boot knows it: where to read the record.
struct Engine {
    portal: [u8; 4],
    port: u16,
    host: String,
    name: String,
}

struct State {
    port: Option<u16>,
    keys: Vec<[u8; 32]>,
    /// Where each key came from, for the console.
    key_note: String,
    names: Vec<String>,
    phase: &'static str,
    detail: String,
    floor: u64,
    pending: Option<(Signed, Verb, Source)>,
    /// `{"id":…,"command":…,"result":…}` of the last command done.
    last: Option<String>,
    engine: Option<Engine>,
    /// The record's last id reported as taken before, so a hold says it once.
    record_said: u64,
    /// The hold after a failed boot, read with the rest of the media.
    hold: (Hold, u64, &'static str),
}

struct Global(UnsafeCell<Option<State>>);
// Boot services are single-threaded; `answer` runs inside a poll and only
// reads and queues.
unsafe impl Sync for Global {}
static API: Global = Global(UnsafeCell::new(None));

fn st() -> Option<&'static mut State> {
    unsafe { (*API.0.get()).as_mut() }
}

/// A key the OVMF tests sign with, compiled in only when the build sets
/// `STORMBOOTX_UPDATE_TEST_KEY` (`tests/update-ovmf.sh`). No golden does.
fn test_key() -> Option<[u8; 32]> {
    option_env!("STORMBOOTX_UPDATE_TEST_KEY").and_then(manifest::hex32)
}

/// Read the media's `api =` and `command_key =`, and the floor. Before
/// `net::up`, which opens the listening sockets.
pub fn init() {
    let conf = config::conf_text().unwrap_or_default();
    let port = match config::field(&conf, "api").as_deref().map(str::trim) {
        Some("off") => None,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => Some(n),
            _ => {
                uefi::println!("api         : `api = {p}` is not a port or off; {DEFAULT_PORT}");
                Some(DEFAULT_PORT)
            }
        },
        None => Some(DEFAULT_PORT),
    };
    let mut keys = manifest::RELEASE_KEYS.to_vec();
    let mut note = format!("{} release key(s)", keys.len());
    if let Some(k) = test_key() {
        keys.push(k);
        note.push_str(", a TEST key");
    }
    match config::field(&conf, "command_key") {
        Some(v) => match command::key_hex(&v) {
            Some(k) => {
                keys.push(k);
                note.push_str(&format!(", command_key in {}", config::file_of(&["command_key"])));
            }
            None => uefi::println!("api         : command_key is not 64 hex digits; ignored"),
        },
        None => {}
    }
    let floor = floor();
    match port {
        Some(p) => uefi::println!("api         : port {p} once a NIC holds a lease; commands signed by {note}"),
        None => uefi::println!("api         : off (api = off in {})", config::file_of(&["api"])),
    }
    unsafe {
        *API.0.get() = Some(State {
            port,
            keys,
            key_note: note,
            names: Vec::new(),
            phase: "starting",
            detail: String::new(),
            floor,
            pending: None,
            last: None,
            engine: None,
            record_said: 0,
            hold: read_policy(),
        })
    };
}

/// The port the API listens on, if any.
pub fn port() -> Option<u16> {
    st().and_then(|s| s.port)
}

/// A name a command may be addressed to: the claimed host, the DNS name, a
/// stated tag, the serial, `mac-<hex>`.
pub fn name(n: &str) {
    let Some(s) = st() else { return };
    if !n.is_empty() && !s.names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
        s.names.push(n.to_string());
    }
}

/// What the boot is doing, for `/status`.
pub fn phase(p: &'static str, detail: &str) {
    if let Some(s) = st() {
        s.phase = p;
        s.detail = detail.to_string();
    }
}

/// The engine and the name its record is read under.
pub fn engine(portal: [u8; 4], port: u16, host: &str, name: &str) {
    if let Some(s) = st() {
        s.engine = Some(Engine { portal, port, host: host.to_string(), name: name.to_string() });
    }
}

// ------------------------------------------------------------------ answers

/// The answer to one request. Runs inside a poll of the stack: it reads and
/// queues, and never touches the network.
pub fn answer(method: &str, path: &str, body: &[u8], peer: [u8; 4]) -> Vec<u8> {
    let Some(s) = st() else { return command::error(503, "starting") };
    let path = path.split('?').next().unwrap_or(path);
    match (method, path) {
        ("GET", "/api/v1/status") => command::response(200, "application/json", status_json(s).as_bytes()),
        ("GET", "/api/v1/inventory") => match hardware::cached() {
            Some(j) => command::response(200, "application/json", j.as_bytes()),
            None => command::error(503, "the inventory is collected at the claim, or when holding"),
        },
        ("GET", "/api/v1/log") => {
            command::response(200, "text/plain; charset=utf-8", netlog::history().as_bytes())
        }
        ("POST", "/api/v1/command") => {
            let text = core::str::from_utf8(body).unwrap_or("");
            match accept(s, text, Source::Api(peer)) {
                Ok(()) => {
                    let (c, v, _) = s.pending.as_ref().unwrap();
                    let when = if s.phase == "holding" { "now" } else { "at the boot's next step" };
                    let b = format!(
                        "{{\"accepted\":{},\"command\":\"{}\",\"when\":\"{when}\"}}",
                        c.id,
                        v.name()
                    );
                    command::response(202, "application/json", b.as_bytes())
                }
                Err((code, why)) => {
                    uefi::println!("command     : refused from {}: {why}", ip(peer));
                    command::error(code, &why)
                }
            }
        }
        (_, "/api/v1/status" | "/api/v1/inventory" | "/api/v1/log" | "/api/v1/command") => {
            command::error(405, "method not allowed")
        }
        _ => command::error(404, "not found"),
    }
}

fn status_json(s: &State) -> String {
    let names: Vec<String> = s.names.iter().map(|n| format!("\"{}\"", command::escape(n))).collect();
    let addrs: Vec<String> = net::addresses().iter().map(|a| format!("\"{}\"", ip(*a))).collect();
    format!(
        "{{\"agent\":\"stormbootx\",\"version\":\"{}\",\"commit\":\"{}\",\"phase\":\"{}\",\"detail\":\"{}\",\
         \"names\":[{}],\"addresses\":[{}],\"command_id\":{},\"pending\":{},\"last\":{}}}",
        env!("CARGO_PKG_VERSION"),
        option_env!("STORMBOOTX_BUILD").unwrap_or("unstamped"),
        s.phase,
        command::escape(&s.detail),
        names.join(","),
        addrs.join(","),
        s.floor,
        match &s.pending {
            Some((c, v, _)) => format!("{{\"id\":{},\"command\":\"{}\"}}", c.id, v.name()),
            None => "null".into(),
        },
        s.last.as_deref().unwrap_or("null")
    )
}

/// Check a command and queue it: signature, host, id, verb. The floor is
/// raised here, before it runs.
fn accept(s: &mut State, body: &str, from: Source) -> Result<(), (u16, String)> {
    if s.phase == "handed off" {
        return Err((409, "the image's bootloader is running; stormbootx takes no command now".into()));
    }
    if s.pending.is_some() {
        return Err((409, "a command is already waiting".into()));
    }
    let c = command::parse_body(body).map_err(|e| (400, e))?;
    let text = command::signed_text(&c);
    let sig = ed25519_compact::Signature::new(c.sig);
    if !s.keys.iter().any(|k| ed25519_compact::PublicKey::new(*k).verify(text.as_bytes(), &sig).is_ok()) {
        return Err((403, format!("the signature is not by any of {} key(s) ({})", s.keys.len(), s.key_note)));
    }
    command::for_me(&c, &s.names, s.floor).map_err(|e| (403, e))?;
    let v = command::verb(&c.command, &c.args).map_err(|e| (400, e))?;
    s.floor = c.id;
    set_floor(c.id);
    uefi::println!(
        "command     : {} {}{} (id {}, from {}{}), signature good",
        v.name(),
        c.args,
        if c.args.is_empty() { "" } else { " " },
        c.id,
        match from {
            Source::Api(p) => format!("the API, {}", ip(p)),
            Source::Record => String::from("the engine's record"),
        },
        if c.by.is_empty() { String::new() } else { format!(", by {}", c.by) }
    );
    s.pending = Some((c, v, from));
    Ok(())
}

/// `StormBootCommandId`: the highest command id this machine has taken.
fn floor() -> u64 {
    let mut buf = [0u8; 24];
    match uefi::runtime::get_variable(cstr16!("StormBootCommandId"), &handoff::VENDOR, &mut buf) {
        Ok((d, _)) => core::str::from_utf8(d).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0),
        Err(_) => 0,
    }
}

fn set_floor(id: u64) {
    let attrs =
        VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
    let v = id.to_string();
    if let Err(e) = uefi::runtime::set_variable(cstr16!("StormBootCommandId"), &handoff::VENDOR, attrs, v.as_bytes()) {
        uefi::println!("command     : StormBootCommandId not set ({:?}); a replay is refused this boot only", e.status());
    }
}

fn ip(a: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

// ---------------------------------------------------------------- the record

/// Read the engine's record once and queue what it carries.
fn read_record() {
    let Some(s) = st() else { return };
    if s.pending.is_some() {
        return;
    }
    let Some(e) = &s.engine else { return };
    let (portal, port, host, name) = (e.portal, e.port, e.host.clone(), e.name.clone());
    let (status, body) = match registry::command(portal, port, &host, &name) {
        Ok(r) => r,
        Err(_) => return,
    };
    if status == 404 {
        return;
    }
    if status != 200 {
        uefi::println!("command     : the engine's record for {name} answered HTTP {status}");
        return;
    }
    let Some(s) = st() else { return };
    match accept(s, &body, Source::Record) {
        Ok(()) => {}
        Err((_, why)) => {
            // One the engine has not cleared yet, done here before.
            let id = command::json_u64(&body, "id").unwrap_or(0);
            if id != 0 && id <= s.floor {
                if s.record_said != id {
                    s.record_said = id;
                    let _ = registry::command_done(portal, port, &host, &name, id, "taken before");
                }
                return;
            }
            if s.record_said != id {
                s.record_said = id;
                uefi::println!("command     : the engine's record for {name} refused: {why}");
            }
        }
    }
}

/// Say what a command did: the console, `/status`, and the engine when it
/// came from the record.
fn done(c: &Signed, v: &Verb, from: Source, result: &str) {
    uefi::println!("command     : {} (id {}): {result}", v.name(), c.id);
    let Some(s) = st() else { return };
    s.last = Some(format!(
        "{{\"id\":{},\"command\":\"{}\",\"result\":\"{}\"}}",
        c.id,
        v.name(),
        command::escape(result)
    ));
    if from == Source::Record {
        if let Some(e) = &s.engine {
            let (portal, port, host, name) = (e.portal, e.port, e.host.clone(), e.name.clone());
            match registry::command_done(portal, port, &host, &name, c.id, result) {
                Ok(200..=299) => {}
                Ok(code) => uefi::println!("command     : the engine answered HTTP {code} to done"),
                Err(e) => uefi::println!("command     : done not reported to the engine ({e})"),
            }
        }
    }
}

// ---------------------------------------------------------------- execution

/// What the boot does after a command.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Next {
    /// Go on as it was going.
    Go,
    /// Claim and boot, whatever the intent says.
    Install,
    /// Boot the local disk, nothing claimed.
    Local,
    /// Run the boot again from the engine.
    Retry,
    /// Stay in stormbootx until another command.
    Hold,
}

/// Take a command waiting from the API, else read the engine's record once,
/// and do it. The boot's fixed points call this.
pub fn checkpoint(read_engine: bool) -> Next {
    if read_engine {
        read_record();
    }
    match st().and_then(|s| s.pending.take()) {
        Some((c, v, from)) => execute(c, v, from),
        None => Next::Go,
    }
}

fn execute(c: Signed, v: Verb, from: Source) -> Next {
    match &v {
        Verb::Reboot | Verb::Poweroff => {
            done(&c, &v, from, if v == Verb::Reboot { "resetting" } else { "powering off (ACPI S5)" });
            reset(if v == Verb::Reboot { ResetType::COLD } else { ResetType::SHUTDOWN })
        }
        Verb::BootNext(n) => match boot_next(*n) {
            Ok(what) => {
                done(&c, &v, from, &format!("BootNext = {what}; resetting"));
                reset(ResetType::COLD)
            }
            Err(e) => {
                done(&c, &v, from, &format!("refused: {e}"));
                Next::Go
            }
        },
        Verb::Install => {
            done(&c, &v, from, "claiming, whatever the intent says");
            Next::Install
        }
        Verb::Local => {
            done(&c, &v, from, "booting the local disk");
            Next::Local
        }
        Verb::Retry => {
            done(&c, &v, from, "booting again from the engine");
            Next::Retry
        }
        Verb::Hold => {
            done(&c, &v, from, "holding until another command");
            Next::Hold
        }
        Verb::Inventory => {
            hardware::refresh();
            let j = hardware::json();
            done(&c, &v, from, &format!("collected again: {} bytes (GET /api/v1/inventory)", j.len()));
            Next::Go
        }
        Verb::Format(serial) => {
            let r = hardware::wipe(serial);
            // The inventory with the drive as it is now.
            let _ = hardware::json();
            match r {
                Ok(what) => done(&c, &v, from, &format!("wiped {what}")),
                Err(e) => done(&c, &v, from, &format!("refused: {e}")),
            }
            Next::Go
        }
        Verb::Maint => {
            done(
                &c,
                &v,
                from,
                "refused: no maintenance image is served yet (baremetalservices's image as a namespace)",
            );
            Next::Go
        }
    }
}

/// The log out and the reply sent, then `ResetSystem`.
fn reset(how: ResetType) -> ! {
    netlog::flush();
    // The API's 202 and the record's done are on the wire before the reset.
    net::pump(1_000);
    ui::end();
    uefi::runtime::reset(how, Status::SUCCESS, None)
}

/// `BootNext = n`, when `Boot<n>` exists. Says which entry it is.
fn boot_next(n: u16) -> Result<String, String> {
    let name = format!("Boot{n:04X}");
    let mut wide = [0u16; 9];
    let var = uefi::CStr16::from_str_with_buf(&name, &mut wide).map_err(|_| "bad name".to_string())?;
    let mut buf = alloc::vec![0u8; 4096];
    let (opt, _) = uefi::runtime::get_variable(var, &VariableVendor::GLOBAL_VARIABLE, &mut buf)
        .map_err(|e| format!("there is no {name} ({:?})", e.status()))?;
    // EFI_LOAD_OPTION: attributes (4), path length (2), the description in
    // UCS-2 up to its NUL.
    let desc: String = opt
        .get(6..)
        .unwrap_or(&[])
        .chunks_exact(2)
        .map(|w| u16::from_le_bytes([w[0], w[1]]))
        .take_while(|&w| w != 0)
        .map(|w| char::from_u32(w as u32).unwrap_or('?'))
        .collect();
    let attrs =
        VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
    uefi::runtime::set_variable(cstr16!("BootNext"), &VariableVendor::GLOBAL_VARIABLE, attrs, &n.to_le_bytes())
        .map_err(|e| format!("BootNext not set ({:?})", e.status()))?;
    Ok(format!("{name} \"{desc}\""))
}

// --------------------------------------------------------------------- hold

/// How long a failed boot holds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hold {
    Off,
    Secs(u64),
    Forever,
}

/// The hold after a failed boot, its retry in minutes, and where it was
/// said.
pub fn policy() -> (Hold, u64, &'static str) {
    st().map(|s| s.hold).unwrap_or((Hold::Off, 0, "nowhere"))
}

/// `hold =` (media), else `stormbootx_hold:` (install-config.yaml): `off`,
/// `forever`, or seconds (`120`, `2m`). Off when neither says. With
/// `hold_retry =` / `stormbootx_hold_retry:` minutes, 0 for never. Read at
/// start: the media is not read after an attach (#46).
fn read_policy() -> (Hold, u64, &'static str) {
    let conf = config::conf_text().unwrap_or_default();
    let yaml = || {
        config::read_bytes(crate::installconf::PATH, crate::installconf::MAX)
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default()
    };
    let (hold, from) = match config::field(&conf, "hold") {
        Some(v) => (Some(v), config::file_of(&["hold"])),
        None => (crate::logwire::yaml_value(&yaml(), "stormbootx_hold").map(String::from), "install-config.yaml"),
    };
    let retry = config::field(&conf, "hold_retry")
        .or_else(|| crate::logwire::yaml_value(&yaml(), "stormbootx_hold_retry").map(String::from));
    let hold = match hold.as_deref().map(str::trim) {
        None | Some("off") | Some("0") => Hold::Off,
        Some("forever") => Hold::Forever,
        Some(v) => match secs(v) {
            Some(n) => Hold::Secs(n),
            None => {
                uefi::println!("hold        : `{v}` in {from} is not seconds, forever or off; off");
                Hold::Off
            }
        },
    };
    let retry = retry.as_deref().and_then(|r| r.trim().parse::<u64>().ok()).unwrap_or(0);
    (hold, retry, from)
}

fn secs(v: &str) -> Option<u64> {
    match v.strip_suffix('m') {
        Some(m) => m.trim().parse::<u64>().ok().map(|m| m * 60),
        None => v.strip_suffix('s').unwrap_or(v).trim().parse().ok(),
    }
}

/// How a hold ended.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum After {
    /// Fall through to the local disk, as before the hold.
    FallThrough,
    /// Run the boot again; `true` claims whatever the intent says.
    Retry(bool),
}

/// Stay in stormbootx with the API up (#167): until a command, the time
/// `how` gives, or the `retry` minutes, whichever comes first. `L` on the
/// keyboard falls through at once.
pub fn hold(why: &str, how: Hold, retry_min: u64) -> After {
    let addrs = net::addresses();
    let Some(port) = port() else { return After::FallThrough };
    if addrs.is_empty() || how == Hold::Off {
        return After::FallThrough;
    }
    let mut how = how;
    let at: Vec<String> = addrs.iter().map(|a| format!("http://{}:{port}", ip(*a))).collect();
    // What a person or stormcentral reads from it.
    let _ = hardware::json();
    phase("holding", why);
    uefi::println!("");
    uefi::println!("no network boot: {why}");
    let span = |h: Hold| match h {
        Hold::Secs(n) => format!("for {n} s, then the local disk"),
        _ => String::from("until a command"),
    };
    uefi::println!(
        "hold        : holding {}; the API at {} (status, inventory, log, signed commands){}",
        span(how),
        at.join(", "),
        if retry_min > 0 { format!("; the boot again every {retry_min} min") } else { String::new() }
    );
    ui::problem(why);
    ui::next(&format!("holding: API at {}  (L: the local disk now)", at.join(", ")));
    netlog::flush();

    let (mut t, mut record_at, mut flush_at) = (0u64, 0u64, FLUSH_EVERY_MS);
    let retry_ms = retry_min * 60_000;
    loop {
        net::pump(100);
        t += 100;
        if t >= record_at {
            record_at = t + RECORD_EVERY_MS;
            read_record();
        }
        let next = checkpoint(false);
        match next {
            Next::Go => {}
            Next::Hold => {
                how = Hold::Forever;
                phase("holding", "told to hold");
            }
            Next::Install => return After::Retry(true),
            Next::Retry => return After::Retry(false),
            Next::Local => return After::FallThrough,
        }
        if t >= flush_at {
            flush_at = t + FLUSH_EVERY_MS;
            netlog::flush();
        }
        if matches!(ui::take_char(), Some(Some('l' | 'L'))) {
            uefi::println!("hold        : L pressed; the local disk");
            return After::FallThrough;
        }
        if let Hold::Secs(n) = how {
            if t >= n * 1000 {
                uefi::println!("hold        : {n} s and no command; the local disk");
                return After::FallThrough;
            }
        }
        if retry_ms > 0 && t >= retry_ms {
            uefi::println!("hold        : {retry_min} min; booting again");
            return After::Retry(false);
        }
    }
}
