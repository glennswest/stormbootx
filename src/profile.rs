//! Where a boot's seconds go (#168): one `timing :` line before the result.
//!
//! The master's review (2026-10-21) measured stormbootx at 11–15 s on metal
//! against 3 s on pve, and the only way to say where was to line a console
//! log up with its byte-offset timestamps. `mark` notes the end of each step
//! on the TSC (CNTVCT on aarch64); `line` turns the marks into durations,
//! calibrating the counter then, so nothing is spent on it at the start.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;

struct Marks(UnsafeCell<Vec<(&'static str, u64)>>);
// Boot services are single-threaded.
unsafe impl Sync for Marks {}
static MARKS: Marks = Marks(UnsafeCell::new(Vec::new()));

fn marks() -> &'static mut Vec<(&'static str, u64)> {
    unsafe { &mut *MARKS.0.get() }
}

/// The boot starts now.
pub fn begin() {
    let m = marks();
    m.clear();
    m.push(("start", crate::entropy::ticks()));
}

/// The step `label` ends now.
pub fn mark(label: &'static str) {
    marks().push((label, crate::entropy::ticks()));
}

fn ms_text(ms: u64) -> String {
    if ms >= 1000 {
        format!("{}.{:02} s", ms / 1000, (ms % 1000) / 10)
    } else {
        format!("{ms} ms")
    }
}

/// `nic fec 4.03 s, drivers 2.10 s, …; 11.20 s in all`, or none before two
/// marks.
pub fn line() -> Option<String> {
    let m = marks();
    if m.len() < 2 {
        return None;
    }
    let t = crate::entropy::ticks();
    uefi::boot::stall(core::time::Duration::from_millis(10));
    let per_ms = ((crate::entropy::ticks() - t) / 10).max(1);
    let ms = |a: u64, b: u64| b.saturating_sub(a) / per_ms;
    let steps: Vec<String> = m.windows(2).map(|w| format!("{} {}", w[1].0, ms_text(ms(w[0].1, w[1].1)))).collect();
    let total = ms(m[0].1, m[m.len() - 1].1);
    Some(format!("{}; {} in all", steps.join(", "), ms_text(total)))
}

/// Print `line` on the console (and so to the collectors, #162).
pub fn print() {
    if let Some(l) = line() {
        uefi::println!("timing      : {l}");
    }
}
