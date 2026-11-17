//! The handoff's guard (#219): what a frozen screen says about where the
//! boot stopped, and a watchdog over the handoff.
//!
//! forge1 (R6415, stormnic-mlx5 bound) froze twice for hours on `EFI stub:
//! Loaded initrd`, the Linux EFI stub's last line before ExitBootServices,
//! with nothing on the serial port after it. Between that line and the
//! kernel's first are the stub's ExitBootServices call and every driver's
//! ExitBootServices handler (the NIC drivers stop their DMA there). So:
//!
//! - **`BEFORE_EXIT_BOOT_SERVICES`** (an event group; boot services still
//!   work): one line, `handoff : ExitBootServices called …`, to the serial
//!   consoles, and the panel's handoff step Blt'd from a row `arm` drew
//!   (#220: the panel is pixels). Once only, and with nothing allocated: a
//!   handler here that changes the memory map makes the call fail on its
//!   map key, and a loader that retries would see it again.
//! - **`EXIT_BOOT_SERVICES`** at `TPL_CALLBACK`, which EDK2 runs after the
//!   `TPL_NOTIFY` handlers the drivers use: no boot services are left, so it
//!   paints a green line along the bottom of the screen's framebuffer.
//!   Every handler before it returned; a freeze with the line is the
//!   kernel's.
//! - **The UEFI watchdog** (`handoff_watchdog =`, default 300 s), armed as
//!   the bootloader starts, so a hang before ExitBootServices resets the
//!   machine instead of sitting until someone cuts the power. The firmware
//!   turns it off at ExitBootServices, and `disarm` does when the bootloader
//!   comes back to us. A hang inside or after ExitBootServices is beyond any
//!   UEFI timer.
//!
//! Both events' notify functions live in this image, so `disarm` closes them
//! before stormbootx can return and be unloaded.

use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

use uefi::boot::{self, SearchType};
use uefi_raw::protocol::console::{GraphicsOutputProtocol, SimpleTextOutputProtocol};
use uefi_raw::table::boot::{BootServices, EventType, Tpl};
use uefi_raw::{Event, Guid, Status, guid};

/// `EFI_EVENT_GROUP_BEFORE_EXIT_BOOT_SERVICES` (UEFI 2.8).
const BEFORE_EBS: Guid = guid!("8be0e274-3970-4b44-80c5-1ab9502f3bfc");
const DEVICE_PATH: Guid = guid!("09576e91-6d3f-11d2-8e39-00a0c969723b");
/// Watchdog codes up to 0xFFFF are the firmware's.
const WATCHDOG_CODE: u64 = 0x1_0219;
/// The line painted at ExitBootServices: pixels high, and 0x00RRGGBB green,
/// which is green in both 32-bit layouts (the green byte is byte 1 in each).
const BAND: usize = 8;
const GREEN: u32 = 0x0000_c000;

/// The serial line and the panel row, made before the handoff.
pub const SAID: &str = "handoff     : ExitBootServices called by the bootloader; the drivers stop their devices, the kernel is next";
const ROW: &str = "ExitBootServices: drivers stopping their devices; kernel next";

struct Guard {
    serials: Vec<*mut SimpleTextOutputProtocol>,
    /// The panel's handoff row on each screen, drawn saying `ROW`.
    marks: Vec<crate::ui::Mark>,
    /// `ConOut` itself when the panel does not hold it.
    conout: *mut SimpleTextOutputProtocol,
    line: Vec<u16>,
    /// The framebuffer: base, pixels per scan line, width, height.
    fb: Option<(u64, usize, usize, usize)>,
    before: Event,
    at: Event,
    watchdog: u64,
}

static mut GUARD: Option<Guard> = None;
static SAID_ONCE: AtomicBool = AtomicBool::new(false);
static PAINTED: AtomicBool = AtomicBool::new(false);

#[allow(static_mut_refs)]
fn guard() -> Option<&'static mut Guard> {
    unsafe { GUARD.as_mut() }
}

fn bs() -> Option<&'static BootServices> {
    unsafe { uefi::table::system_table_raw()?.as_ref().boot_services.as_ref() }
}

fn ucs2(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.chars().map(|c| if (0x20..0x7f).contains(&(c as u32)) { c as u16 } else { b'?' as u16 }).collect();
    v.push(0);
    v
}

/// The first real screen's linear framebuffer, if it has one (not BLT-only,
/// 32 bits a pixel). ConSplitter's virtual GOP has no device path.
fn framebuffer() -> Option<(u64, usize, usize, usize)> {
    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&GraphicsOutputProtocol::GUID)).ok()?;
    for h in handles.iter() {
        if crate::net::handle_protocol(h.as_ptr(), &DEVICE_PATH).is_none() {
            continue;
        }
        let Some(g) = crate::net::handle_protocol(h.as_ptr(), &GraphicsOutputProtocol::GUID) else { continue };
        let g = g as *const GraphicsOutputProtocol;
        unsafe {
            let mode = (*g).mode;
            if mode.is_null() || (*mode).info.is_null() {
                continue;
            }
            let info = &*(*mode).info;
            // 0: RGB, 1: BGR, both 8 bits a colour and a reserved byte.
            if info.pixel_format.0 > 1 || (*mode).frame_buffer_base == 0 {
                continue;
            }
            let (w, h, stride) = (info.horizontal_resolution as usize, info.vertical_resolution as usize, info.pixels_per_scan_line as usize);
            if w == 0 || h <= BAND || stride < w || (*mode).frame_buffer_size < stride * h * 4 {
                continue;
            }
            return Some(((*mode).frame_buffer_base, stride, w, h));
        }
    }
    None
}

