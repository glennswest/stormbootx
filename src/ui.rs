//! The boot screen (#144, #220): one panel, redrawn in place, instead of a
//! scrolling log, in the look of the boot screen it hands the display to.
//!
//! Owner, 2026-10-14: "can we do a better ui for stormbootx. It looks messy
//! at times"; 2026-11-15 (#220): "I want a enterprise boot screen (As in
//! trek)": the machine looks like one product from power-on to login. So the
//! panel is drawn on the screen's pixels with **stormblock-screen-spec**
//! (stormblock#637), the crate stormblock's boot screen (#618) draws with:
//! its palette, its typeface (Antonio Bold), its 1920 × 1080 grid and its
//! frame. Nothing about the look is defined here; a colour or a size that
//! is not the spec's is a drift.
//!
//! The panel: the header frame says the machine's name, its lease, the
//! machine and this build with the clock, and, large on the right, the step
//! at work (or `STOPPED`); the lower frame lists the boot's steps with a
//! state pill, what each did and its time, the image read's bar, and when
//! something goes wrong the reason and what happens next. Every log line
//! still goes to the serial console exactly as before (the consoles
//! stormcentral keeps, and every OVMF test, read it), and to a log view
//! behind F2. Nothing is mixed into the panel.
//!
//! **How.** `begin` puts its own `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL` in the
//! system table's `ConOut` (the firmware's ConSplitter, restored by `end`),
//! so every line printed by anyone (this binary, the media's NIC drivers, the
//! bootloader it starts) lands in `out_string`: kept for F2 and written to
//! the serial consoles only. A console whose handle carries GOP is a screen
//! (GraphicsConsole); every other one is serial (a terminal on a UART, a
//! BMC's redirection). Each screen gets a back buffer in RAM at its GOP's
//! mode; the panel is drawn there (`Xrgb8888`, the Blt buffer's BGRx) and
//! the changed part is Blt'd to the GOP, so a BLT-only mode works and the
//! framebuffer is never read. A machine with no screen console, or `ui =
//! log` in `stormboot.conf`, gets today's output and no panel.
//!
//! **The hand-over.** As the attached image's bootloader starts (`handoff`)
//! the frame is redrawn as the boot screen's first frame
//! (`Frame::default()`, `BOOT` or `INSTALL`), and left up: stormuefi prints
//! to `ConOut` (the serial consoles and the handoff row), and stormblock's
//! screen draws its first frame before it takes the display, in the same
//! geometry, so there is no flash and no black gap. `end` hands `ConOut`
//! back with a cleared screen before anything is left to the firmware or an
//! interactive program (the network console, a local disk's bootloader, a
//! return to the boot manager or the EFI Shell).

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;

use stormblock_screen_spec::font::{self, size};
use stormblock_screen_spec::frame::{self, Frame};
use stormblock_screen_spec::palette::{self, Rgb};
use stormblock_screen_spec::{Align, Grid, PixelFormat, Surface};
use uefi::boot::{self, SearchType};
use uefi_raw::protocol::console::{
    GraphicsOutputBltOperation, GraphicsOutputBltPixel, GraphicsOutputProtocol, InputKey, SimpleTextOutputProtocol,
};
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
    "FIRMWARE HANDOFF",
    "NIC UP",
    "DHCP",
    "FORGE REACHED",
    "BOOT INTENT",
    "CLAIM",
    "IMAGE LOADED",
    "HANDOFF",
];

/// The big readout while a step is at work.
const BIG: [&str; 8] = ["FIRMWARE", "NIC", "DHCP", "FORGE", "INTENT", "CLAIM", "IMAGE", "HANDOFF"];

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

const SCAN_F2: u16 = 0x0C;
const SCAN_ESC: u16 = 0x17;

const DEVICE_PATH: uefi_raw::Guid = uefi_raw::guid!("09576e91-6d3f-11d2-8e39-00a0c969723b");

/// Log lines kept for F2.
const KEEP: usize = 400;

// ------------------------------------------------------------ the layout
//
// Design pixels on the spec's 1920 × 1080 grid. The frame is the spec's
// (`grid::frame`); these are the places inside it, as the boot screen uses
// them (stormblock src/screen/draw.rs): x 260 onwards, right of the frames'
// bars and clear of their inner corners.

/// (x, y, w, h).
type Area = (f32, f32, f32, f32);

