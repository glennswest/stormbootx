//! The boot's log over the network (#162).
//!
//! The serial console is the only record of a boot, and a BMC's
//! serial-over-LAN dies (server8, stormblock#476): what the machine printed
//! is then gone. So every console line is also sent as a stormcast frame
//! (`logwire.rs`): to the fleet's group `239.255.42.1:5514` (TTL 4) on every
//! NIC with a lease, and to a unicast collector when one is named, because
//! multicast does not leave a subnet and stormcentral (UDP 5514) is on
//! another one.
//!
//! - **Every line, from the first.** `begin` runs before the banner. With the
//!   boot panel up, `ui.rs`'s `ConOut` hands each string here; otherwise this
//!   module puts a `ConOut` of its own in the system table that keeps a copy
//!   and passes everything on unchanged. Either way the media's NIC drivers'
//!   lines are caught too.
//! - **Kept until a NIC holds a lease**, then sent at fixed points of the
//!   boot (`flush`): once the machine's name is known, after the claim,
//!   before the chain-load, before a self-update restarts, and on the
//!   fall-through before the NICs are given back. Never from inside a print:
//!   a line printed while the stack is mid-poll would re-enter it. At most
//!   [`KEEP_LINES`] lines and [`KEEP_BYTES`] bytes wait; past that the
//!   oldest go, and the next send says how many.
//! - **Where**: `syslog =` in `stormboot.conf`, else `syslog:` in the
//!   media's `install-config.yaml` (the key stormpump reads, #162):
//!   `host[:port]` (port 514 when bare), a name looked up at the lease's DNS
//!   server once. `off` sends nothing at all. Nothing named: the group only,
//!   and that is not an error.
//! - **HOSTNAME** is the best name known when the line is sent: the name the
//!   claim booted, else the DNS name or a stated tag, else `mac-<hex>` (what
//!   the engine calls a machine it has not named). stormcentral files lines
//!   by it. **TIMESTAMP** is the line's own time once NTP set the clock this
//!   boot (#77); before that `-`, as stormcast sends for a clock it does not
//!   believe, and the receiver keeps the arrival time.
//!
//! Best effort by construction: nothing here waits for a collector, fails a
//! boot, or prints more than a line about itself.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use uefi_raw::protocol::console::SimpleTextOutputProtocol;
use uefi_raw::table::boot::{BootServices, Tpl};
use uefi_raw::table::system::SystemTable;
use uefi_raw::{Boolean, Char16, Status};

use crate::logwire::{self, Target};
use crate::{clock, config, installconf, net, sntp};

/// Lines kept while no NIC holds a lease.
pub const KEEP_LINES: usize = 4000;
pub const KEEP_BYTES: usize = 512 * 1024;
/// A longer line is cut (a frame holds about 1400 bytes of it anyway).
const LINE_MAX: usize = 1024;
/// APP-NAME.
const TAG: &str = "stormbootx";
/// How long one destination may take to drain a send.
const SEND_MS: u64 = 3000;

/// How sure a name is; a surer one replaces a less sure one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// `mac-<hex>`, the engine's name for a machine nothing named.
    Mac = 1,
    /// The DNS name, or a tag the media states.
    Named = 2,
    /// The host the claim booted.
    Claimed = 3,
}

struct Log {
    st: *mut SystemTable,
    /// Lines waiting, with the tick each ended at.
    lines: VecDeque<(u64, String)>,
    bytes: usize,
    partial: String,
    dropped: usize,
    host: String,
    rank: Option<Rank>,
    off: bool,
    /// The unicast collector as named, and where it was named.
    target: Option<(String, u16, &'static str)>,
    /// The collector's address once looked up; `Err` is said once.
    resolved: Option<Result<[u8; 4], ()>>,
    /// A setting that could not be read, said at the first send.
    note: Option<String>,
    said: bool,
    ticks_per_ms: u64,
    /// The Unix time in ns at a tick, once NTP set the clock.
    clock: Option<(u64, u64)>,
    /// Our own `ConOut` and the one it passes everything to, when the panel
    /// is not the one in front.
    ours: *mut SimpleTextOutputProtocol,
    orig: *mut SimpleTextOutputProtocol,
    /// Every line so far, for the API's `/api/v1/log` (#167): kept with
    /// `syslog = off` too, and never drained by a send.
    history: VecDeque<String>,
    history_bytes: usize,
}

static mut LOG: Option<Log> = None;
/// Set while a string is being kept, so a timer's print during another
/// (snpwatch's hang watch, #88) is passed on and not kept twice over.
static mut BUSY: bool = false;

#[allow(static_mut_refs)]
fn log() -> Option<&'static mut Log> {
    unsafe { LOG.as_mut() }
}