/// As the bootloader from the attached image is started: the two events and
/// the watchdog. Says on the console what was armed.
pub fn arm() {
    if guard().is_some() {
        return;
    }
    let Some(bs) = bs() else { return };
    // The consoles as they are now, not as they were at `ui::begin`.
    crate::ui::rescan();
    let conout = uefi::table::system_table_raw().map(|st| unsafe { st.as_ref().stdout }).unwrap_or(ptr::null_mut());
    let (serials, marks) = crate::ui::raw_targets(ROW).unwrap_or_default();
    let mut g = Guard {
        serials,
        marks,
        conout,
        line: ucs2(&alloc::format!("{SAID}\r\n")),
        fb: framebuffer(),
        before: ptr::null_mut(),
        at: ptr::null_mut(),
        watchdog: crate::config::handoff_watchdog(),
    };
    SAID_ONCE.store(false, Ordering::SeqCst);
    PAINTED.store(false, Ordering::SeqCst);
    let mut group = BEFORE_EBS;
    let before = unsafe {
        (bs.create_event_ex)(EventType::NOTIFY_SIGNAL, Tpl::CALLBACK, Some(before_ebs), ptr::null_mut(), &mut group, &mut g.before)
    };
    let at = unsafe {
        (bs.create_event)(EventType::SIGNAL_EXIT_BOOT_SERVICES, Tpl::CALLBACK, Some(at_ebs), ptr::null_mut(), &mut g.at)
    };
    let dog = if g.watchdog > 0 {
        unsafe { (bs.set_watchdog_timer)(g.watchdog as usize, WATCHDOG_CODE, 0, ptr::null()) }
    } else {
        Status::SUCCESS
    };
    let fb = g.fb.map(|(_, _, w, h)| alloc::format!("a green line on the {w}x{h} screen")).unwrap_or_else(|| "no framebuffer to mark".into());
    let watch = match (g.watchdog, dog) {
        (0, _) => alloc::string::String::from("no watchdog (handoff_watchdog = off)"),
        (s, Status::SUCCESS) => alloc::format!("watchdog {s} s until ExitBootServices"),
        (s, st) => alloc::format!("watchdog {s} s refused ({st:?})"),
    };
    let marks = match (before, at) {
        (Status::SUCCESS, Status::SUCCESS) => alloc::format!("ExitBootServices marked (a console line before, {fb} after the drivers)"),
        (b, a) => alloc::format!("ExitBootServices not fully marked (before: {b:?}, after: {a:?})"),
    };
    if before != Status::SUCCESS {
        g.before = ptr::null_mut();
    }
    if at != Status::SUCCESS {
        g.at = ptr::null_mut();
    }
    unsafe { GUARD = Some(g) };
    uefi::println!("handoff     : {watch}; {marks}");
}

/// The bootloader came back to us: the watchdog off, the events closed.
pub fn disarm() {
    #[allow(static_mut_refs)]
    let Some(g) = (unsafe { GUARD.take() }) else { return };
    let Some(bs) = bs() else { return };
    unsafe {
        if g.watchdog > 0 {
            let _ = (bs.set_watchdog_timer)(0, WATCHDOG_CODE, 0, ptr::null());
        }
        if !g.before.is_null() {
            let _ = (bs.close_event)(g.before);
        }
        if !g.at.is_null() {
            let _ = (bs.close_event)(g.at);
        }
    }
}

/// Boot services still work, but nothing here allocates: the line and the
/// row were made by `arm`.
unsafe extern "efiapi" fn before_ebs(_: Event, _: *mut c_void) {
    if SAID_ONCE.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(g) = guard() else { return };
    unsafe {
        if g.serials.is_empty() && g.marks.is_empty() {
            if !g.conout.is_null() {
                let _ = ((*g.conout).output_string)(g.conout, g.line.as_ptr());
            }
            return;
        }
        for &d in &g.serials {
            let _ = ((*d).output_string)(d, g.line.as_ptr());
        }
        for m in &g.marks {
            m.show();
        }
    }
}

/// After the drivers' handlers: memory writes only.
unsafe extern "efiapi" fn at_ebs(_: Event, _: *mut c_void) {
    if PAINTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(g) = guard() else { return };
    let Some((base, stride, w, h)) = g.fb else { return };
    let px = base as usize as *mut u32;
    for y in h - BAND..h {
        for x in 0..w {
            unsafe { ptr::write_volatile(px.add(y * stride + x), GREEN) };
        }
    }
}