/// The header frame's content: name, lease, machine, build; the readout.
const HEAD: Area = (256.0, 70.0, 1638.0, 274.0);
/// The lower frame's content, between its bar on the left and at the bottom.
const LOWER: Area = (256.0, 352.0, 1638.0, 664.0);
/// One step's row.
const ROW_TOP: f32 = 396.0;
const ROW_H: f32 = 56.0;
fn row_area(i: usize) -> Area {
    (256.0, ROW_TOP + ROW_H * i as f32, 1638.0, ROW_H)
}
/// The image read's bar.
const PROGRESS: Area = (256.0, 852.0, 1638.0, 50.0);
/// The columns: step, state pill, detail, time (right-aligned).
const COL_STEP: f32 = 260.0;
const COL_STATE: f32 = 560.0;
const PILL_W: f32 = 150.0;
const COL_DETAIL: f32 = 740.0;
const COL_RIGHT: f32 = 1880.0;
/// The lower frame's side blocks while stormbootx has the screen; at the
/// hand-over they are the boot screen's (`Frame::default()`).
const BLOCKS: [&str; 3] = ["NETWORK", "STEPS", "IMAGE"];
const KEYS: &str = "F2  LOG";
const KEYS_BACK: &str = "F2  ESC  BACK";
/// The log lines F2 shows, and their spacing.
const LOG_LINES: usize = 20;

/// One screen: its text console (for the cursor), its GOP, and the panel as
/// last drawn at the GOP's mode.
struct Screen {
    out: *mut SimpleTextOutputProtocol,
    gop: *mut GraphicsOutputProtocol,
    w: u32,
    h: u32,
    buf: Vec<u32>,
}

impl Screen {
    fn open(out: *mut SimpleTextOutputProtocol, handle: uefi_raw::Handle) -> Option<Screen> {
        let gop = crate::net::handle_protocol(handle, &GraphicsOutputProtocol::GUID)? as *mut GraphicsOutputProtocol;
        let mut s = Screen { out, gop, w: 0, h: 0, buf: Vec::new() };
        s.fit().then_some(s)
    }

    /// The GOP's mode now, and the buffer sized to it; false with no mode a
    /// panel fits on.
    fn fit(&mut self) -> bool {
        let (w, h) = unsafe {
            let m = (*self.gop).mode;
            if m.is_null() || (*m).info.is_null() {
                return false;
            }
            ((*(*m).info).horizontal_resolution, (*(*m).info).vertical_resolution)
        };
        if w < 320 || h < 200 {
            return false;
        }
        if (w, h) != (self.w, self.h) {
            self.w = w;
            self.h = h;
            self.buf = alloc::vec![0u32; w as usize * h as usize];
        }
        true
    }

    fn grid(&self) -> Grid {
        Grid::new(self.w, self.h)
    }

    fn surface(&mut self) -> Surface<'_> {
        Surface::new(&mut self.buf, self.w, self.h, self.w, PixelFormat::Xrgb8888)
    }

    /// A design area in display pixels: (x0, y0, x1, y1).
    fn rect(&self, a: Area) -> (i64, i64, i64, i64) {
        let g = self.grid();
        (g.x(a.0).max(0), g.y(a.1).max(0), g.x(a.0 + a.2).min(self.w as i64), g.y(a.1 + a.3).min(self.h as i64))
    }

    /// Show part of the buffer.
    fn blit(&mut self, (x0, y0, x1, y1): (i64, i64, i64, i64)) {
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        unsafe {
            let _ = ((*self.gop).blt)(
                self.gop,
                self.buf.as_mut_ptr() as *mut GraphicsOutputBltPixel,
                GraphicsOutputBltOperation::BLT_BUFFER_TO_VIDEO,
                x0 as usize,
                y0 as usize,
                x0 as usize,
                y0 as usize,
                (x1 - x0) as usize,
                (y1 - y0) as usize,
                self.w as usize * 4,
            );
        }
    }
}