fn bs(st: *mut SystemTable) -> &'static BootServices {
    unsafe { &*(*st).boot_services }
}

/// Start keeping lines. First thing in `main`, before the banner. Reads the
/// media's setting now: the media is not read after an attach (#46).
pub fn begin() {
    if log().is_some() {
        return;
    }
    let Some(st) = uefi::table::system_table_raw() else { return };
    let st = st.as_ptr();
    let orig = unsafe { (*st).stdout };
    if orig.is_null() {
        return;
    }
    let mut note = None;
    let stated = match config::stated_syslog() {
        Some(v) => Some((v, config::file_of(&["syslog"]))),
        None => config::read_bytes(installconf::PATH, installconf::MAX)
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|y| logwire::yaml_syslog(&y).map(String::from))
            .map(|v| (v, installconf::PATH)),
    };
    let (off, target) = match &stated {
        None => (false, None),
        Some((v, from)) => match logwire::target(v) {
            Some(Target::Off) => (true, None),
            Some(Target::To(h, p)) => (false, Some((String::from(h), p, *from))),
            None => {
                note = Some(format!("syslog {v:?} in {from} is not host[:port] or off; the group only"));
                (false, None)
            }
        },
    };
    let t0 = crate::entropy::ticks();
    uefi::boot::stall(core::time::Duration::from_millis(10));
    let ticks_per_ms = ((crate::entropy::ticks() - t0) / 10).max(1);
    let mut l = Log {
        st,
        lines: VecDeque::new(),
        bytes: 0,
        partial: String::new(),
        dropped: 0,
        host: String::new(),
        rank: None,
        off,
        target,
        resolved: None,
        note,
        said: false,
        ticks_per_ms,
        clock: None,
        ours: core::ptr::null_mut(),
        orig,
        history: VecDeque::new(),
        history_bytes: 0,
    };
    // Installed with `syslog = off` too: the API's log is kept either way.
    {
        l.ours = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(SimpleTextOutputProtocol {
            reset: hook_reset,
            output_string: hook_output,
            test_string: hook_test_string,
            query_mode: hook_query_mode,
            set_mode: hook_set_mode,
            set_attribute: hook_set_attribute,
            clear_screen: hook_clear_screen,
            set_cursor_position: hook_set_cursor,
            enable_cursor: hook_enable_cursor,
            mode: unsafe { (*orig).mode },
        }));
        unsafe { (*st).stdout = l.ours };
        crc(st);
    }
    unsafe { LOG = Some(l) };
}

/// Take our `ConOut` out of the system table again: before this image
/// returns to the firmware, which unloads it.
pub fn end() {
    let Some(l) = log() else { return };
    if l.ours.is_null() {
        return;
    }
    unsafe {
        if (*l.st).stdout == l.ours {
            (*l.st).stdout = l.orig;
            crc(l.st);
        }
    }
}

/// The system table's CRC, after its `ConOut` changed.
fn crc(st: *mut SystemTable) {
    unsafe {
        (*st).header.crc = 0;
        let mut c = 0u32;
        let size = (*st).header.size as usize;
        if (bs(st).calculate_crc32)(st as *mut core::ffi::c_void, size, &mut c) == Status::SUCCESS {
            (*st).header.crc = c;
        }
    }
}

/// The machine's name for the frames, if it is surer than the one held.
pub fn name(host: &str, rank: Rank) {
    let Some(l) = log() else { return };
    if host.is_empty() || l.rank.is_some_and(|r| r > rank) {
        return;
    }
    l.host = String::from(host);
    l.rank = Some(rank);
}

/// NTP gave the time (#77): Unix nanoseconds, now.
pub fn synced(unix_ns: u64) {
    if let Some(l) = log() {
        l.clock = Some((unix_ns, crate::entropy::ticks()));
    }
}

