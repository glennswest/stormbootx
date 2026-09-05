//! stormbootx — a UEFI NVMe/TCP boot extension.
//!
//! Boots a machine from an image that lives in sbregistry, with no kernel, no
//! initramfs and no local media beyond the binary itself. The sequence is:
//!
//!   service tag (SMBIOS)  ->  claim boothost/<tag>  ->  attach nvme-tcp://
//!     ->  publish EFI_BLOCK_IO_PROTOCOL  ->  firmware boots it
//!
//! *Which* image a machine boots is a fleet decision, and it lives next to the
//! images rather than on the media or in DHCP: a `boothost/<service tag>`
//! synonym on the storage engine. One request returns a copy-on-write clone of
//! the golden that machine is assigned *and* the address, NQN and NSID that
//! reach it. Moving a box to a new version is a PUT on its name.
//!
//! Once BlockIO is installed the firmware's own machinery does the rest: the
//! partition driver reads the GPT, the FAT driver mounts the ESP, and the boot
//! manager loads a bootloader from a disk that is not in this chassis.
//!
//! Identity is the **service tag**, not a MAC. NICs get swapped and added to,
//! and then a MAC-keyed boot server thinks it is looking at a different
//! machine. The service tag is the chassis, it needs no network to read, and
//! it is what is printed on the pull-out tab when someone has to find the box.
//!
//! Deliberately not used: EFI_HTTP (a driver stack firmware may not carry, when
//! one HTTP request over the TCP4 we already need is a hundred lines), and PXE
//! or TFTP anywhere at all.
//!
//! **Nothing in here is fatal.** Every failure — no service tag, no TCP stack,
//! no resolver, no portal, a target that refuses the connection — ends in the
//! same place: the firmware moves on to the local disk and the machine boots
//! what it already has. A boot path that needs the network in order to boot
//! *without* the network turns one provisioning outage into a fleet outage,
//! because every machine that reboots for any reason during it stays down. See
//! `fall_through`.

#![no_main]
#![no_std]

extern crate alloc;

mod blockio;
mod config;
mod dhcp4;
mod nvme;
mod registry;
mod shell;
mod sha256;
mod smbios;
mod tcp4;

use alloc::format;
use alloc::string::String;

use uefi::prelude::*;

/// Where sbregistry lives, for the claim path.
const REGISTRY_IP: [u8; 4] = [192, 168, 200, 22];
const REGISTRY_PORT: u16 = 5100;
const REGISTRY_HOST: &str = "sbregistry.gt.lo:5100";

/// The golden to claim when this machine has no clone yet.
const GOLDEN: &str = "stormcos-edge";

/// Attach a fixed target instead of asking the registry.
///
/// The registry claim is the model — a CoW clone per service tag, bound to the
/// machine that holds it. This exists because the two halves have to be proven
/// separately: a direct attach exercises SMBIOS, TCP4, the NVMe/TCP handshake
/// and BlockIO with nothing else in the path, so a failure here is a failure in
/// *this* binary rather than in a claim that returned the wrong thing. Set
/// `USE_REGISTRY` once the volume being served is a per-machine clone.
const USE_REGISTRY: bool = false;

/// The floor: what to attach when the config file says nothing.
///
/// `nqn` and `nsid` are the fallback for a claim that cannot be reached, not
/// the intended image — which image this machine boots is a `boothost/<service
/// tag>` synonym on the engine, and nothing on the media names it.
const DEFAULTS: config::Defaults = config::Defaults {
    portal: [192, 168, 31, 202], // forge.g16.lo, eth1 (MTU 9000)
    port: 4420,
    nqn: "nqn.2026-09.lo.g16:stormcos",
    nsid: 2, // drives[1] = stormcos-sno-10.21.img
    api_port: 9090, // the engine API on the same host as the portal
};

fn banner(line: &str) {
    uefi::println!("{line}");
}

