//! The boot screen (#144): one panel, redrawn in place, instead of a
//! scrolling log.
//!
//! Owner, 2026-10-14: "can we do a better ui for stormbootx. It looks messy
//! at times". The screen shows a header (version, machine, time), the boot's
//! steps with a state and a time each, a progress bar while the image is
//! read, and, when something goes wrong, the reason and what happens next.
//! Every log line still goes to the serial console exactly as before (the
//! consoles stormcentral keeps, and every OVMF test, read it), and to a
//! second screen behind F2. Nothing is mixed into the panel.
//!
//! **How.** `begin` puts its own `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL` in the
//! system table's `ConOut` (the firmware's ConSplitter, restored by `end`),
//! so every line printed by anyone (this binary, the media's NIC drivers, the
//! bootloader it starts) lands in `out_string`: kept for F2 and written to
//! the serial consoles only. The panel is drawn on the screen consoles
//! directly. Which is which: a console whose handle carries GOP is a screen
//! (GraphicsConsole); every other one is serial (a terminal on a UART, a
//! BMC's redirection). A machine with no screen console, or `ui = log` in
//! `stormboot.conf`, gets today's output and no panel.
//!
//! `end` hands `ConOut` back with a cleared screen before anything is left
//! to the firmware or an interactive program (the network console, a local
//! disk's bootloader, a return to the boot manager or the EFI Shell). The
//! attached image's bootloader runs under the panel: stormuefi prints a few
//! lines and then the kernel takes the screen.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

use uefi::boot::{self, SearchType};
use uefi_raw::protocol::console::{GraphicsOutputProtocol, InputKey, SimpleTextOutputProtocol};
use uefi_raw::table::boot::BootServices;
use uefi_raw::table::system::SystemTable;
use uefi_raw::{Boolean, Char16, Status};

/// The boot's steps, in order. The owner's list (#144).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Firmware = 0,
    Nic,
    Dhcp,
    Engine,
    Intent,
    Claim,
    Image,
    Handoff,
}

const LABELS: [&str; 8] = [
    "firmware handoff",
    "NIC up",
    "DHCP",
    "forge reached",
    "boot intent",
    "claim",
    "image loaded",
    "handoff",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Active,
    Done,
    Failed,
    Skipped,
}

struct Line {
    state: State,
    detail: String,
    start: u64,
    end: u64,
}

// EFI text attributes: foreground | background << 4.
const NORMAL: usize = 0x07;
const BRIGHT: usize = 0x0F;
const DIM: usize = 0x08;
const GREEN: usize = 0x0A;
const YELLOW: usize = 0x0E;
const RED: usize = 0x0C;
const CYAN: usize = 0x0B;
const BAR: usize = 0x70; // black on light grey

const SCAN_F2: u16 = 0x0C;
const SCAN_ESC: u16 = 0x17;

/// Log lines kept for F2.
const KEEP: usize = 400;

struct Ui {
    st: *mut SystemTable,
    splitter: *mut SimpleTextOutputProtocol,
    ours: *mut SimpleTextOutputProtocol,
    screens: Vec<*mut SimpleTextOutputProtocol>,
    serials: Vec<*mut SimpleTextOutputProtocol>,
    installed: bool,
    cols: usize,
    rows: usize,
    steps: [Line; 8],
    title: String,
    machine: String,
    name: Option<String>,
    synced: bool,
    problem: Option<(String, bool)>,
    next: Option<String>,
    progress: Option<(u64, u64)>,
    log: VecDeque<String>,
    partial: String,
    details: bool,
    keys: VecDeque<InputKey>,
    /// Keys are ours to read until a bootloader runs under the panel.
    keys_ours: bool,
    t0: u64,
    ticks_per_ms: u64,
    drawn_at: u64,
}

static mut UI: Option<Ui> = None;
/// Set while `out_string` runs, so a line printed from a timer (snpwatch's
/// hang watch, #88) during another goes to the serial consoles only.
static mut BUSY: bool = false;