/// A part of the panel pre-drawn for a moment nothing may allocate in
/// (#219's `BEFORE_EXIT_BOOT_SERVICES`): Blt `px` (`w` × `h`, BGRx) to the
/// GOP at (`x`, `y`).
pub struct Mark {
    pub gop: *mut GraphicsOutputProtocol,
    pub px: Vec<u32>,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Mark {
    /// Boot services only: no allocation.
    ///
    /// # Safety
    /// `gop` is still the GOP it was made from.
    pub unsafe fn show(&self) {
        unsafe {
            let _ = ((*self.gop).blt)(
                self.gop,
                self.px.as_ptr() as *mut GraphicsOutputBltPixel,
                GraphicsOutputBltOperation::BLT_BUFFER_TO_VIDEO,
                0,
                0,
                self.x,
                self.y,
                self.w,
                self.h,
                self.w * 4,
            );
        }
    }
}

struct Ui {
    st: *mut SystemTable,
    splitter: *mut SimpleTextOutputProtocol,
    out_handle: uefi_raw::Handle,
    err_handle: uefi_raw::Handle,
    ours: *mut SimpleTextOutputProtocol,
    /// The screens' text consoles, as found (`rescan` compares them).
    outs: Vec<*mut SimpleTextOutputProtocol>,
    screens: Vec<Screen>,
    serials: Vec<*mut SimpleTextOutputProtocol>,
    installed: bool,
    /// `end` was called: nothing is drawn again.
    ended: bool,
    steps: [Line; 8],
    title: String,
    machine: String,
    name: Option<String>,
    synced: bool,
    problem: Option<(String, bool)>,
    next: Option<String>,
    progress: Option<(u64, u64)>,
    /// The boot intent is `install`: `STORMCOS  INSTALL`.
    install: bool,
    /// The bootloader is running: the frame is the boot screen's first.
    last: bool,
    /// What the frame on the screens was drawn for: (failed, details, last,
    /// install). A change redraws it all.
    framed: Option<(bool, bool, bool, bool)>,
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

/// Whether a text console is a screen: GraphicsConsole sits on the GOP
/// handle. Everything else (terminals, a BMC's redirection) is serial.
fn is_screen(h: uefi_raw::Handle) -> bool {
    crate::net::handle_protocol(h, &GraphicsOutputProtocol::GUID).is_some()
}

type Consoles = (Vec<(*mut SimpleTextOutputProtocol, uefi_raw::Handle)>, Vec<*mut SimpleTextOutputProtocol>);

/// The text consoles behind the splitter: (screens with their handles,
/// serials).
fn consoles(
    splitter: *mut SimpleTextOutputProtocol,
    out_handle: uefi_raw::Handle,
    err_handle: uefi_raw::Handle,
) -> Option<Consoles> {
    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&SimpleTextOutputProtocol::GUID)).ok()?;
    let mut screens = Vec::new();
    let mut serials = Vec::new();
    for h in handles.iter() {
        let raw = h.as_ptr();
        // ConSplitter's own virtual consoles have no device path, and its
        // ConOut carries a GOP of its own: drawing on it reaches every
        // console, the serial one too. Under the EFI Shell the system table's
        // ConOut is the shell's, so the splitter's is not `out_handle`.
        if raw == out_handle || raw == err_handle || crate::net::handle_protocol(raw, &DEVICE_PATH).is_none() {
            continue;
        }
        let Some(p) = crate::net::handle_protocol(raw, &SimpleTextOutputProtocol::GUID) else { continue };
        let p = p as *mut SimpleTextOutputProtocol;
        if p == splitter {
            continue;
        }
        if is_screen(raw) { screens.push((p, raw)) } else { serials.push(p) }
    }
    Some((screens, serials))
}

/// The screens of `found` a panel can be drawn on.
fn open_screens(found: &[(*mut SimpleTextOutputProtocol, uefi_raw::Handle)]) -> Vec<Screen> {
    found.iter().filter_map(|&(out, h)| Screen::open(out, h)).collect()
}

/// Read the consoles again (#219). `begin` runs before any
/// `ConnectController`, and a connect over every handle can stop a terminal
/// and start a new one on the same UART (forge1: the serial log ended at
/// `binding the platform's own drivers first` while the screen went on).
/// The pointers kept from before would then write nowhere, or into a freed
/// instance. Called after every connect over all handles, after the NIC
/// take-over and at the handoff; says so when the set changed.
pub fn rescan() {
    let Some(u) = ui() else { return };
    let Some((found, serials)) = consoles(u.splitter, u.out_handle, u.err_handle) else { return };
    let outs: Vec<_> = found.iter().map(|s| s.0).collect();
    if outs == u.outs && serials == u.serials {
        return;
    }
    let was = (u.outs.len(), u.serials.len());
    u.serials = serials;
    let mut gone = false;
    // `end` emptied the screens for good; that stays.
    if !u.ended {
        u.outs = outs;
        u.screens = open_screens(&found);
        if u.screens.is_empty() {
            gone = true;
        } else if u.installed {
            u.framed = None;
            for sc in &u.screens {
                unsafe {
                    let _ = ((*sc.out).enable_cursor)(sc.out, Boolean::FALSE);
                }
            }
            draw(u);
        }
    }
    let now = (u.outs.len(), u.serials.len());
    if gone {
        // The panel has nowhere to go: `ConOut` back to the splitter.
        suspend();
    }
    uefi::println!(
        "console     : the consoles changed under a connect: {} screen(s), {} serial (were {}, {}); output follows the new ones",
        now.0,
        now.1,
        was.0,
        was.1
    );
}

