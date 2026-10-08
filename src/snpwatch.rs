//! Which driver is under each NIC's SNP, and a watch on every call into it
//! (#88).
//!
//! Every deadline in `net.rs` is a TSC compare between calls. A NIC driver's
//! `Receive` or `GetStatus` that never returns is outside all of them: the
//! boot stops with nothing on the console, and that looks exactly like a SOL
//! session that dropped (server1, #78: nine silent minutes). Boot services run
//! on one thread, so a stuck call cannot be abandoned from here. It can be
//! named:
//!
//! - `open_nic` prints the driver before the first SNP call on each NIC
//!   (`driver_of`), so a driver that hangs in `Start` is named on the line
//!   above the silence;
//! - every SNP call is marked (`enter` → `Call`) with its NIC and start time,
//!   and a periodic timer event at `TPL_NOTIFY` prints `nic N: SNP.Receive
//!   (<driver>) has not returned after 5 s` while one is stuck, again every
//!   30 s, and `returned after N s` if it ever comes back.
//!
//! `TPL_NOTIFY` because EDK2's SnpDxe raises to `TPL_CALLBACK` inside its
//! calls, and an event at that level would wait for the very call it is
//! watching. A driver that hangs at `TPL_NOTIFY` or above, or with interrupts
//! off, stops the timer too, and nothing in firmware can say so; the driver
//! line before `Start` is then the last word.
//!
//! The notify function only ever prints while a call is marked, which is
//! never while this binary is printing, so its line does not tear one of
//! ours. Console drivers raise to `TPL_NOTIFY` inside `OutputString`, so it
//! cannot tear the firmware's or a driver's either. `close` (from
//! `net::release`) cancels the timer before the image can be unloaded.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use uefi::boot::{self, SearchType};
use uefi::{Guid, Status, guid};
use uefi_raw::protocol::driver::{ComponentName2Protocol, DriverBindingProtocol};
use uefi_raw::protocol::loaded_image::LoadedImageProtocol;
use uefi_raw::table::boot::{BootServices, EventType, OpenProtocolInformationEntry, TimerDelay, Tpl};

const BY_CHILD_CONTROLLER: u32 = 0x08;
const BY_DRIVER: u32 = 0x10;
const COMPONENT_NAME2: Guid = guid!("6a7a5cff-e8d9-4f70-bada-75ab3025ce14");
const COMPONENT_NAME: Guid = guid!("107a772c-d5e1-11d4-9a46-0090273fc14d");
const DRIVER_BINDING: Guid = guid!("18a031ab-b443-4d1a-a5c0-0c09261e9f71");
const LOADED_IMAGE: Guid = guid!("5b1b31a1-9562-11d2-8e3f-00a0c969723b");
const DEVICE_PATH: Guid = guid!("09576e91-6d3f-11d2-8e39-00a0c969723b");
const PCI_IO: Guid = guid!("4cf5b200-68b8-4ca5-9eec-b23e3f50029a");
/// EFI_NETWORK_INTERFACE_IDENTIFIER_PROTOCOL_31 and its predecessor: an UNDI.
const NII31: Guid = guid!("1aced566-76ed-4218-bc81-767f1f977a89");
const NII: Guid = guid!("e18541cd-f755-4f73-928d-643c8a79b229");

pub(crate) fn bs() -> Option<&'static BootServices> {
    unsafe { uefi::table::system_table_raw()?.as_ref().boot_services.as_ref() }
}

// ------------------------------------------------------------ which driver

/// The driver under this SNP handle, for the console: its ComponentName, else
/// its image's file name, else its handle. `None` when nothing on the handle
/// says who installed it.
///
/// Consumers stack on an SNP handle too, so "who opened something here
/// `BY_DRIVER`" is not the question: under OVMF that answers MNP's VLAN
/// Configuration Driver. Two opens identify the NIC's driver:
///
/// - a driver that made SNP on a child handle (its device path ends in a MAC
///   node: VirtioNetDxe, the stormnic drivers, an UNDI's child) opened its
///   parent's protocols `BY_CHILD_CONTROLLER` for that child. Only asked of a
///   MAC child: a PCI handle is itself PciBusDxe's child;
/// - a driver that bound the handle itself opened its PCI I/O or an UNDI's
///   NII `BY_DRIVER` (option-ROM drivers; SnpDxe over an UNDI).
///
/// Both are named when both are there (`Intel UNDI + SNP driver`).
pub fn driver_of(snp: uefi_raw::Handle) -> Option<String> {
    let bs = bs()?;
    let own = boot::image_handle().as_ptr();
    let mut agents: Vec<uefi_raw::Handle> = Vec::new();
    if ends_in_mac(snp) {
        let handles = boot::locate_handle_buffer(SearchType::AllHandles).ok()?;
        for h in handles.iter().map(|h| h.as_ptr()).filter(|&h| h != snp) {
            for (_, e) in opens(bs, h) {
                if e.attributes & BY_CHILD_CONTROLLER != 0 && e.controller_handle == snp && e.agent_handle != own {
                    agents.push(e.agent_handle);
                }
            }
        }
    }
    for (guid, e) in opens(bs, snp) {
        if [PCI_IO, NII31, NII].contains(&guid) && e.attributes & BY_DRIVER != 0 && e.agent_handle != own {
            agents.push(e.agent_handle);
        }
    }
    let mut names: Vec<String> = Vec::new();
    for a in agents {
        let n = name_of(a).unwrap_or_else(|| format!("driver handle {a:p}"));
        if !names.contains(&n) {
            names.push(n);
        }
    }
    (!names.is_empty()).then(|| names.join(" + "))
}