#[allow(static_mut_refs)]
fn ui() -> Option<&'static mut Ui> {
    unsafe { UI.as_mut() }
}

fn bs(st: *mut SystemTable) -> &'static BootServices {
    unsafe { &*(*st).boot_services }
}

fn ucs2(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s
        .chars()
        .map(|c| match c as u32 {
            0x20..=0x7e => c as u16,
            _ => b'?' as u16,
        })
        .collect();
    v.push(0);
    v
}

/// Whether a text console is a screen: GraphicsConsole sits on the GOP
/// handle. Everything else (terminals, a BMC's redirection) is serial.
fn is_screen(h: uefi_raw::Handle) -> bool {
    crate::net::handle_protocol(h, &GraphicsOutputProtocol::GUID).is_some()
}

/// Take over the screen, if there is one and the media does not say
/// `ui = log`. Before this, and without a screen, output is as it always was.
pub fn begin() {
    if ui().is_some() || !crate::config::panel_wanted() {
        return;
    }
    let Some(st) = uefi::table::system_table_raw() else { return };
    let st = st.as_ptr();
    let (splitter, out_handle, err_handle) = unsafe { ((*st).stdout, (*st).stdout_handle, (*st).stderr_handle) };
    if splitter.is_null() {
        return;
    }
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&SimpleTextOutputProtocol::GUID))
    else {
        return;
    };
    let mut screens = Vec::new();
    let mut serials = Vec::new();
    for h in handles.iter() {
        let raw = h.as_ptr();
        if raw == out_handle || raw == err_handle {
            continue;
        }
        let Some(p) = crate::net::handle_protocol(raw, &SimpleTextOutputProtocol::GUID) else { continue };
        let p = p as *mut SimpleTextOutputProtocol;
        if p == splitter {
            continue;
        }
        if is_screen(raw) { screens.push(p) } else { serials.push(p) }
    }
    if screens.is_empty() {
        return;
    }
    let ours = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(SimpleTextOutputProtocol {
        reset: hook_reset,
        output_string: out_string,
        test_string: hook_test_string,
        query_mode: hook_query_mode,
        set_mode: hook_set_mode,
        set_attribute: hook_set_attribute,
        clear_screen: hook_clear_screen,
        set_cursor_position: hook_set_cursor,
        enable_cursor: hook_enable_cursor,
        mode: unsafe { (*splitter).mode },
    }));
    let t0 = crate::entropy::ticks();
    boot::stall(core::time::Duration::from_millis(10));
    let ticks_per_ms = ((crate::entropy::ticks() - t0) / 10).max(1);

    let title = match option_env!("STORMBOOTX_BUILD") {
        Some(b) => format!("stormbootx {} ({b})", env!("CARGO_PKG_VERSION")),
        None => format!("stormbootx {}", env!("CARGO_PKG_VERSION")),
    };
    let line = || Line { state: State::Pending, detail: String::new(), start: 0, end: 0 };
    let mut u = Ui {
        st,
        splitter,
        ours,
        screens,
        serials,
        installed: false,
        cols: 80,
        rows: 25,
        steps: [line(), line(), line(), line(), line(), line(), line(), line()],
        title,
        machine: String::new(),
        name: None,
        synced: false,
        problem: None,
        next: None,
        progress: None,
        log: VecDeque::new(),
        partial: String::new(),
        details: false,
        keys: VecDeque::new(),
        keys_ours: true,
        t0,
        ticks_per_ms,
        drawn_at: 0,
    };
    let vendor = unsafe { (*st).firmware_vendor };
    if !vendor.is_null() {
        let mut s = String::new();
        let mut p = vendor;
        unsafe {
            while *p != 0 && s.len() < 40 {
                s.push(char::from_u32(*p as u32).unwrap_or('?'));
                p = p.add(1);
            }
        }
        u.steps[0].detail = format!("{s} firmware");
    }
    u.steps[0].state = State::Done;
    unsafe { UI = Some(u) };
    install();
}