/// What the handoff's ExitBootServices breadcrumb writes to (#219), when the
/// panel holds `ConOut`: the serial consoles, and the handoff step's row on
/// each screen drawn now with `detail`, to be Blt'd then. `None` without the
/// panel: write to `ConOut` itself.
pub fn raw_targets(detail: &str) -> Option<(Vec<*mut SimpleTextOutputProtocol>, Vec<Mark>)> {
    let u = ui()?;
    if !u.installed {
        return None;
    }
    let serials = u.serials.clone();
    if u.details || u.ended {
        return Some((serials, Vec::new()));
    }
    let i = Step::Handoff as usize;
    let saved = (u.steps[i].state, core::mem::replace(&mut u.steps[i].detail, String::from(detail)));
    u.steps[i].state = State::Active;
    let mut marks = Vec::new();
    let mut screens = core::mem::take(&mut u.screens);
    for sc in screens.iter_mut() {
        let g = sc.grid();
        step_row(&mut sc.surface(), &g, u, i);
        let (x0, y0, x1, y1) = sc.rect(row_area(i));
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let mut px = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize);
        for y in y0..y1 {
            let at = y as usize * sc.w as usize;
            px.extend_from_slice(&sc.buf[at + x0 as usize..at + x1 as usize]);
        }
        marks.push(Mark { gop: sc.gop, px, x: x0 as usize, y: y0 as usize, w: (x1 - x0) as usize, h: (y1 - y0) as usize });
    }
    // The row as it is now, in the buffers (the screens never saw the mark).
    u.steps[i].state = saved.0;
    u.steps[i].detail = saved.1;
    for sc in screens.iter_mut() {
        let g = sc.grid();
        step_row(&mut sc.surface(), &g, u, i);
    }
    u.screens = screens;
    Some((serials, marks))
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
    let Some((found, serials)) = consoles(splitter, out_handle, err_handle) else { return };
    let screens = open_screens(&found);
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

    let title = build();
    let line = || Line { state: State::Pending, detail: String::new(), start: 0, end: 0 };
    let mut u = Ui {
        st,
        splitter,
        out_handle,
        err_handle,
        ours,
        outs: found.iter().map(|s| s.0).collect(),
        screens,
        serials,
        installed: false,
        ended: false,
        steps: [line(), line(), line(), line(), line(), line(), line(), line()],
        title,
        machine: String::new(),
        name: None,
        synced: false,
        problem: None,
        next: None,
        progress: None,
        install: false,
        last: false,
        framed: None,
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
        u.steps[Step::Firmware as usize].detail = format!("{s} firmware");
    }
    u.steps[Step::Firmware as usize].state = State::Done;
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
    u.screens.retain_mut(|s| s.fit());
    for sc in &u.screens {
        // Before the first frame: GraphicsConsole erases its cursor with a
        // Blt of its own.
        unsafe {
            let _ = ((*sc.out).enable_cursor)(sc.out, Boolean::FALSE);
        }
    }
    u.framed = None;
    draw(u);
}