/// Keep a copy of what someone printed. From `ConOut.OutputString`: ours, or
/// the panel's (`ui.rs`).
pub fn capture(s: *const Char16) {
    let Some(l) = log() else { return };
    if s.is_null() {
        return;
    }
    let old = unsafe { (bs(l.st).raise_tpl)(Tpl::NOTIFY) };
    if unsafe { BUSY } {
        unsafe { (bs(l.st).restore_tpl)(old) };
        return;
    }
    unsafe { BUSY = true };
    let mut p = s;
    unsafe {
        while *p != 0 {
            match *p {
                0x0a => {
                    let line = core::mem::take(&mut l.partial);
                    if !line.trim().is_empty() {
                        keep(l, line);
                    }
                }
                0x0d => {}
                c => {
                    if l.partial.len() < LINE_MAX {
                        l.partial.push(char::from_u32(c as u32).unwrap_or('?'));
                    }
                }
            }
            p = p.add(1);
        }
        BUSY = false;
        (bs(l.st).restore_tpl)(old);
    }
}

fn keep(l: &mut Log, line: String) {
    l.history_bytes += line.len() + 1;
    l.history.push_back(line.clone());
    while l.history.len() > KEEP_LINES || l.history_bytes > KEEP_BYTES {
        match l.history.pop_front() {
            Some(old) => l.history_bytes -= old.len() + 1,
            None => break,
        }
    }
    if l.off {
        return;
    }
    l.bytes += line.len();
    l.lines.push_back((crate::entropy::ticks(), line));
    while l.lines.len() > KEEP_LINES || l.bytes > KEEP_BYTES {
        match l.lines.pop_front() {
            Some((_, old)) => {
                l.bytes -= old.len();
                l.dropped += 1;
            }
            None => break,
        }
    }
}

/// Every line kept so far, one per line, oldest first (the API's
/// `/api/v1/log`, #167). At most [`KEEP_LINES`] lines and [`KEEP_BYTES`].
pub fn history() -> String {
    let Some(l) = log() else { return String::new() };
    let old = unsafe { (bs(l.st).raise_tpl)(Tpl::NOTIFY) };
    let mut out = String::with_capacity(l.history_bytes);
    for line in l.history.iter() {
        out.push_str(line);
        out.push('\n');
    }
    unsafe { (bs(l.st).restore_tpl)(old) };
    out
}

/// The lines waiting, taken out under the raised TPL.
fn take(l: &mut Log) -> (Vec<(u64, String)>, usize) {
    let old = unsafe { (bs(l.st).raise_tpl)(Tpl::NOTIFY) };
    let lines: Vec<(u64, String)> = l.lines.drain(..).collect();
    let dropped = core::mem::take(&mut l.dropped);
    l.bytes = 0;
    unsafe { (bs(l.st).restore_tpl)(old) };
    (lines, dropped)
}

/// A line's TIMESTAMP: its own time once NTP set the clock, else `-`.
fn stamp(l: &Log, at: u64) -> String {
    let Some((ns, tick)) = l.clock else { return String::from("-") };
    let per_us = (l.ticks_per_ms / 1000).max(1);
    let us = if at >= tick {
        (ns / 1000).saturating_add((at - tick) / per_us)
    } else {
        (ns / 1000).saturating_sub((tick - at) / per_us)
    };
    let c = sntp::civil(us / 1_000_000);
    logwire::rfc3339(c.year, c.month, c.day, c.hour, c.minute, c.second, (us % 1_000_000) as u32)
}

