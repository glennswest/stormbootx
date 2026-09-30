//! Where stormbootx's randomness comes from (#56): the TCP ISN seed, each
//! connection's local port, and the DHCP transaction id.
//!
//! The first source present wins (owner, 2026-09-30):
//!
//! 1. the firmware's `EFI_RNG_PROTOCOL`, **read only**: nothing here installs
//!    one, so the next stage never finds a weak RNG left behind (the Linux EFI
//!    stub seeds its own from that protocol);
//! 2. the CPU's own: RDSEED, then RDRAND on x86_64, and RNDR on aarch64. A new
//!    architecture is a new arm of `cpu`;
//! 3. **jitter**, which works everywhere: the cycle counter (RDTSC, or CNTVCT
//!    on ARM) sampled across short `Stall`s of varying length, mixed with
//!    SHA-256 (`sha256.rs`) together with the firmware's time, the NIC's MAC
//!    and the SMBIOS UUID.
//!
//! **Nothing here can fail the boot.** Jitter always produces a seed: on a
//! machine with nothing else it is a weak one, but a machine that boots with a
//! weak ISN is better than a machine that does not boot. The console names the
//! source (`rng : firmware | rdrand | rndr | jitter`).
//!
//! `rng = cpu` or `rng = jitter` in `stormboot.conf` skips the sources above
//! it. That is how the OVMF test exercises the jitter path (and it's the way
//! to take a suspect firmware RNG out of the picture on a real machine).
//!
//! Past the first draw, a firmware or CPU source is asked afresh for every
//! value, and if a later draw fails, the value falls back to the SHA-256
//! generator. Jitter feeds only that generator: 32 bytes of seed, then
//! SHA-256 over the seed and a counter.

use crate::sha256::Sha256;

/// Which source is in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Firmware,
    Rdseed,
    Rdrand,
    #[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
    Rndr,
    Jitter,
}

impl Source {
    /// The word the console prints: `rdrand` covers both x86 instructions.
    pub fn name(self) -> &'static str {
        match self {
            Source::Firmware => "firmware",
            Source::Rdseed | Source::Rdrand => "rdrand",
            Source::Rndr => "rndr",
            Source::Jitter => "jitter",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Source::Firmware => "EFI_RNG_PROTOCOL",
            Source::Rdseed => "RDSEED",
            Source::Rdrand => "RDRAND",
            Source::Rndr => "RNDR",
            Source::Jitter => "cycle-counter jitter, time, MAC and SMBIOS UUID through SHA-256",
        }
    }
}

/// The first source to try, from `rng =` in `stormboot.conf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Firmware,
    Cpu,
    Jitter,
}

impl Start {
    pub fn parse(s: &str) -> Option<Start> {
        match s.trim() {
            "firmware" | "auto" => Some(Start::Firmware),
            "cpu" => Some(Start::Cpu),
            "jitter" => Some(Start::Jitter),
            _ => None,
        }
    }
}

pub struct Entropy {
    pub source: Source,
    /// SHA-256 generator state: the fallback for a failed draw, and all of
    /// jitter's output.
    seed: [u8; 32],
    counter: u64,
}

impl Entropy {
    /// Pick the source. `mac` is the machine's NIC MAC, mixed into jitter.
    pub fn new(start: Start, mac: Option<[u8; 6]>) -> Entropy {
        let mut first = [0u8; 32];
        let mut source = None;
        if start == Start::Firmware && firmware(&mut first) {
            source = Some(Source::Firmware);
        }
        if source.is_none() && start != Start::Jitter {
            source = cpu::sources().into_iter().find(|&s| cpu::fill(s, &mut first));
        }
        let mut e = Entropy { source: source.unwrap_or(Source::Jitter), seed: [0; 32], counter: 0 };
        if source.is_none() {
            jitter(&mut first, mac);
        } else {
            // A hardware seed still goes through the hash with the jitter
            // inputs: a biased source is never worse for it.
            let mut j = [0u8; 32];
            jitter_light(&mut j, mac);
            let mut h = Sha256::new();
            h.update(&first);
            h.update(&j);
            first = *h.finalize().as_bytes();
        }
        e.seed = first;
        e
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        let fresh = match self.source {
            Source::Firmware => firmware(&mut b),
            Source::Jitter => false,
            s => cpu::fill(s, &mut b),
        };
        if fresh {
            return u64::from_le_bytes(b);
        }
        self.counter += 1;
        let mut h = Sha256::new();
        h.update(&self.seed);
        h.update(&self.counter.to_le_bytes());
        let d = h.finalize();
        u64::from_le_bytes(d.as_bytes()[..8].try_into().unwrap())
    }
}

// ------------------------------------------------------------- the firmware

#[repr(C)]
struct RngProtocol {
    get_info: unsafe extern "efiapi" fn(*mut RngProtocol, *mut usize, *mut uefi::Guid) -> uefi::Status,
    get_rng: unsafe extern "efiapi" fn(*mut RngProtocol, *const uefi::Guid, usize, *mut u8) -> uefi::Status,
}

const RNG_PROTOCOL: uefi::Guid = uefi::guid!("3152bca5-eade-433d-862e-c01cdc291f44");

/// Fill `buf` from `EFI_RNG_PROTOCOL` with its default algorithm.
fn firmware(buf: &mut [u8]) -> bool {
    let Ok(handles) = uefi::boot::locate_handle_buffer(uefi::boot::SearchType::ByProtocol(&RNG_PROTOCOL)) else {
        return false;
    };
    for h in handles.iter() {
        let Some(p) = crate::net::handle_protocol(h.as_ptr(), &RNG_PROTOCOL) else { continue };
        let rng = p as *mut RngProtocol;
        let st = unsafe { ((*rng).get_rng)(rng, core::ptr::null(), buf.len(), buf.as_mut_ptr()) };
        if st == uefi::Status::SUCCESS && buf.iter().any(|&b| b != 0) {
            return true;
        }
    }
    false
}