/// The system table's CRC, after its `ConOut` changed.
fn crc(st: *mut SystemTable) {
    unsafe {
        (*st).header.crc = 0;
        let mut c = 0u32;
        let size = (*st).header.size as usize;
        if (bs(st).calculate_crc32)(st as *mut c_void, size, &mut c) == Status::SUCCESS {
            (*st).header.crc = c;
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
    u.framed = None;
    unsafe {
        let _ = ((*u.splitter).set_attribute)(u.splitter, 0x07);
        let _ = ((*u.splitter).clear_screen)(u.splitter);
        let _ = ((*u.splitter).enable_cursor)(u.splitter, Boolean::TRUE);
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
        u.ended = true;
        u.screens.clear();
        u.outs.clear();
    }
}

/// This binary's version and commit.
fn build() -> String {
    match option_env!("STORMBOOTX_BUILD") {
        Some(b) => format!("stormbootx {} ({b})", env!("CARGO_PKG_VERSION")),
        None => format!("stormbootx {}", env!("CARGO_PKG_VERSION")),
    }
}

/// `end`, then one line naming this build on the screen it hands over (#180).
/// `end` clears the panel and its header, and a log's banner has scrolled off
/// by now, so what is on the screen afterwards would not say which stormbootx
/// ran: a machine can hold several (a stick, a BMC's old CD).
pub fn end_saying(what: &str) {
    end();
    uefi::println!("{}: {what}", build());
}

fn now(u: &Ui) -> u64 {
    (crate::entropy::ticks() - u.t0) / u.ticks_per_ms
}

fn secs(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} MS")
    } else {
        format!("{}.{} S", ms / 1000, (ms % 1000) / 100)
    }
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

// --------------------------------------------------------------- drawing

fn failed(u: &Ui) -> bool {
    u.steps.iter().any(|l| l.state == State::Failed)
}

fn active(u: &Ui) -> Option<usize> {
    (0..u.steps.len()).rev().find(|&i| u.steps[i].state == State::Active)
}

/// Draw on every screen with `f`, then show `areas` (all of it when empty).
fn paint(u: &mut Ui, areas: &[Area], f: impl Fn(&mut Surface, &Grid, &Ui)) {
    if !u.installed || u.ended || u.screens.is_empty() {
        return;
    }
    let mut screens = core::mem::take(&mut u.screens);
    for sc in screens.iter_mut() {
        let g = sc.grid();
        f(&mut sc.surface(), &g, u);
        if areas.is_empty() {
            let all = (0, 0, sc.w as i64, sc.h as i64);
            sc.blit(all);
        } else {
            for &a in areas {
                let r = sc.rect(a);
                sc.blit(r);
            }
        }
    }
    u.screens = screens;
}

fn clear(s: &mut Surface, g: &Grid, a: Area) {
    let (x0, y0) = (g.x(a.0), g.y(a.1));
    s.fill_rect(x0, y0, g.x(a.0 + a.2) - x0, g.y(a.1 + a.3) - y0, palette::BG);
}

/// Display width between two design columns.
fn span(g: &Grid, from: f32, to: f32) -> f32 {
    g.xf(to) - g.xf(from)
}

fn draw(u: &mut Ui) {
    if u.screens.is_empty() || !u.installed {
        return;
    }
    u.drawn_at = now(u);
    let key = (failed(u), u.details, u.last, u.install);
    if u.framed != Some(key) {
        u.framed = Some(key);
        paint(u, &[], |s, g, u| {
            s.clear(palette::BG);
            draw_frame(s, g, u);
            header(s, g, u);
            lower(s, g, u);
        });
    } else {
        paint(u, &[HEAD, LOWER], |s, g, u| {
            header(s, g, u);
            lower(s, g, u);
        });
    }
}

/// The spec's frame: ours while stormbootx has the screen, the boot
/// screen's first frame from the handoff on.
fn draw_frame(s: &mut Surface, g: &Grid, u: &Ui) {
    let kind = if u.install { "INSTALL" } else { "BOOT" };
    let title = format!("STORMCOS  {kind}");
    let release = format!("STORMBOOTX {}", env!("CARGO_PKG_VERSION"));
    let mut f = Frame { title: &title, kind, release: &release, failed: failed(u), ..Default::default() };
    if !u.last {
        f.blocks = BLOCKS;
        f.keys = if u.details { KEYS_BACK } else { KEYS };
    }
    frame::draw(s, g, &f);
}

/// The header: who and where on the left, the readout on the right.
fn header(s: &mut Surface, g: &Grid, u: &Ui) {
    clear(s, g, HEAD);
    let x = g.xf(COL_STEP);
    let left = span(g, COL_STEP, 1100.0);
    let name = u.name.as_deref().map(str::to_uppercase).unwrap_or_else(|| String::from("—"));
    s.text_fit(x, g.yf(150.0), g.p(size::NAME), palette::PEACH, Align::Left, &name, left);
    let lease = &u.steps[Step::Dhcp as usize];
    let addr = if lease.state == State::Done { lease.detail.to_uppercase() } else { String::from("NO ADDRESS YET") };
    s.text_fit(x, g.yf(202.0), g.p(size::ADDRESS), palette::BLUE, Align::Left, &addr, left);
    s.text_fit(x, g.yf(250.0), g.p(size::BODY), palette::GREY, Align::Left, &u.machine.to_uppercase(), left);
    let build = format!("{}  ·  {}", u.title.to_uppercase(), clock_text(u));
    s.text_fit(x, g.yf(296.0), g.p(size::PILL), palette::LAVENDER, Align::Left, &build, left);

    // The readout: the step at work, else how the boot stands.
    let right = g.xf(COL_RIGHT);
    let room = span(g, 1120.0, COL_RIGHT);
    let t = now(u);
    let (big, colour, sub, said): (&str, Rgb, String, String) = match active(u) {
        Some(i) => {
            let l = &u.steps[i];
            let sub = match (i == Step::Image as usize, u.progress) {
                (true, Some((bytes, rate))) => format!("{} MIB  ·  {rate} MIB/S", bytes >> 20),
                _ => secs(t.saturating_sub(l.start)),
            };
            (BIG[i], palette::RUNNING, sub, l.detail.clone())
        }
        None if failed(u) => ("STOPPED", palette::FAILED, format!("AFTER {}", secs(t)), u.next.clone().unwrap_or_default()),
        None if u.problem.is_some() => ("LOCAL", palette::WANTING, format!("AFTER {}", secs(t)), u.next.clone().unwrap_or_default()),
        None => {
            let i = (0..u.steps.len()).rev().find(|&i| u.steps[i].state == State::Done).unwrap_or(0);
            (BIG[i], palette::DONE, format!("IN {}", secs(t)), u.steps[i].detail.clone())
        }
    };
    s.text_fit(right, g.yf(206.0), g.p(size::BIG), colour, Align::Right, big, room);
    s.text_fit(right, g.yf(262.0), g.p(size::ETA), palette::BLUE, Align::Right, &sub, room);
    s.text_fit(right, g.yf(304.0), g.p(size::SMALL), palette::GREY, Align::Right, &said.to_uppercase(), room);
}

fn lower(s: &mut Surface, g: &Grid, u: &Ui) {
    if u.details {
        details(s, g, u);
    } else {
        steps(s, g, u);
    }
}

/// The steps, the read's bar, the problem and what next.
fn steps(s: &mut Surface, g: &Grid, u: &Ui) {
    clear(s, g, LOWER);
    let y = g.yf(384.0);
    let hp = g.p(size::HEADING);
    s.text(g.xf(COL_STEP), y, hp, palette::GREY, Align::Left, "STEP");
    s.text(g.xf(COL_STATE), y, hp, palette::GREY, Align::Left, "STATE");
    s.text(g.xf(COL_DETAIL), y, hp, palette::GREY, Align::Left, "DETAIL");
    s.text(g.xf(COL_RIGHT), y, hp, palette::GREY, Align::Right, "TIME");
    for i in 0..LABELS.len() {
        step_row(s, g, u, i);
    }
    progress_row(s, g, u);
    problem_rows(s, g, u);
}

/// One step: its name, a state pill, what it did, its time.
fn step_row(s: &mut Surface, g: &Grid, u: &Ui, i: usize) {
    clear(s, g, row_area(i));
    let l = &u.steps[i];
    let ry = ROW_TOP + ROW_H * i as f32;
    let base = g.yf(ry + 40.0);
    let (said, pill, ink, name, detail) = match l.state {
        State::Pending => ("PENDING", palette::PENDING, palette::GREY, palette::GREY, palette::GREY),
        State::Active => ("RUNNING", palette::RUNNING, palette::INK, palette::RUNNING, palette::RUNNING),
        State::Done => ("DONE", palette::DONE, palette::INK, palette::PEACH, palette::BLUE),
        State::Failed => ("FAILED", palette::FAILED, palette::INK, palette::FAILED, palette::FAILED),
        State::Skipped => ("SKIPPED", palette::NEUTRAL, palette::INK, palette::GREY, palette::GREY),
    };
    s.text_fit(g.xf(COL_STEP), base, g.p(size::ROW), name, Align::Left, LABELS[i], span(g, COL_STEP, COL_STATE - 16.0));
    let (py, ph) = (g.y(ry + 12.0), g.lh(36.0));
    s.pill(g.x(COL_STATE), py, g.lw(PILL_W), ph, pill);
    let pp = g.p(size::PILL);
    s.text(g.xf(COL_STATE + PILL_W / 2.0), py as f32 + (ph as f32 + pp * 0.72) / 2.0, pp, ink, Align::Center, said);
    let time = match l.state {
        State::Active => secs(now(u).saturating_sub(l.start)),
        State::Done | State::Failed if l.end >= l.start && l.start > 0 => secs(l.end - l.start),
        _ => String::new(),
    };
    let tw = s.text(g.xf(COL_RIGHT), base, g.p(size::LABEL), palette::LAVENDER, Align::Right, &time);
    let room = span(g, COL_DETAIL, COL_RIGHT) - tw - g.p(24.0);
    s.text_fit(g.xf(COL_DETAIL), base, g.p(size::LABEL), detail, Align::Left, &l.detail.to_uppercase(), room);
}

/// The image read: how much has moved, and how fast. How much the
/// bootloader will read is not known here, so the bar shows that reads are
/// moving: it fills 16 MiB a step and starts again.
fn progress_row(s: &mut Surface, g: &Grid, u: &Ui) {
    clear(s, g, PROGRESS);
    let Some((bytes, rate)) = u.progress else { return };
    let frac = (((bytes >> 24) % 32) + 1) as f32 / 32.0;
    s.bar(g.x(COL_STEP), g.y(864.0), g.lw(1000.0), g.lh(26.0), frac, palette::TRACK, palette::BLUE);
    s.text(
        g.xf(COL_RIGHT),
        g.yf(890.0),
        g.p(size::LABEL),
        palette::BLUE,
        Align::Right,
        &format!("{} MIB READ  ·  {rate} MIB/S", bytes >> 20),
    );
}

/// `s` in lines that fit `max` at `px`, at most `n` of them.
fn wrap(s: &str, px: f32, max: f32, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let next = if cur.is_empty() { String::from(word) } else { format!("{cur} {word}") };
        if !cur.is_empty() && font::text_width(&next, px) > max {
            out.push(core::mem::replace(&mut cur, String::from(word)));
            if out.len() == n {
                // More follows: the last line says so (`text_fit` cuts it
                // to fit, ellipsis and all).
                if let Some(last) = out.last_mut() {
                    last.push_str(" …");
                }
                return out;
            }
        } else {
            cur = next;
        }
    }
    if !cur.is_empty() && out.len() < n {
        out.push(cur);
    }
    out
}

