//! tcp4probe — does this machine's firmware carry an upper network stack?
//!
//! A second, standalone UEFI binary. `stormbootx` needs exactly one thing from
//! the platform that a NIC does not imply: `EFI_TCP4_PROTOCOL`. The TCP/IP
//! stack is a separate set of DXE drivers, and firmware commonly loads them
//! only once network boot is enabled in setup — so "the machine has a network
//! card" and "the machine can run this boot agent" are different questions.
//!
//! This answers the second one before anyone writes a stick, and it is the
//! thing to run first on every new server model. It reports two levels,
//! because the first is necessary and not sufficient:
//!
//!   1. **Which protocols exist**, layer by layer, so a stack that is present
//!      but stops at MNP is visible as exactly that rather than as "no TCP4".
//!   2. **Whether a TCP4 child can actually be created and configured**, which
//!      is what `stormbootx` does and the only test that proves it will work.
//!
//! A `ConnectController` pass runs between the two when TCP4 is missing:
//! drivers that are present but unbound are the failure this is most likely to
//! turn up, and the fix for them is free.
//!
//! Reference point: **Fedora's OVMF has no upper network stack at all.** SNP
//! appears, MNP/IP4/TCP4 do not, and a `ConnectController` pass over every
//! handle changes nothing. Seeing that here means the emulator, not the code.

#![no_main]
#![no_std]

extern crate alloc;

// The whole socket module comes along; this binary only needs the handle
// survey and one connect, so most of it is dead here by design. `dhcp4` comes
// with it because `tcp4` falls back to running DHCP itself, and the probe
// used to be the agent's path (until #56 it was; stormbootx now carries its
// own TCP/IP in net.rs and uses neither). `universal` comes because
// `tcp4::machine_mac` picks the MAC with it.
#[path = "tcp4.rs"]
#[allow(dead_code)]
mod tcp4;
#[path = "dhcp4.rs"]
#[allow(dead_code)]
mod dhcp4;
#[path = "universal.rs"]
#[allow(dead_code)]
mod universal;
// For reading back stormbootx's `install-config.yaml` hand-down (#79) the way
// the initramfs will: the header, the chunks, the digest.
#[path = "installconf.rs"]
#[allow(dead_code)]
mod installconf;
#[path = "sha256.rs"]
#[allow(dead_code)]
mod sha256;

use uefi::boot::{self, SearchType};
use uefi::prelude::*;
use uefi::{Guid, guid};

/// The stack, bottom to top. A run that stops partway down this list tells you
/// which driver is missing, which is a different conversation with a firmware
/// vendor than "networking does not work".
const LAYERS: &[(&str, Guid)] = &[
    ("EFI_SIMPLE_NETWORK", guid!("a19832b9-ac25-11d3-9a2d-0090273fc14d")),
    ("EFI_MANAGED_NETWORK_SB", guid!("f36ff770-a7e1-42cf-9ed2-56f0f271f44c")),
    ("EFI_ARP_SB", guid!("f44c00ee-1f2c-4a00-aa09-1c9f3e0800a3")),
    ("EFI_DHCP4_SB", guid!("9d9a39d8-bd42-4a73-a4d5-8ee94be11380")),
    ("EFI_IP4_SB", guid!("c51711e7-b4bf-404a-bfb8-0a048ef1ffe4")),
    ("EFI_IP4_CONFIG2", guid!("5b446ed1-e30b-4faa-871a-3654eca36080")),
    ("EFI_UDP4_SB", guid!("83f01464-99bd-45e5-b383-af6305d8e9e6")),
    ("EFI_TCP4_SB", guid!("00720665-67eb-4a99-baf7-d3c33a1c7cc9")),
    ("EFI_TCP4", guid!("65530bc7-a359-410f-b010-5aadc7ec2b62")),
];

fn count(guid: &Guid) -> usize {
    boot::locate_handle_buffer(SearchType::ByProtocol(guid))
        .map(|h| h.len())
        .unwrap_or(0)
}