fn run() -> Result<(), String> {
    banner("");
    // Identify the build, always. Four separate boots during hardware bring-up
    // were spent reading output from three *different* stale sticks in the same
    // machine, each looking plausible, because nothing on the console said
    // which binary was talking. A version and a commit cost one line.
    match option_env!("STORMBOOTX_BUILD") {
        Some(b) => uefi::println!(
            "stormbootx {} ({b}) — NVMe/TCP boot extension",
            env!("CARGO_PKG_VERSION")
        ),
        None => uefi::println!(
            "stormbootx {} (unstamped build) — NVMe/TCP boot extension",
            env!("CARGO_PKG_VERSION")
        ),
    }
    banner("============================================================");

    // 1. Who am I? No network, no configuration, no BMC.
    let tag = smbios::service_tag().ok_or("SMBIOS carries no system serial number")?;
    uefi::println!("service tag : {tag}");
    if let Some(model) = smbios::model() {
        // Printed because whether a platform carries the TCP/IP driver stack is
        // a per-model fact, not a per-machine one. A console line naming the
        // model is what makes that something to write down once.
        uefi::println!("model       : {model}");
    }

    // 2. Is there a usable TCP stack? Presence of SNP is not enough — the
    //    layered IP4/TCP4 drivers are a separate build option in firmware, and
    //    even when they are built in nothing may have bound them yet.
    match tcp4::ensure_available() {
        tcp4::Presence::Present => uefi::println!("tcp4        : available"),
        tcp4::Presence::BoundOnDemand => {
            uefi::println!("tcp4        : available (bound on demand from the NIC handle)")
        }
        tcp4::Presence::BoundAfterFullPass => {
            uefi::println!("tcp4        : available (bound after a full ConnectController pass)")
        }
        tcp4::Presence::BoundAfterWait(ms) => uefi::println!(
            "tcp4        : available (appeared after {ms} ms — the platform was not ready)"
        ),
        tcp4::Presence::Absent => return Err(tcp4::NO_TCP4_ADVICE.into()),
    }

    // 3. What should I boot?
    let attach = if USE_REGISTRY {
        // Reuse a clone this machine already holds, so a reboot reattaches the
        // same volume rather than minting another.
        uefi::println!("registry    : {REGISTRY_HOST}");
        match registry::existing(REGISTRY_IP, REGISTRY_PORT, REGISTRY_HOST, &tag)? {
            Some(a) => {
                uefi::println!("  reattaching the clone already bound to {tag}");
                a
            }
            None => {
                uefi::println!("  no clone for {tag}; claiming from golden {GOLDEN}");
                registry::claim(REGISTRY_IP, REGISTRY_PORT, REGISTRY_HOST, GOLDEN, &tag)?
            }
        }
    } else {
        // Where to attach: the config file, else the compiled floor. Nothing
        // on the network is asked for this — the portal is an appliance
        // address, and the question worth asking is answered below.
        let cfg = config::resolve(&DEFAULTS);
        uefi::println!("target      : {}", cfg.source);

        // Resolution says *where*; the claim says *which*. Which image this
        // machine runs is a fleet decision that lives next to the images, as a
        // `boothost/<service tag>` synonym — so moving this box to a new
        // version is a PUT on its name rather than a visit to the machine, and
        // nothing on the media has to change. The engine's API is the same host
        // as the portal: one serves the bytes, the other says which bytes.
        //
        // Falling back rather than failing is the whole rule here. A machine
        // with no synonym yet, or an engine that is down, still boots what
        // resolution produced — an image nobody has assigned beats no image.
        let claimed = if cfg.claim {
            let [a, b, c, d] = cfg.portal;
            let host = format!("{a}.{b}.{c}.{d}:{}", cfg.api_port);
            uefi::println!("claim       : {}/{tag} at {host}", registry::BOOTHOST_NS);
            match registry::claim_boothost(cfg.portal, cfg.api_port, &host, &tag) {
                Ok(a) => {
                    uefi::println!("  claimed a clone of this machine's image");
                    Some(a)
                }
                Err(e) => {
                    uefi::println!("  {e}");
                    uefi::println!("  falling back to the resolved target");
                    None
                }
            }
        } else {
            uefi::println!("claim       : disabled by the config file");
            None
        };

        claimed.unwrap_or(registry::Attach {
            address: cfg.portal,
            port: cfg.port,
            nqn: cfg.nqn,
            nsid: cfg.nsid,
        })
    };

    let [a, b, c, d] = attach.address;
    uefi::println!(
        "  portal    : {a}.{b}.{c}.{d}:{}  nsid {}",
        attach.port,
        attach.nsid
    );
    uefi::println!("  nqn       : {}", attach.nqn);

    // 4. Attach. The host NQN is derived from the service tag so the target
    //    sees a stable initiator identity across reboots.
    let hostnqn = format!("nqn.2026-09.lo.storm:host-{tag}");
    uefi::println!("attaching   : {hostnqn}");

    let ns = nvme::Namespace::attach(
        attach.address,
        attach.port,
        &attach.nqn,
        attach.nsid,
        &hostnqn,
    )?;

    let g = ns.geometry;
    let gib = (g.blocks.saturating_mul(g.block_size as u64)) / (1024 * 1024 * 1024);
    uefi::println!(
        "  namespace : {} blocks x {} bytes  ({gib} GiB)",
        g.blocks,
        g.block_size
    );
    // Say which transfer size was chosen and where the number came from. This
    // used to be a constant edited by hand to match the network, and then a
    // number derived from the MTU that got it backwards on a jumbo path, so
    // the console has to show the input as well as the answer.
    let source = if ns.mdts == 0 {
        String::from("controller stated no MDTS")
    } else {
        format!("controller MDTS {}", ns.mdts)
    };
    let path = match ns.mtu {
        Some(mtu) if mtu >= 9000 => format!("path MTU {mtu}, jumbo"),
        Some(mtu) => format!("path MTU {mtu}"),
        None => String::from("the stack reported no MTU"),
    };
    uefi::println!(
        "  transfer  : {} KiB per command  ({source}; {path})",
        ns.max_transfer / 1024
    );

    // 5. Publish it as an ordinary disk, then boot it.
    let handle = blockio::publish(ns)?;
    uefi::println!("blockio     : published on handle {handle:p}");

    // Do not hand back to the firmware boot manager and hope it boots the new
    // disk — it will not, because the disk is not in BootOrder, and the machine
    // drops to setup instead. Chain-load the image's own bootloader. This does
    // not return unless the bootloader fails or exits.
    banner("");
    banner("RESULT: image attached; starting its bootloader.");
    banner("============================================================");
    blockio::boot_attached(handle)?;
    Err(String::from("the attached image did not boot; nothing to chain-load"))
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    // run() hands off to the image's bootloader and does not return on
    // success; every path back here is a failure to fall through from.
    match run() {
        Ok(()) => fall_through("attach succeeded but no bootloader started"),
        Err(err) => fall_through(&err),
    }
}