/// Why the boot is not going on as planned (red when a step failed), and
/// what happens next.
fn problem_rows(s: &mut Surface, g: &Grid, u: &Ui) {
    let px = g.p(size::BODY);
    let room = span(g, COL_STEP, COL_RIGHT);
    let mut base = 934.0;
    if let Some((p, failed)) = &u.problem {
        let colour = if *failed { palette::FAILED } else { palette::WANTING };
        for line in wrap(&p.to_uppercase(), px, room, 2) {
            s.text_fit(g.xf(COL_STEP), g.yf(base), px, colour, Align::Left, &line, room);
            base += 36.0;
        }
    }
    if let Some(n) = &u.next {
        s.text_fit(g.xf(COL_STEP), g.yf(1006.0f32.min(base)), px, palette::RUNNING, Align::Left, &n.to_uppercase(), room);
    }
}

/// F2: the last log lines, as printed.
fn details(s: &mut Surface, g: &Grid, u: &Ui) {
    clear(s, g, LOWER);
    s.text(g.xf(COL_STEP), g.yf(384.0), g.p(size::HEADING), palette::GREY, Align::Left, "LOG  ·  THE SERIAL CONSOLE CARRIES EVERY LINE");
    let px = g.p(size::HEADING);
    let room = span(g, COL_STEP, COL_RIGHT);
    let skip = u.log.len().saturating_sub(LOG_LINES);
    for (k, l) in u.log.iter().skip(skip).enumerate() {
        s.text_fit(g.xf(COL_STEP), g.yf(420.0 + 30.0 * k as f32), px, palette::BLUE, Align::Left, l, room);
    }
}