fn survey() {
    for (name, guid) in LAYERS {
        match count(guid) {
            0 => uefi::println!("  {name:<24} absent"),
            n => uefi::println!("  {name:<24} {n} handle(s)"),
        }
    }
}

/// What a loader before this one handed down to Linux (stormbootx #76, #77):
/// the volatile variables, as an image started after it reads them. Silent
/// when there are none, which is every boot not chain-loaded by stormbootx.
fn handed_down() {
    use uefi::runtime::{VariableVendor, get_variable};
    let vendor = VariableVendor(guid!("ab361f54-0166-44a4-a088-1ac22e98ab76"));
    for (name, label) in [
        (uefi::cstr16!("StormBootTag"), "StormBootTag"),
        (uefi::cstr16!("StormBootHostNqn"), "StormBootHostNqn"),
        (uefi::cstr16!("StormBootClock"), "StormBootClock"),
        (uefi::cstr16!("StormBootUpdate"), "StormBootUpdate"),
    ] {
        let mut buf = [0u8; 256];
        if let Ok((value, attrs)) = get_variable(name, &vendor, &mut buf) {
            uefi::println!(
                "handed down : {label} = {}  (attributes {:#x})",
                core::str::from_utf8(value).unwrap_or("<not UTF-8>"),
                attrs.bits()
            );
        }
    }
    install_config(&vendor);
}

/// Reassemble `StormBootInstallConfig` (#79) as the initramfs will, and say
/// whether it is whole: length and SHA-256 against the header. The content is
/// not printed; it carries secrets.
fn install_config(vendor: &uefi::runtime::VariableVendor) {
    use uefi::runtime::get_variable;
    let mut buf = [0u8; 256];
    let Ok((value, attrs)) = get_variable(uefi::cstr16!("StormBootInstallConfig"), vendor, &mut buf) else {
        return;
    };
    let text = core::str::from_utf8(value).unwrap_or("");
    uefi::println!("handed down : StormBootInstallConfig = {text}  (attributes {:#x})", attrs.bits());
    let Some(h) = installconf::parse_header(text) else {
        uefi::println!("install cfg : the header does not parse");
        return;
    };
    let mut hash = sha256::Sha256::default();
    let mut total = 0usize;
    for i in 0..h.chunks {
        let mut name = [0u8; 40];
        let mut wide = [0u16; 40];
        let mut chunk = [0u8; installconf::CHUNK];
        let var = match uefi::CStr16::from_str_with_buf(installconf::chunk_name(i, &mut name), &mut wide) {
            Ok(v) => v,
            Err(_) => return,
        };
        match get_variable(var, vendor, &mut chunk) {
            Ok((data, a)) if a.bits() == attrs.bits() => {
                hash.update(data);
                total += data.len();
            }
            _ => {
                uefi::println!("install cfg : chunk {i} of {} missing", h.chunks);
                return;
            }
        }
    }
    let ok = total == h.len && hash.finalize().matches_hex(h.sha256);
    uefi::println!(
        "install cfg : {total} bytes reassembled from {} chunk(s), {}",
        h.chunks,
        if ok { "length and sha256 match" } else { "MISMATCH" }
    );
}