/// The driver bound to a PCI function, by name: whoever opened its PCI I/O
/// `BY_DRIVER` (#4, the storage controllers in the inventory).
pub fn pci_driver(pci: uefi_raw::Handle) -> Option<String> {
    let bs = bs()?;
    let mut names: Vec<String> = Vec::new();
    for (guid, e) in opens(bs, pci) {
        if guid == PCI_IO && e.attributes & BY_DRIVER != 0 {
            let n = name_of(e.agent_handle).unwrap_or_else(|| format!("driver handle {:p}", e.agent_handle));
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    (!names.is_empty()).then(|| names.join(" + "))
}

/// Whether the handle's device path ends in a MAC node (messaging, subtype
/// 11): a NIC driver's child, not the controller.
fn ends_in_mac(h: uefi_raw::Handle) -> bool {
    let Some(dp) = crate::net::handle_protocol(h, &DEVICE_PATH) else { return false };
    let mut node = dp as *const u8;
    let mut last = (0u8, 0u8);
    for _ in 0..64 {
        let (ty, sub) = unsafe { (*node, *node.add(1)) };
        let len = unsafe { u16::from_le_bytes([*node.add(2), *node.add(3)]) } as usize;
        if ty == 0x7f || len < 4 {
            break;
        }
        last = (ty, sub);
        node = unsafe { node.add(len) };
    }
    last == (3, 11)
}

/// Every open of every protocol on `h`: (protocol, entry).
pub(crate) fn opens(bs: &BootServices, h: uefi_raw::Handle) -> Vec<(Guid, OpenProtocolInformationEntry)> {
    let mut out = Vec::new();
    let mut guids: *mut *const uefi_raw::Guid = ptr::null_mut();
    let mut count = 0usize;
    unsafe {
        if (bs.protocols_per_handle)(h, &mut guids, &mut count) != Status::SUCCESS || guids.is_null() {
            return out;
        }
        for i in 0..count {
            let g = *guids.add(i);
            if g.is_null() {
                continue;
            }
            let mut entries: *const OpenProtocolInformationEntry = ptr::null();
            let mut n = 0usize;
            if (bs.open_protocol_information)(h, g, &mut entries, &mut n) == Status::SUCCESS && !entries.is_null() {
                for k in 0..n {
                    let e = &*entries.add(k);
                    out.push((
                        *(g as *const Guid),
                        OpenProtocolInformationEntry {
                            agent_handle: e.agent_handle,
                            controller_handle: e.controller_handle,
                            attributes: e.attributes,
                            open_count: e.open_count,
                        },
                    ));
                }
                let _ = (bs.free_pool)(entries as *mut u8);
            }
        }
        let _ = (bs.free_pool)(guids as *mut u8);
    }
    out
}

/// A driver's name: ComponentName2, then ComponentName (in the first language
/// each says it supports), then the file name of its image. Asked of the
/// agent handle, then of its DriverBinding's image handle.
pub(crate) fn name_of(agent: uefi_raw::Handle) -> Option<String> {
    let mut handles = alloc::vec![agent];
    if let Some(db) = crate::net::handle_protocol(agent, &DRIVER_BINDING) {
        let image = unsafe { (*(db as *const DriverBindingProtocol)).image_handle };
        if !image.is_null() && image != agent {
            handles.push(image);
        }
    }
    for &h in handles.iter() {
        for (guid, two) in [(COMPONENT_NAME2, true), (COMPONENT_NAME, false)] {
            let Some(p) = crate::net::handle_protocol(h, &guid) else { continue };
            // Both protocols have this layout; only the language codes differ
            // (RFC 4646 `en;fr`, or ISO 639-2 `engfra`).
            let cn = p as *const ComponentName2Protocol;
            let mut lang = [0u8; 16];
            let langs = unsafe { (*cn).supported_languages };
            let mut len = 0;
            while !langs.is_null() && len < lang.len() - 1 {
                let c = unsafe { *langs.add(len) };
                if c == 0 || (two && c == b';') || (!two && len == 3) {
                    break;
                }
                lang[len] = c;
                len += 1;
            }
            if len == 0 {
                lang[..3].copy_from_slice(if two { b"en\0" } else { b"eng" });
            }
            let mut s: *const u16 = ptr::null();
            let st = unsafe { ((*cn).get_driver_name)(cn, lang.as_ptr(), &mut s) };
            if st == Status::SUCCESS && !s.is_null() {
                let name = unsafe { ucs2(s, 96) };
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
    }
    handles.iter().find_map(|&h| image_file(h))
}

/// The last file-path node of an image's `FilePath`, without its directory:
/// `stormnic-ixgbe.efi`.
fn image_file(h: uefi_raw::Handle) -> Option<String> {
    let li = crate::net::handle_protocol(h, &LOADED_IMAGE)? as *const LoadedImageProtocol;
    let mut node = unsafe { (*li).file_path } as *const u8;
    let mut last = None;
    for _ in 0..64 {
        if node.is_null() {
            break;
        }
        let (ty, sub) = unsafe { (*node, *node.add(1)) };
        let len = unsafe { u16::from_le_bytes([*node.add(2), *node.add(3)]) } as usize;
        if ty == 0x7f || len < 4 {
            break;
        }
        if ty == 4 && sub == 4 && len > 4 {
            // Unaligned UCS-2, so copied out before it is read as u16.
            let mut text = Vec::new();
            for k in 0..(len - 4) / 2 {
                let c = unsafe { u16::from_le_bytes([*node.add(4 + 2 * k), *node.add(5 + 2 * k)]) };
                if c == 0 {
                    break;
                }
                text.push(c);
            }
            last = Some(String::from_utf16_lossy(&text));
        }
        node = unsafe { node.add(len) };
    }
    let path = last?;
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).trim();
    (!file.is_empty()).then(|| String::from(file))
}

/// A NUL-terminated UCS-2 string, at most `max` characters.
unsafe fn ucs2(s: *const u16, max: usize) -> String {
    let mut v = Vec::new();
    for k in 0..max {
        let c = unsafe { *s.add(k) };
        if c == 0 {
            break;
        }
        v.push(c);
    }
    String::from(String::from_utf16_lossy(&v).trim())
}

// ---------------------------------------------------------------- the watch

/// The SNP functions this binary calls.
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Snp {
    Start = 1,
    Initialize,
    ReceiveFilters,
    Receive,
    Transmit,
    GetStatus,
}

fn call_name(code: u8) -> &'static str {
    match code {
        1 => "Start",
        2 => "Initialize",
        3 => "ReceiveFilters",
        4 => "Receive",
        5 => "Transmit",
        6 => "GetStatus",
        _ => "?",
    }
}

/// The call in progress (0: none), its NIC, its start in TSC ticks, and the
/// second it was last reported at (0: not yet).
static CALL: AtomicU8 = AtomicU8::new(0);
static NIC: AtomicUsize = AtomicUsize::new(0);
static SINCE: AtomicU64 = AtomicU64::new(0);
static TOLD: AtomicU64 = AtomicU64::new(0);
/// TSC ticks per millisecond, from `net.rs`'s calibration. 0 until `arm`.
static PER_MS: AtomicU64 = AtomicU64::new(0);
static EVENT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// The driver of each NIC, by `net.rs`'s index. Written only while no call is
/// marked, which is the only time the notify function reads it.
struct Names(core::cell::UnsafeCell<Vec<String>>);
unsafe impl Sync for Names {}
static NAMES: Names = Names(core::cell::UnsafeCell::new(Vec::new()));

/// Seconds the first report waits for, and between later ones.
const FIRST: u64 = 5;
const AGAIN: u64 = 30;

fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn secs_since(start: u64) -> u64 {
    let per_ms = PER_MS.load(Ordering::SeqCst);
    if per_ms == 0 {
        return 0;
    }
    rdtsc().wrapping_sub(start) / per_ms / 1000
}

/// Record the name printed for NIC `nic`.
pub fn set_name(nic: usize, name: &str) {
    if CALL.load(Ordering::SeqCst) != 0 {
        return;
    }
    let names = unsafe { &mut *NAMES.0.get() };
    if names.len() <= nic {
        names.resize(nic + 1, String::new());
    }
    names[nic] = String::from(name);
}

/// Start the watch: a 1 s periodic timer. Idempotent. A firmware that will not
/// make the event leaves the calls unwatched, which is how they were before.
pub fn arm(per_ms: u64) -> Result<(), Status> {
    PER_MS.store(per_ms.max(1), Ordering::SeqCst);
    if !EVENT.load(Ordering::SeqCst).is_null() {
        return Ok(());
    }
    let bs = bs().ok_or(Status::UNSUPPORTED)?;
    let mut ev: uefi_raw::Event = ptr::null_mut();
    unsafe {
        let st = (bs.create_event)(
            EventType::TIMER | EventType::NOTIFY_SIGNAL,
            Tpl::NOTIFY,
            Some(tick),
            ptr::null_mut(),
            &mut ev,
        );
        if st != Status::SUCCESS {
            return Err(st);
        }
        // 100 ns units.
        let st = (bs.set_timer)(ev, TimerDelay::PERIODIC, 10_000_000);
        if st != Status::SUCCESS {
            let _ = (bs.close_event)(ev);
            return Err(st);
        }
    }
    EVENT.store(ev, Ordering::SeqCst);
    Ok(())
}

/// Stop the watch. The event's notify function lives in this image, so it
/// must be gone before the image is unloaded (the fall-through).
pub fn close() {
    let ev = EVENT.swap(ptr::null_mut(), Ordering::SeqCst);
    if ev.is_null() {
        return;
    }
    if let Some(bs) = bs() {
        unsafe {
            let _ = (bs.set_timer)(ev, TimerDelay::CANCEL, 0);
            let _ = (bs.close_event)(ev);
        }
    }
}

/// A marked SNP call; unmarked when dropped.
pub struct Call(());

/// Mark a call into NIC `nic`'s SNP until the returned guard drops.
#[inline]
pub fn enter(nic: usize, what: Snp) -> Call {
    TOLD.store(0, Ordering::SeqCst);
    SINCE.store(rdtsc(), Ordering::SeqCst);
    NIC.store(nic, Ordering::SeqCst);
    CALL.store(what as u8, Ordering::SeqCst);
    Call(())
}

impl Drop for Call {
    #[inline]
    fn drop(&mut self) {
        let code = CALL.swap(0, Ordering::SeqCst);
        if TOLD.swap(0, Ordering::SeqCst) != 0 {
            let nic = NIC.load(Ordering::SeqCst);
            uefi::println!(
                "      nic {nic}: SNP.{} returned after {} s",
                call_name(code),
                secs_since(SINCE.load(Ordering::SeqCst))
            );
        }
    }
}

/// The timer's notify function, at `TPL_NOTIFY`, once a second.
unsafe extern "efiapi" fn tick(_: uefi_raw::Event, _: *mut c_void) {
    let code = CALL.load(Ordering::SeqCst);
    if code == 0 {
        return;
    }
    let secs = secs_since(SINCE.load(Ordering::SeqCst));
    let told = TOLD.load(Ordering::SeqCst);
    if !((told == 0 && secs >= FIRST) || (told != 0 && secs >= told + AGAIN)) {
        return;
    }
    TOLD.store(secs.max(1), Ordering::SeqCst);
    let nic = NIC.load(Ordering::SeqCst);
    let names = unsafe { &*NAMES.0.get() };
    let driver = names.get(nic).filter(|n| !n.is_empty()).map(String::as_str).unwrap_or("driver unknown");
    let mut line = Line::new();
    let _ = core::fmt::Write::write_fmt(
        &mut line,
        format_args!(
            "      nic {nic}: SNP.{} ({driver}) has not returned after {secs} s; a NIC driver stuck in a call cannot be abandoned from here\r\n",
            call_name(code)
        ),
    );
    line.flush();
}

/// A console line built without allocating, for the notify function.
struct Line {
    buf: [u16; 256],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Line { buf: [0; 256], len: 0 }
    }

    fn flush(&mut self) {
        if self.len == 0 {
            return;
        }
        self.buf[self.len.min(self.buf.len() - 1)] = 0;
        unsafe {
            let Some(st) = uefi::table::system_table_raw() else { return };
            let out = st.as_ref().stdout;
            if !out.is_null() {
                let _ = ((*out).output_string)(out, self.buf.as_ptr() as *const _);
            }
        }
        self.len = 0;
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            if self.len >= self.buf.len() - 1 {
                self.flush();
            }
            let c = c as u32;
            self.buf[self.len] = if c < 0x10000 { c as u16 } else { b'?' as u16 };
            self.len += 1;
        }
        Ok(())
    }
}