/// Nothing here is fatal. Give up on the network and let the machine boot
/// itself.
///
/// This is the policy, not an error handler: **a boot path must never need the
/// network in order to boot without it.** An agent that stops when the portal
/// is unreachable turns one provisioning outage into a fleet outage — every
/// machine that reboots for any reason during it stays down, and the blast
/// radius of a maintenance window on one server becomes the whole estate.
/// Falling through costs a machine one stale boot; stopping costs the fleet.
///
/// `ABORTED` rather than `SUCCESS` because it is the conventional signal to
/// the boot manager that this option did not boot anything and the next one
/// should be tried. It is the fall-through, not a complaint.
fn fall_through(err: &str) -> Status {
    let disks = blockio::local_disks();

    uefi::println!("");
    uefi::println!("no network boot: {err}");

    // Offer a console before falling through. Firmware is the worst place to
    // debug blind, and every question worth asking here — what NICs are there,
    // what address do they hold, can this machine reach that host — needs a
    // machine that has already failed, which is exactly this moment.
    //
    // Offered on a timer and never forced: a machine that reboots unattended
    // must not stop at a prompt because nobody was watching. Silence takes the
    // path it would have taken anyway.
    if shell::offer(5) {
        shell::run();
    }

    if disks > 0 {
        // Short. This runs on every reboot while a portal is down, and a boot
        // path that adds half a minute to each of them is its own outage.
        uefi::println!(
            "RESULT: falling through to the local disk ({disks} found). \
             The machine boots what it already has."
        );
    } else {
        // Nothing to fall through to, so there is time to read this and it is
        // the one case where a human is definitely needed.
        uefi::println!(
            "RESULT: nothing to fall through to — this machine has no local disk \
             and could not reach a portal."
        );
        uefi::boot::stall(core::time::Duration::from_secs(30));
    }
    uefi::println!("============================================================");
    Status::ABORTED
}