// ------------------------------------------------------------------ the CPU

#[cfg(target_arch = "x86_64")]
mod cpu {
    use super::Source;

    /// What this CPU has, best first (CPUID.7:EBX bit 18 RDSEED, CPUID.1:ECX
    /// bit 30 RDRAND).
    pub fn sources() -> alloc::vec::Vec<Source> {
        let mut v = alloc::vec::Vec::new();
        let max = core::arch::x86_64::__cpuid(0).eax;
        if max >= 7 && core::arch::x86_64::__cpuid_count(7, 0).ebx & (1 << 18) != 0 {
            v.push(Source::Rdseed);
        }
        if core::arch::x86_64::__cpuid(1).ecx & (1 << 30) != 0 {
            v.push(Source::Rdrand);
        }
        v
    }

    fn one(seed: bool) -> Option<u64> {
        // Retried, as Intel advises; RDSEED runs dry faster than RDRAND.
        for _ in 0..if seed { 100 } else { 10 } {
            let v: u64;
            let ok: u8;
            unsafe {
                if seed {
                    core::arch::asm!("rdseed {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok,
                        options(nomem, nostack));
                } else {
                    core::arch::asm!("rdrand {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok,
                        options(nomem, nostack));
                }
            }
            // All-zeros and all-ones are what broken parts return.
            if ok == 1 && v != 0 && v != u64::MAX {
                return Some(v);
            }
        }
        None
    }

    pub fn fill(s: Source, buf: &mut [u8]) -> bool {
        let seed = match s {
            Source::Rdseed => true,
            Source::Rdrand => false,
            _ => return false,
        };
        for chunk in buf.chunks_mut(8) {
            match one(seed) {
                Some(v) => chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]),
                None => return false,
            }
        }
        true
    }

    pub fn counter() -> u64 {
        unsafe { core::arch::x86_64::_rdtsc() }
    }
}

#[cfg(target_arch = "aarch64")]
mod cpu {
    use super::Source;

    /// ID_AA64ISAR0_EL1.RNDR, bits [63:60].
    pub fn sources() -> alloc::vec::Vec<Source> {
        let isar0: u64;
        unsafe { core::arch::asm!("mrs {}, id_aa64isar0_el1", out(reg) isar0, options(nomem, nostack)) };
        if (isar0 >> 60) & 0xf != 0 { alloc::vec![Source::Rndr] } else { alloc::vec::Vec::new() }
    }

    pub fn fill(s: Source, buf: &mut [u8]) -> bool {
        if s != Source::Rndr {
            return false;
        }
        for chunk in buf.chunks_mut(8) {
            let mut got = None;
            for _ in 0..10 {
                let v: u64;
                let ok: u64;
                // RNDR sets NZCV to 0b0100 (Z) on failure.
                unsafe {
                    core::arch::asm!("mrs {v}, s3_3_c2_c4_0", "cset {ok}, ne", v = out(reg) v,
                        ok = out(reg) ok, options(nomem, nostack));
                }
                if ok == 1 {
                    got = Some(v);
                    break;
                }
            }
            match got {
                Some(v) => chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]),
                None => return false,
            }
        }
        true
    }

    pub fn counter() -> u64 {
        let v: u64;
        unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack)) };
        v
    }
}

// ---------------------------------------------------------------- jitter

/// The non-random inputs: firmware time, MAC, SMBIOS UUID. They make two
/// machines differ, not one boot unpredictable.
fn context(h: &mut Sha256, mac: Option<[u8; 6]>) {
    if let Ok(t) = uefi::runtime::get_time() {
        h.update(&t.year().to_le_bytes());
        h.update(&[t.month(), t.day(), t.hour(), t.minute(), t.second()]);
        h.update(&t.nanosecond().to_le_bytes());
    }
    if let Some(m) = mac {
        h.update(&m);
    }
    if let Some(u) = crate::smbios::uuid() {
        h.update(&u);
    }
    h.update(&cpu::counter().to_le_bytes());
}

/// The jitter seed: 4096 samples of the cycle counter across stalls of 1..8
/// µs, whose exact length the firmware's timer, the cache and the bus decide.
/// Each sample's delta goes into the hash, so the low bits that do vary are
/// kept and the ones that do not cost nothing. About 20 ms.
fn jitter(out: &mut [u8; 32], mac: Option<[u8; 6]>) {
    let mut h = Sha256::new();
    context(&mut h, mac);
    let mut prev = cpu::counter();
    for i in 0..4096u64 {
        let wait = 1 + ((prev ^ i) & 7);
        uefi::boot::stall(core::time::Duration::from_micros(wait));
        let now = cpu::counter();
        h.update(&now.wrapping_sub(prev).to_le_bytes());
        prev = now;
    }
    context(&mut h, mac);
    *out = *h.finalize().as_bytes();
}

/// A few samples only, mixed into a hardware seed.
fn jitter_light(out: &mut [u8; 32], mac: Option<[u8; 6]>) {
    let mut h = Sha256::new();
    context(&mut h, mac);
    let mut prev = cpu::counter();
    for _ in 0..64 {
        uefi::boot::stall(core::time::Duration::from_micros(1));
        let now = cpu::counter();
        h.update(&now.wrapping_sub(prev).to_le_bytes());
        prev = now;
    }
    *out = *h.finalize().as_bytes();
}
