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
// survey and one connect, so most of it is dead here by design.
#[path = "tcp4.rs"]
#[allow(dead_code)]
mod tcp4;

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

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    uefi::println!("");
    uefi::println!("tcp4probe — is there a TCP/IP stack in this firmware?");
    uefi::println!("============================================================");
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