/// Put our ConOut in the system table and draw.
fn install() {
    let Some(u) = ui() else { return };
    if u.installed {
        return;
    }
    unsafe {
        (*u.st).stdout = u.ours;
    }
    crc(u.st);
    u.installed = true;
    measure(u);
    for &s in &u.screens {
        unsafe {
            ((*s).set_attribute)(s, NORMAL);
            ((*s).clear_screen)(s);
            ((*s).enable_cursor)(s, Boolean::FALSE);
        }
    }
    draw(u);
}

/// The system table's CRC, after its `ConOut` changed.
fn crc(st: *mut SystemTable) {
    unsafe {
        (*st).header.crc32 = 0;
        let mut c = 0u32;
        let size = (*st).header.size as usize;
        if (bs(st).calculate_crc32)(st as *mut c_void, size, &mut c) == Status::SUCCESS {
            (*st).header.crc32 = c;
        }
    }
}

/// Give `ConOut` back to the firmware, with a clear screen and the cursor
/// on: before anything else gets the screen. The panel can come back with
/// `resume`.
pub fn suspend() {
    let Some(u) = ui() else { return };
    if !u.installed {
        return;
    }
    unsafe {
        (*u.st).stdout = u.splitter;
    }
    crc(u.st);
    u.installed = false;
    unsafe {
        ((*u.splitter).set_attribute)(u.splitter, NORMAL);
        ((*u.splitter).clear_screen)(u.splitter);
        ((*u.splitter).enable_cursor)(u.splitter, Boolean::TRUE);
    }
}

/// The panel again, after `suspend`.
pub fn resume() {
    install();
}

/// For good: `suspend`, and nothing more is drawn.
pub fn end() {
    suspend();
    if let Some(u) = ui() {
        u.screens.clear();
    }
}

fn now(u: &Ui) -> u64 {
    (crate::entropy::ticks() - u.t0) / u.ticks_per_ms
}

fn measure(u: &mut Ui) {
    let s = u.screens[0];
    unsafe {
        let mode = (*s).mode;
        let m = if mode.is_null() { 0 } else { (*mode).mode.max(0) as usize };
        let (mut c, mut r) = (0usize, 0usize);
        if ((*s).query_mode)(s, m, &mut c, &mut r) == Status::SUCCESS && c >= 40 && r >= 16 {
            u.cols = c;
            u.rows = r;
        }
    }
}

/// `text` at `col,row` in `attr` on every screen, cut or padded to `width`.
fn put(u: &Ui, col: usize, row: usize, attr: usize, text: &str, width: usize) {
    if row >= u.rows || col >= u.cols {
        return;
    }
    // The bottom-right cell scrolls the screen on some consoles.
    let max = if row + 1 == u.rows { u.cols - col - 1 } else { u.cols - col };
    let width = width.min(max);
    let mut s: String = text.chars().take(width).collect();
    while s.chars().count() < width {
        s.push(' ');
    }
    let w = ucs2(&s);
    for &d in &u.screens {
        unsafe {
            ((*d).set_attribute)(d, attr);
            ((*d).set_cursor_position)(d, col, row);
            ((*d).output_string)(d, w.as_ptr());
        }
    }
}

fn secs(ms: u64) -> String {
    format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
}

fn clock_text(u: &Ui) -> String {
    match uefi::runtime::get_time() {
        Ok(t) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}{}",
            t.year(),
            t.month(),
            t.day(),
            t.hour(),
            t.minute(),
            t.second(),
            if u.synced { " UTC (NTP)" } else { " (RTC)" }
        ),
        Err(_) => String::new(),
    }
}

const STEP_ROW: usize = 3;
const PROGRESS_ROW: usize = STEP_ROW + 9;
const PROBLEM_ROW: usize = PROGRESS_ROW + 2;