fn ip_text(a: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

/// Send what is waiting, if a NIC holds a lease. Never waits for one, and
/// never fails: what cannot be sent now waits for the next call.
pub fn flush() {
    // Nothing is held across a call that prints: a print comes back here
    // through `capture`.
    let (off, said, host, target, resolved) = match log() {
        Some(l) => (l.off, l.said, l.host.clone(), l.target.clone(), l.resolved),
        None => return,
    };
    if !net::leased() {
        return;
    }
    if off {
        if !said {
            uefi::println!("log         : not sent over the network (syslog = off)");
            if let Some(l) = log() {
                l.said = true;
            }
        }
        return;
    }

    // The collector, looked up once.
    let to = match (&target, resolved) {
        (None, _) => None,
        (Some(_), Some(r)) => r.ok(),
        (Some((h, _, _)), None) => {
            let lease = net::leased_reply().unwrap_or_default();
            let r = clock::lookup(h, &lease);
            if let Err(e) = &r {
                uefi::println!("log         : {e}; the group only");
            }
            let r = r.map_err(|_| ());
            if let Some(l) = log() {
                l.resolved = Some(r);
            }
            r.ok()
        }
    };
    if !said {
        let unicast = match (&target, to) {
            (Some((h, p, from)), Some(_)) if config::parse_ipv4(h).is_some() => {
                format!(" and to {h}:{p} (syslog in {from})")
            }
            (Some((h, p, from)), Some(a)) => format!(" and to {h}:{p} ({}, syslog in {from})", ip_text(a)),
            _ => String::new(),
        };
        uefi::println!(
            "log         : sent to the stormcast group {}:{}{unicast} as {}",
            ip_text(logwire::GROUP),
            logwire::GROUP_PORT,
            if host.is_empty() { "-" } else { host.as_str() }
        );
        let note = log().and_then(|l| {
            l.said = true;
            l.note.take()
        });
        if let Some(n) = note {
            uefi::println!("log         : {n}");
        }
    }

    let Some(l) = log() else { return };
    let (lines, dropped) = take(l);
    let mut frames: Vec<Vec<u8>> = Vec::with_capacity(lines.len() + 1);
    if dropped > 0 {
        let msg = format!(
            "{dropped} earlier line(s) were not kept while no NIC held a lease (at most {KEEP_LINES} lines, {} KiB)",
            KEEP_BYTES / 1024
        );
        frames.push(logwire::frame("-", &l.host, TAG, &msg));
    }
    for (at, line) in &lines {
        frames.push(logwire::frame(&stamp(l, *at), &l.host, TAG, line));
    }
    if frames.is_empty() {
        return;
    }
    net::udp_send(logwire::GROUP, logwire::GROUP_PORT, &frames, true, Some(logwire::GROUP_TTL), SEND_MS);
    if let (Some(a), Some((_, p, _))) = (to, &target) {
        net::udp_send(a, *p, &frames, false, None, SEND_MS);
    }
}

// ------------------------------------------------------- the ConOut hook

fn orig() -> *mut SimpleTextOutputProtocol {
    log().map(|l| l.orig).unwrap_or(core::ptr::null_mut())
}

unsafe extern "efiapi" fn hook_output(_this: *mut SimpleTextOutputProtocol, s: *const Char16) -> Status {
    capture(s);
    let o = orig();
    if o.is_null() {
        return Status::SUCCESS;
    }
    unsafe { ((*o).output_string)(o, s) }
}

unsafe extern "efiapi" fn hook_reset(_this: *mut SimpleTextOutputProtocol, ext: Boolean) -> Status {
    let o = orig();
    if o.is_null() {
        return Status::SUCCESS;
    }
    unsafe { ((*o).reset)(o, ext) }
}

unsafe extern "efiapi" fn hook_test_string(_this: *mut SimpleTextOutputProtocol, s: *const Char16) -> Status {
    let o = orig();
    if o.is_null() {
        return Status::SUCCESS;
    }
    unsafe { ((*o).test_string)(o, s) }
}

unsafe extern "efiapi" fn hook_query_mode(
    _this: *mut SimpleTextOutputProtocol,
    mode: usize,
    cols: *mut usize,
    rows: *mut usize,
) -> Status {
    let o = orig();
    if o.is_null() {
        return Status::UNSUPPORTED;
    }
    unsafe { ((*o).query_mode)(o, mode, cols, rows) }
}

unsafe extern "efiapi" fn hook_set_mode(_this: *mut SimpleTextOutputProtocol, mode: usize) -> Status {
    let o = orig();
    if o.is_null() {
        return Status::UNSUPPORTED;
    }
    unsafe { ((*o).set_mode)(o, mode) }
}

unsafe extern "efiapi" fn hook_set_attribute(_this: *mut SimpleTextOutputProtocol, attr: usize) -> Status {
    let o = orig();
    if o.is_null() {
        return Status::SUCCESS;
    }
    unsafe { ((*o).set_attribute)(o, attr) }
}

unsafe extern "efiapi" fn hook_clear_screen(_this: *mut SimpleTextOutputProtocol) -> Status {
    let o = orig();
    if o.is_null() {
        return Status::SUCCESS;
    }
    unsafe { ((*o).clear_screen)(o) }
}

unsafe extern "efiapi" fn hook_set_cursor(_this: *mut SimpleTextOutputProtocol, col: usize, row: usize) -> Status {
    let o = orig();
    if o.is_null() {
        return Status::SUCCESS;
    }
    unsafe { ((*o).set_cursor_position)(o, col, row) }
}

unsafe extern "efiapi" fn hook_enable_cursor(_this: *mut SimpleTextOutputProtocol, on: Boolean) -> Status {
    let o = orig();
    if o.is_null() {
        return Status::SUCCESS;
    }
    unsafe { ((*o).enable_cursor)(o, on) }
}