/// What a bootloader sees of the volume it was loaded from, through the
/// `SimpleFileSystem` on its `DeviceHandle` (#42): shim opens `grubx64.efi`
/// there, systemd-boot its loader entries. On an ESP stormbootx read itself,
/// that filesystem is stormbootx's (`espfs.rs`); `tests/esp-ovmf.sh` checks
/// every line.
fn boot_fs() {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;
    use uefi::cstr16;
    use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode, FileSystemInfo};

    let mut fs = match boot::get_image_file_system(boot::image_handle()) {
        Ok(fs) => fs,
        Err(e) => {
            uefi::println!("boot fs     : none on this image's DeviceHandle ({:?})", e.status());
            return;
        }
    };
    let mut root = match fs.open_volume() {
        Ok(r) => r,
        Err(e) => {
            uefi::println!("boot fs     : OpenVolume failed ({:?})", e.status());
            return;
        }
    };
    let read_only = match root.get_boxed_info::<FileSystemInfo>() {
        Ok(i) => {
            uefi::println!(
                "boot fs     : {} bytes, {}, label \"{}\", {}-byte blocks",
                i.volume_size(),
                if i.read_only() { "read-only" } else { "writable" },
                i.volume_label(),
                i.block_size()
            );
            i.read_only()
        }
        Err(e) => {
            uefi::println!("boot fs     : GetInfo(FileSystemInfo) failed ({:?})", e.status());
            false
        }
    };

    // A listing, the way a loader looks for what to load.
    match root.open(cstr16!("\\EFI\\BOOT"), FileMode::Read, FileAttribute::empty()).map(|h| h.into_directory()) {
        Ok(Some(mut dir)) => {
            let mut names: Vec<String> = Vec::new();
            loop {
                match dir.read_entry_boxed() {
                    Ok(Some(info)) => names.push(format!("{}", info.file_name())),
                    Ok(None) => break,
                    Err(e) => {
                        names.push(format!("<{:?}>", e.status()));
                        break;
                    }
                }
            }
            uefi::println!("boot fs     : \\EFI\\BOOT lists {}", names.join(" | "));
        }
        Ok(None) => uefi::println!("boot fs     : \\EFI\\BOOT is not a directory"),
        Err(e) => uefi::println!("boot fs     : no \\EFI\\BOOT ({:?})", e.status()),
    }

    // The bootloader itself, read in an odd size, then sought into.
    match root
        .open(cstr16!("\\EFI\\BOOT\\BOOTX64.EFI"), FileMode::Read, FileAttribute::empty())
        .map(|h| h.into_regular_file())
    {
        Ok(Some(mut f)) => {
            let size = f.get_boxed_info::<FileInfo>().map(|i| i.file_size()).unwrap_or(0);
            if size > 8 << 20 {
                uefi::println!("boot fs     : BOOTX64.EFI is {size} bytes; not hashed here");
            } else {
                let mut hash = sha256::Sha256::new();
                let mut buf = alloc::vec![0u8; 7919];
                let mut total = 0u64;
                let mut mid = [0u8; 64];
                let half = size / 2;
                loop {
                    match f.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            for (i, &b) in buf[..n].iter().enumerate() {
                                let at = total + i as u64;
                                if at >= half && at < half + 64 {
                                    mid[(at - half) as usize] = b;
                                }
                            }
                            hash.update(&buf[..n]);
                            total += n as u64;
                        }
                        Err(e) => {
                            uefi::println!("boot fs     : Read failed at {total} ({:?})", e.status());
                            return;
                        }
                    }
                }
                let hex = hash.finalize().to_hex();
                uefi::println!(
                    "boot fs     : BOOTX64.EFI {total} bytes, sha256 {}",
                    core::str::from_utf8(&hex).unwrap_or("?")
                );
                let mut again = [0u8; 64];
                let n = (size - half).min(64) as usize;
                let seek = f.set_position(half).is_ok()
                    && f.read(&mut again[..n]).is_ok_and(|got| got == n)
                    && again[..n] == mid[..n]
                    && f.set_position(u64::MAX).is_ok()
                    && f.get_position().is_ok_and(|p| p == size)
                    && f.read(&mut again).is_ok_and(|got| got == 0);
                uefi::println!("boot fs     : seek {}", if seek { "ok" } else { "WRONG" });
            }
        }
        Ok(None) => uefi::println!("boot fs     : BOOTX64.EFI is a directory"),
        Err(e) => uefi::println!("boot fs     : BOOTX64.EFI does not open ({:?})", e.status()),
    }

    // A systemd-boot loader entry, opened relative to a directory.
    if let Ok(Some(mut loader)) =
        root.open(cstr16!("loader"), FileMode::Read, FileAttribute::empty()).map(|h| h.into_directory())
    {
        let entry = loader
            .open(cstr16!("entries\\stormbootx-test.conf"), FileMode::Read, FileAttribute::empty())
            .map(|h| h.into_regular_file());
        if let Ok(Some(mut e)) = entry {
            let mut buf = [0u8; 256];
            let n = e.read(&mut buf).unwrap_or(0);
            let text = core::str::from_utf8(&buf[..n]).unwrap_or("?");
            uefi::println!("boot fs     : loader entry: {}", text.lines().next().unwrap_or(""));
        }
    }

    // A volume that says it is read-only must refuse a write.
    if read_only {
        match root.open(cstr16!("\\written.txt"), FileMode::CreateReadWrite, FileAttribute::empty()) {
            Ok(_) => uefi::println!("boot fs     : a read-only volume let a file be created"),
            Err(e) => uefi::println!("boot fs     : create refused ({:?})", e.status()),
        }
    }
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    uefi::println!("");
    match option_env!("STORMBOOTX_BUILD") {
        Some(b) => uefi::println!(
            "tcp4probe {} ({b}) — is there a TCP/IP stack in this firmware?",
            env!("CARGO_PKG_VERSION")
        ),
        None => uefi::println!(
            "tcp4probe {} (unstamped) — is there a TCP/IP stack in this firmware?",
            env!("CARGO_PKG_VERSION")
        ),
    }
    uefi::println!("============================================================");
    handed_down();
    boot_fs();
    // What the RTC reads, as the next stage will read it (#77: stormbootx may
    // have set it from NTP).
    match uefi::runtime::get_time() {
        Ok(t) => uefi::println!(
            "rtc         : {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.year(),
            t.month(),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(e) => uefi::println!("rtc         : GetTime failed ({:?})", e.status()),
    }
    uefi::println!("as found:");
    survey();

    // Binding is only interesting when something is missing; when TCP4 is
    // already there, a full ConnectController pass is side effects for nothing.
    let presence = tcp4::ensure_available();
    if presence != tcp4::Presence::Present {
        uefi::println!("");
        uefi::println!("after ConnectController:");
        survey();
    }

    uefi::println!("");
    match presence {
        tcp4::Presence::Present => uefi::println!("verdict     : TCP4 was already bound"),
        tcp4::Presence::BoundOnDemand => uefi::println!(
            "verdict     : TCP4 appeared once the NIC handle was connected.\n\
             \x20             The drivers were in the image and nothing had asked for them."
        ),
        tcp4::Presence::BoundAfterFullPass => uefi::println!(
            "verdict     : TCP4 appeared after a pass over every handle."
        ),
        tcp4::Presence::BoundAfterWait(ms) => uefi::println!(
            "verdict     : TCP4 appeared only after waiting {ms} ms.\n\
             \x20             The platform was not ready when this ran — its NIC driver had\n\
             \x20             not been dispatched yet, or the stack binds asynchronously.\n\
             \x20             Whichever boot option runs first on this model pays for it."
        ),
        tcp4::Presence::Absent => {
            uefi::println!("verdict     : no TCP4. {}", tcp4::NO_TCP4_ADVICE);
            uefi::println!("============================================================");
            boot::stall(core::time::Duration::from_secs(30));
            return Status::ABORTED;
        }
    }

    // Presence is necessary and not sufficient: a child that cannot be
    // configured is a stack that exists and does not work, and stormbootx
    // would fail here rather than at the handle survey. Connecting to the
    // discard port asks the stack to do everything up to the SYN without
    // needing a service to answer — the interesting failures (NO_MAPPING, no
    // route) all happen before that.
    uefi::println!("");
    uefi::println!("creating and configuring a TCP4 child...");
    match tcp4::Tcp4Socket::connect([127, 0, 0, 1], 9) {
        Ok(_) => uefi::println!("  connected — the stack is fully usable"),
        Err(e) => uefi::println!(
            "  {e}\n  \
             (a refused or timed-out connect is fine and means the stack works;\n  \
             NO_MAPPING or a Configure failure is the stack not being usable.)"
        ),
    }

    uefi::println!("============================================================");
    boot::stall(core::time::Duration::from_secs(20));
    Status::SUCCESS
}