fn draw(u: &mut Ui) {
    if u.screens.is_empty() || !u.installed {
        return;
    }
    u.drawn_at = now(u);
    if u.details {
        draw_details(u);
        return;
    }
    draw_header(u);
    put(u, 0, 1, BRIGHT, &format!(" {}", u.machine), u.cols);
    put(u, 0, 2, NORMAL, "", u.cols);
    for i in 0..LABELS.len() {
        draw_step(u, i);
    }
    put(u, 0, STEP_ROW + LABELS.len(), NORMAL, "", u.cols);
    draw_progress(u);
    put(u, 0, PROGRESS_ROW + 1, NORMAL, "", u.cols);
    let mut row = PROBLEM_ROW;
    if let Some((p, failed)) = &u.problem {
        let attr = if *failed { RED } else { YELLOW };
        for chunk in wrap(p, u.cols - 4, 3) {
            put(u, 0, row, attr, &format!("  ! {chunk}"), u.cols);
            row += 1;
        }
    }
    if let Some(n) = &u.next {
        put(u, 0, row, BRIGHT, &format!("  > {n}"), u.cols);
        row += 1;
    }
    while row + 1 < u.rows {
        put(u, 0, row, NORMAL, "", u.cols);
        row += 1;
    }
    draw_footer(u);
}

fn draw_header(u: &Ui) {
    let right = clock_text(u);
    let mut left = format!(" {}", u.title);
    if let Some(n) = &u.name {
        left.push_str(&format!("  -  {n}"));
    }
    let pad = u.cols.saturating_sub(left.len() + right.len() + 1);
    put(u, 0, 0, BAR, &format!("{left}{}{right} ", " ".repeat(pad)), u.cols);
}

fn draw_footer(u: &Ui) {
    let text = if u.details {
        " F2 or Esc: back to the boot steps"
    } else {
        " F2: every log line  -  the serial console carries the full log"
    };
    put(u, 0, u.rows - 1, DIM, text, u.cols);
}

fn draw_step(u: &Ui, i: usize) {
    let l = &u.steps[i];
    let (mark, attr) = match l.state {
        State::Pending => ("[    ]", DIM),
        State::Active => ("[ .. ]", YELLOW),
        State::Done => ("[ OK ]", GREEN),
        State::Failed => ("[FAIL]", RED),
        State::Skipped => ("[ -- ]", DIM),
    };
    let text_attr = match l.state {
        State::Pending | State::Skipped => DIM,
        State::Active => YELLOW,
        State::Failed => RED,
        State::Done => NORMAL,
    };
    let time = match l.state {
        State::Active => secs(now(u).saturating_sub(l.start)),
        State::Done | State::Failed if l.end >= l.start && l.start > 0 => secs(l.end - l.start),
        _ => String::new(),
    };
    let row = STEP_ROW + i;
    let detail_w = u.cols.saturating_sub(2 + 6 + 2 + 18 + 9);
    put(u, 0, row, attr, &format!("  {mark}"), 8);
    put(
        u,
        8,
        row,
        text_attr,
        &format!("  {:<18}{:<w$}{:>8} ", LABELS[i], l.detail, time, w = detail_w),
        u.cols - 8,
    );
}

fn draw_progress(u: &Ui) {
    let Some((bytes, rate)) = u.progress else {
        put(u, 0, PROGRESS_ROW, NORMAL, "", u.cols);
        return;
    };
    // How much the bootloader will read is not known here, so the bar shows
    // that reads are moving: a block that travels 16 MiB a cell.
    let width = 30usize;
    let pos = ((bytes >> 24) as usize) % width;
    let bar: String = (0..width).map(|i| if i <= pos { '#' } else { '.' }).collect();
    put(
        u,
        0,
        PROGRESS_ROW,
        CYAN,
        &format!("            [{bar}]  {} MiB read  {rate} MiB/s", bytes >> 20),
        u.cols,
    );
}