/// Redraw some parts of the panel (the frame stays): `header` and the rows
/// `rows` (step indexes), unless the frame must change, which redraws it all.
fn redraw(u: &mut Ui, head: bool, rows: &[usize], progress: bool) {
    if u.framed != Some((failed(u), u.details, u.last, u.install)) {
        draw(u);
        return;
    }
    if u.details {
        return;
    }
    let mut areas: Vec<Area> = rows.iter().map(|&i| row_area(i)).collect();
    if head {
        areas.push(HEAD);
    }
    if progress {
        areas.push(PROGRESS);
    }
    paint(u, &areas, |s, g, u| {
        if head {
            header(s, g, u);
        }
        for &i in rows {
            step_row(s, g, u, i);
        }
        if progress {
            progress_row(s, g, u);
        }
    });
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
    redraw(u, true, &[step as usize], false);
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
    let active = active(u);
    if let Some(i) = active {
        let l = &mut u.steps[i];
        l.state = State::Failed;
        l.end = t.max(1);
        // The reason's first clause in the step's line; all of it below.
        let first = reason.split([':', '.']).next().unwrap_or(reason);
        l.detail = first.chars().take(60).collect();
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

/// The machine, for the header.
pub fn machine(text: &str) {
    let Some(u) = ui() else { return };
    u.machine = String::from(text);
    redraw(u, true, &[], false);
}

/// The name the machine booted as, large in the header.
pub fn name(text: &str) {
    let Some(u) = ui() else { return };
    u.name = Some(String::from(text));
    redraw(u, true, &[], false);
}

/// The boot intent is `install`: the frame says `INSTALL`, as the boot
/// screen's does for an install.
pub fn installing() {
    let Some(u) = ui() else { return };
    u.install = true;
    draw(u);
}

/// The RTC was set from NTP (#77): the header says so.
pub fn synced() {
    let Some(u) = ui() else { return };
    u.synced = true;
    redraw(u, true, &[], false);
}

/// Wait `secs` with the panel's keys read (F2), or a plain stall without it.
pub fn wait(secs: u64) {
    for _ in 0..secs * 10 {
        if let Some(u) = ui() {
            poll_keys(u);
        }
        boot::stall(core::time::Duration::from_millis(100));
    }
}

/// Bytes read from the attached image in all, and the rate.
pub fn progress(bytes: u64, mib_s: u64) {
    let Some(u) = ui() else { return };
    u.progress = Some((bytes, mib_s));
    if now(u).saturating_sub(u.drawn_at) < 250 || u.details {
        return;
    }
    u.drawn_at = now(u);
    redraw(u, true, &[Step::Image as usize], true);
}

/// A bootloader runs under the panel from here: its keys are its own, its
/// lines show in the handoff step, and the frame is the boot screen's first
/// (#220), left up for the screen to draw over.
pub fn handoff(detail: &str) {
    if let Some(u) = ui() {
        u.keys_ours = false;
        u.details = false;
        u.last = true;
    }
    step(Step::Handoff, detail);
}

/// The bootloader came back: keys are ours again.
pub fn handed_back(reason: &str) {
    if let Some(u) = ui() {
        u.keys_ours = true;
        u.last = false;
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

/// Every line anyone prints: kept for F2 and for the network log
/// (`netlog.rs`), written to the serial consoles.
unsafe extern "efiapi" fn out_string(_this: *mut SimpleTextOutputProtocol, s: *const Char16) -> Status {
    // And to the network, once there is one (#162).
    crate::netlog::capture(s);
    let Some(u) = ui() else { return Status::SUCCESS };
    for &d in &u.serials {
        unsafe {
            let _ = ((*d).output_string)(d, s);
        }
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
            paint(u, &[LOWER], details);
        } else if !u.keys_ours && u.steps[Step::Handoff as usize].state == State::Active {
            // The bootloader's lines, one at a time, in its step.
            if let Some(last) = u.log.iter().rev().find(|l| !l.trim().is_empty()) {
                u.steps[Step::Handoff as usize].detail = String::from(last.trim());
            }
            redraw(u, false, &[Step::Handoff as usize], false);
        } else if now(u).saturating_sub(u.drawn_at) >= 500 {
            // The active step's time moves on.
            u.drawn_at = now(u);
            let rows: Vec<usize> = (0..LABELS.len()).filter(|&i| u.steps[i].state == State::Active).collect();
            redraw(u, true, &rows, false);
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
        // GraphicsConsole may have changed the GOP's mode and cleared it:
        // the panel again, at the mode it has now.
        u.screens.retain_mut(|s| s.fit());
        for sc in &u.screens {
            unsafe {
                let _ = ((*sc.out).enable_cursor)(sc.out, Boolean::FALSE);
            }
        }
        u.framed = None;
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