fn draw_details(u: &Ui) {
    put(u, 0, 0, BAR, &format!(" {}  -  every log line (the serial console has them all)", u.title), u.cols);
    let n = u.rows - 2;
    let skip = u.log.len().saturating_sub(n);
    let mut row = 1;
    for l in u.log.iter().skip(skip) {
        put(u, 0, row, NORMAL, l, u.cols);
        row += 1;
    }
    while row < u.rows - 1 {
        put(u, 0, row, NORMAL, "", u.cols);
        row += 1;
    }
    draw_footer(u);
}

/// `s` in lines of at most `w`, at most `n` of them.
fn wrap(s: &str, w: usize, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && cur.len() + 1 + word.len() > w {
            out.push(core::mem::take(&mut cur));
            if out.len() == n {
                return out;
            }
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() && out.len() < n {
        out.push(cur);
    }
    out
}

// ------------------------------------------------------------------ steps

/// `step` has started.
pub fn step(step: Step, detail: &str) {
    let Some(u) = ui() else { return };
    let t = now(u);
    let l = &mut u.steps[step as usize];
    l.state = State::Active;
    l.detail = String::from(detail);
    l.start = t.max(1);
    l.end = 0;
    poll_keys(u);
    draw(u);
}

/// `step` is done; `detail` says with what.
pub fn done(step: Step, detail: &str) {
    let Some(u) = ui() else { return };
    let t = now(u);
    let l = &mut u.steps[step as usize];
    if l.state != State::Active {
        l.start = t.max(1);
    }
    l.state = State::Done;
    l.detail = String::from(detail);
    l.end = t.max(1);
    poll_keys(u);
    draw(u);
}

/// `done`, unless `step` is done already: the first NIC to lease is DHCP's.
pub fn done_once(step: Step, detail: &str) {
    if ui().is_some_and(|u| u.steps[step as usize].state != State::Done) {
        done(step, detail);
    }
}

/// A new detail for `step`, its state and time unchanged.
pub fn detail(step: Step, detail: &str) {
    let Some(u) = ui() else { return };
    u.steps[step as usize].detail = String::from(detail);
    draw_step(u, step as usize);
}

/// `step` did not happen, and needn't have.
pub fn skip(step: Step, detail: &str) {
    let Some(u) = ui() else { return };
    let l = &mut u.steps[step as usize];
    l.state = State::Skipped;
    l.detail = String::from(detail);
    draw(u);
}

/// `step` failed: red, with the reason in its line.
pub fn fail(step: Step, reason: &str) {
    let Some(u) = ui() else { return };
    let t = now(u);
    let l = &mut u.steps[step as usize];
    if l.state != State::Active {
        l.start = t.max(1);
    }
    l.state = State::Failed;
    l.detail = String::from(reason);
    l.end = t.max(1);
    draw(u);
}

/// Why the boot is not going on as planned. Red, and the active step with
/// it, when a step was under way; otherwise said plainly (a `local` intent
/// is not a failure).
pub fn problem(reason: &str) {
    let Some(u) = ui() else { return };
    let t = now(u);
    let active = (0..u.steps.len()).rev().find(|&i| u.steps[i].state == State::Active);
    if let Some(i) = active {
        let l = &mut u.steps[i];
        l.state = State::Failed;
        l.end = t.max(1);
    }
    u.problem = Some((String::from(reason), active.is_some()));
    draw(u);
}

/// What happens next, under the problem: a countdown, the fall-through.
pub fn next(text: &str) {
    let Some(u) = ui() else { return };
    u.next = Some(String::from(text));
    poll_keys(u);
    draw(u);
}

/// The machine, for the header's second line.
pub fn machine(text: &str) {
    let Some(u) = ui() else { return };
    u.machine = String::from(text);
    draw(u);
}

/// The name the machine booted as, beside the version.
pub fn name(text: &str) {
    let Some(u) = ui() else { return };
    u.name = Some(String::from(text));
    draw(u);
}

/// The RTC was set from NTP (#77): the header says so.
pub fn synced() {
    let Some(u) = ui() else { return };
    u.synced = true;
    draw(u);
}

/// Bytes read from the attached image in all, and the rate.
pub fn progress(bytes: u64, mib_s: u64) {
    let Some(u) = ui() else { return };
    u.progress = Some((bytes, mib_s));
    if now(u).saturating_sub(u.drawn_at) < 250 || u.details {
        return;
    }
    u.drawn_at = now(u);
    draw_progress(u);
    draw_step(u, Step::Image as usize);
    draw_header(u);
}

/// A bootloader runs under the panel from here: its keys are its own, and
/// its lines show in the handoff step.
pub fn handoff(detail: &str) {
    if let Some(u) = ui() {
        u.keys_ours = false;
    }
    step(Step::Handoff, detail);
}

/// The bootloader came back: keys are ours again.
pub fn handed_back(reason: &str) {
    if let Some(u) = ui() {
        u.keys_ours = true;
    }
    fail(Step::Handoff, reason);
}

// ------------------------------------------------------------------- keys

fn poll_keys(u: &mut Ui) {
    if !u.keys_ours || !u.installed {
        return;
    }
    let stdin = unsafe { (*u.st).stdin };
    if stdin.is_null() {
        return;
    }
    loop {
        let mut k = InputKey::default();
        if unsafe { ((*stdin).read_key_stroke)(stdin, &mut k) } != Status::SUCCESS {
            return;
        }
        match k.scan_code {
            SCAN_F2 => {
                u.details = !u.details;
                draw(u);
            }
            SCAN_ESC if u.details => {
                u.details = false;
                draw(u);
            }
            _ if u.keys.len() < 16 => u.keys.push_back(k),
            _ => {}
        }
    }
}

/// A printable key pressed while the panel is up, F2 and Esc being the
/// panel's own. `None` with no panel: read the console as before.
pub fn take_char() -> Option<Option<char>> {
    let u = ui()?;
    if !u.installed {
        return None;
    }
    poll_keys(u);
    while let Some(k) = u.keys.pop_front() {
        if let Some(c) = char::from_u32(u32::from(k.unicode_char)).filter(|&c| c != '\0') {
            return Some(Some(c));
        }
    }
    Some(None)
}

// ------------------------------------------------------- the ConOut hook

/// Every line anyone prints: kept for F2, written to the serial consoles.
unsafe extern "efiapi" fn out_string(_this: *mut SimpleTextOutputProtocol, s: *const Char16) -> Status {
    let Some(u) = ui() else { return Status::SUCCESS };
    for &d in &u.serials {
        unsafe { ((*d).output_string)(d, s) };
    }
    if s.is_null() {
        return Status::SUCCESS;
    }
    // A timer event at TPL_NOTIFY must not land mid-update; one that printed
    // while another line was being kept has gone to the serial consoles.
    let old = unsafe { (bs(u.st).raise_tpl)(uefi_raw::table::boot::Tpl::NOTIFY) };
    if unsafe { BUSY } {
        unsafe { (bs(u.st).restore_tpl)(old) };
        return Status::SUCCESS;
    }
    unsafe { BUSY = true };
    let mut p = s;
    let mut lines = false;
    unsafe {
        while *p != 0 {
            match *p {
                0x0a => {
                    let l = core::mem::take(&mut u.partial);
                    u.log.push_back(l);
                    if u.log.len() > KEEP {
                        u.log.pop_front();
                    }
                    lines = true;
                }
                0x0d => {}
                c => {
                    if u.partial.len() < 512 {
                        u.partial.push(char::from_u32(c as u32).unwrap_or('?'));
                    }
                }
            }
            p = p.add(1);
        }
    }
    unsafe { (bs(u.st).restore_tpl)(old) };
    if lines && old <= uefi_raw::table::boot::Tpl::CALLBACK && u.installed {
        if u.details {
            draw_details(u);
        } else if !u.keys_ours && u.steps[Step::Handoff as usize].state == State::Active {
            // The bootloader's lines, one at a time, in its step.
            if let Some(last) = u.log.iter().rev().find(|l| !l.trim().is_empty()) {
                u.steps[Step::Handoff as usize].detail = String::from(last.trim());
            }
            draw_step(u, Step::Handoff as usize);
        } else if now(u).saturating_sub(u.drawn_at) >= 500 {
            // The active step's time moves on.
            u.drawn_at = now(u);
            for i in 0..LABELS.len() {
                if u.steps[i].state == State::Active {
                    draw_step(u, i);
                }
            }
            draw_header(u);
        }
        poll_keys(u);
    }
    unsafe { BUSY = false };
    Status::SUCCESS
}

/// Everything else another program asks of `ConOut` goes to the serial
/// consoles: the screen is the panel's until `end`.
unsafe extern "efiapi" fn hook_reset(_this: *mut SimpleTextOutputProtocol, ext: Boolean) -> Status {
    each_serial(|d| unsafe { ((*d).reset)(d, ext) });
    Status::SUCCESS
}

unsafe extern "efiapi" fn hook_test_string(_this: *mut SimpleTextOutputProtocol, s: *const Char16) -> Status {
    match ui() {
        Some(u) => unsafe { ((*u.splitter).test_string)(u.splitter, s) },
        None => Status::SUCCESS,
    }
}

unsafe extern "efiapi" fn hook_query_mode(
    _this: *mut SimpleTextOutputProtocol,
    mode: usize,
    cols: *mut usize,
    rows: *mut usize,
) -> Status {
    match ui() {
        Some(u) => unsafe { ((*u.splitter).query_mode)(u.splitter, mode, cols, rows) },
        None => Status::UNSUPPORTED,
    }
}

unsafe extern "efiapi" fn hook_set_mode(_this: *mut SimpleTextOutputProtocol, mode: usize) -> Status {
    let Some(u) = ui() else { return Status::UNSUPPORTED };
    let st = unsafe { ((*u.splitter).set_mode)(u.splitter, mode) };
    if st == Status::SUCCESS && !u.screens.is_empty() {
        measure(u);
        for &s in &u.screens {
            unsafe {
                ((*s).clear_screen)(s);
                ((*s).enable_cursor)(s, Boolean::FALSE);
            }
        }
        draw(u);
    }
    st
}

unsafe extern "efiapi" fn hook_set_attribute(_this: *mut SimpleTextOutputProtocol, attr: usize) -> Status {
    each_serial(|d| unsafe { ((*d).set_attribute)(d, attr) });
    Status::SUCCESS
}

unsafe extern "efiapi" fn hook_clear_screen(_this: *mut SimpleTextOutputProtocol) -> Status {
    each_serial(|d| unsafe { ((*d).clear_screen)(d) });
    Status::SUCCESS
}

unsafe extern "efiapi" fn hook_set_cursor(_this: *mut SimpleTextOutputProtocol, col: usize, row: usize) -> Status {
    each_serial(|d| unsafe { ((*d).set_cursor_position)(d, col, row) });
    Status::SUCCESS
}

unsafe extern "efiapi" fn hook_enable_cursor(_this: *mut SimpleTextOutputProtocol, on: Boolean) -> Status {
    each_serial(|d| unsafe { ((*d).enable_cursor)(d, on) });
    Status::SUCCESS
}

fn each_serial(f: impl Fn(*mut SimpleTextOutputProtocol) -> Status) {
    if let Some(u) = ui() {
        for &d in &u.serials {
            let _ = f(d);
        }
    }
}

/// Whether the panel is up (tests and the shell ask).
pub fn active() -> bool {
    ui().is_some_and(|u| u.installed)
}
