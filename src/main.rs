//! stormbootx — a UEFI NVMe/TCP boot extension.
//!
//! Boots a machine from an image that lives on the storage engine (stormblock,
//! forge), with no kernel, no initramfs and no local media beyond the binary
//! itself. The sequence is:
//!
//!   identity (conf or SMBIOS, then MAC)  ->  boot intent  ->  claim  ->  attach nvme-tcp://
//!     ->  publish EFI_BLOCK_IO_PROTOCOL  ->  chain-load its BOOTX64.EFI
//!
//! The claim is `boothost/<tag>` for a tag the media states, and otherwise
//! `boothost/default` carrying the machine's MAC (#15, `universal.rs`): one
//! boot medium for every machine, each getting a clone of its own. An engine
//! too old for that is claimed by SMBIOS serial, as before.
//!
//! *Which* image a machine boots is a fleet decision, and it lives next to the
//! images rather than on the media or in DHCP: a `boothost/<service tag>`
//! synonym on the storage engine. An intent beside it (`install`, `local` or
//! `auto`, see `intent.rs`) is read first, and `local` falls through to the
//! disk without claiming anything. One request returns a copy-on-write clone of
//! the golden that machine is assigned *and* the address, NQN and NSID that
//! reach it. Moving a box to a new version is a PUT on its name.
//!
//! Once BlockIO is installed the firmware's own drivers read the GPT and mount
//! the ESP, and this binary then loads `\EFI\BOOT\BOOTX64.EFI` from that ESP
//! and starts it — on a stormcos image, stormuefi. It does not hand back to
//! the boot manager: a disk that appears mid-boot-option is not in
//! `BootOrder`, so the manager would never boot it (see `blockio::boot_attached`).
//!
//! Identity used to be the **service tag** alone, on the reasoning that NICs
//! get swapped and the service tag is the chassis. Serials turned out not to
//! be unique (seven Supermicro MicroCloud nodes share one), so a machine
//! nothing has named is now known to the engine by its MAC until it is given
//! a name there; the engine keeps the MAC and a unique serial as aliases of
//! that name (stormblock#199/#200), which is what survives a NIC swap.
//!
//! Deliberately not used: EFI_HTTP (a driver stack firmware may not carry, when
//! one HTTP request over the TCP4 we already need is a hundred lines), and PXE
//! or TFTP anywhere at all.
//!
//! **Nothing in here is fatal.** Every failure — no service tag, no TCP stack,
//! no engine, no portal, a target that refuses the connection — ends in the
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
mod dnsname;
mod drivers;
mod esp;
mod espboot;
mod intent;
mod mlxfec;
mod nvme;
mod registry;
mod shell;
mod sha256;
mod smbios;
mod tcp4;
mod universal;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use uefi::prelude::*;

/// Where sbregistry lives, for the old `/v1/clones/claim` path. Compiled out
/// by `USE_REGISTRY = false`; the live path is the boothost claim below.
const REGISTRY_IP: [u8; 4] = [192, 168, 200, 22];
const REGISTRY_PORT: u16 = 5100;
const REGISTRY_HOST: &str = "sbregistry.gt.lo:5100";

/// The golden to claim when this machine has no clone yet.
const GOLDEN: &str = "stormcos-edge";

/// Take the old sbregistry path instead of the engine's boothost claim.
///
/// Off. The engine's `boothost/<tag>` claim, which mints a per-machine clone,
/// replaced it, and is what runs when this is `false` (step 3). Kept because
/// the code is small and is the only record of the sbregistry contract.
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
    // A stated tag wins. Discovery is a convenience for a machine nobody has
    // told; one that has been told should not have its answer second-guessed,
    // and stating one is the only way to bench-test a box as another host or
    // to name a board whose SMBIOS serial is a placeholder shared by every
    // board of its model.
    //
    // No stated tag and no usable serial is no longer the end: the MAC, read
    // once the network stack exists (step 2a), identifies the machine.
    let stated = config::stated_tag();
    let serial = match &stated {
        Some(t) => {
            uefi::println!("service tag : {t}  (stated in {})", config::CONF_PATH);
            None
        }
        None => match smbios::identity(None) {
            Some(id) => {
                uefi::println!("service tag : {}  ({})", id.value(), id.source());
                Some(id.value().to_string())
            }
            None => {
                uefi::println!(
                    "service tag : none  (no usable SMBIOS serial; the MAC will identify it)"
                );
                None
            }
        },
    };
    if let Some(model) = smbios::model() {
        // Printed because whether a platform carries the TCP/IP driver stack is
        // a per-model fact, not a per-machine one. A console line naming the
        // model is what makes that something to write down once.
        uefi::println!("model       : {model}");
    }

    // 1b. Read and report the NIC's 25G FEC, before the network is used. This
    //     is a diagnostic, not a fix: on 2026-09-06 the flaky-SR-link problem
    //     turned out to be switch-side — the fabric (Dell S5148F) was pinned to
    //     `fec off`, and the ConnectX's device-default already *negotiates*
    //     cl108-rs, so setting the switch to `CL108-RS` matched both ends and
    //     the links (including a port dead for days) came up stable. The card
    //     needed no change. The write path exists in `mlxfec` and works, but
    //     nothing on the boot path calls it: `apply(None)` reads and reports,
    //     and that is all this does. See step 2b for why the one thing that
    //     did write was switched off. Nothing here is fatal; an unrecognised
    //     card is named and skipped.
    uefi::println!("nic fec     :");
    let _ = mlxfec::apply(None);

    //     A recovery stick, and only a recovery stick, may state `fec = MODE`
    //     in stormboot.conf. That is an operator saying so on one piece of
    //     media, not this code inferring it from link state — the distinction
    //     that matters, and the one step 2b is about. Write once, warm-reset
    //     once, and the next boot of the same stick finds nothing to do.
    if let Some(want) = config::stated_fec() {
        uefi::println!("              media states fec = {} — writing it", want.name());
        if mlxfec::apply(Some(want)).reset_needed() {
            uefi::println!("              FEC written; warm-resetting to apply");
            uefi::runtime::reset(uefi::runtime::ResetType::WARM, Status::SUCCESS, None);
        }
        uefi::println!("              already {} — nothing written", want.name());
    }

    // 1c. NIC drivers the media carries (#26). A platform can have the whole
    //     upper stack and no UEFI driver for its own NICs (the Supermicro X9
    //     blades), and then there is no SNP for TCP4 to sit on. The platform's
    //     drivers bind first; a media driver only takes a NIC nothing else did.
    let loaded = drivers::load_from_media();
    if !loaded.is_empty() {
        let ok = loaded.iter().filter(|l| l.result.is_ok()).count();
        uefi::println!("drivers     : {ok} of {} started from {}", loaded.len(), drivers::DRIVERS_DIR);
        for l in loaded.iter().filter(|l| l.result.is_err()) {
            uefi::println!("    not started: {}", l.name);
        }
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

    // 2a. The machine's MAC: its identity to the engine when the media names
    //     none (#15). Read now because the NICs a platform left unbound only
    //     exist after `ensure_available`.
    let mac = tcp4::machine_mac();
    let mac_colon = mac.map(|(m, _)| String::from_utf8_lossy(&universal::mac_colon(&m)).into_owned());
    match (&mac_colon, mac) {
        (Some(c), Some((_, n))) => uefi::println!("mac         : {c}  (lowest of {n} NIC(s))"),
        _ => uefi::println!("mac         : none usable"),
    }

    // What an older engine is asked for, and what the host NQN falls back to:
    // the stated tag, else the SMBIOS serial, else the MAC (#7's floor).
    let tag = match (&stated, &serial) {
        (Some(t), _) | (None, Some(t)) => t.clone(),
        (None, None) => smbios::identity(mac_colon.as_deref())
            .map(|id| id.value().to_string())
            .ok_or(
                "no usable identity: no stated tag, the SMBIOS system, baseboard and chassis \
                 serials are all empty or a shared placeholder, and no NIC has a usable MAC. \
                 State one with `tag = <id>` in \\stormboot\\stormboot.conf",
            )?,
    };

    // 2b. The FEC self-heal used to run here, and is switched off (2026-09-07).
    //
    //     It fired when every 25G ConnectX port read link-down, pinned the
    //     card's NV FEC override to RS and warm-reset once. The trigger is
    //     wrong, and the machine it was written for is what proved it: on
    //     2026-09-07 the R230's two 25G ports (dsw1 1/1/5 and 1/1/7) dropped
    //     together at 16:22 UTC and never came back — after 16 h 51 m of
    //     continuous link on a fabric already correctly set to `fec CL108-RS`.
    //     The card stayed powered, one port lasing without PCS lock and the
    //     other dark, which is a card-configuration state and not a fabric one.
    //
    //     The trigger reads link exactly once, through `snp.media_present`, with
    //     no settle wait — the same trap `ensure_available` already documents
    //     and already retries around, because a 25G RS link takes seconds to
    //     come up after a reset and *time was the answer*. So a healthy card,
    //     sampled early enough in UEFI, reads all-down; the write then happens
    //     for no reason and the warm reset re-enters UEFI early enough to do it
    //     again. It self-limits — the write is skipped once the FEC is already
    //     RS — which is why this cost one bad write rather than a boot loop.
    //
    //     There is nothing for it to fix in the first place: the ConnectX-4 Lx
    //     device-default *negotiates* cl108-rs, so a correct fabric matches it
    //     with no card-side change at all. The failure this was built for was
    //     switch-side (`fec off` on all 48 SFP28 ports) and was fixed on the
    //     switch. A boot-time NV write to the NIC is a large hammer for a
    //     problem the card does not have.
    //
    //     `mlxfec` keeps the write path — it works, it is hard-won, and #7's
    //     history is in it. Reading is still done, above. If this is ever
    //     revived it needs a settle wait before it believes "all down", and a
    //     reason to prefer pinning over the device default.

    // 3. What should I boot?
    // The DNS name the network gives this machine (#23): full name, the host
    // label the engine knows it by, and where it came from. Set in step 3b.
    let mut dns: Option<(String, String, &'static str)> = None;

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

            // The engine's version first. It is the first request of the boot,
            // so it is also what brings the network up, and it says which
            // interface reached the engine: the one whose lease names the
            // machine (#23). Universal boot (#15) needs an engine that gives
            // each machine claiming the default a clone of its own; one from
            // before stormblock#200 would give them all one, so anything but
            // a version after 19.3.0 is "no".
            let (version, iface) = registry::engine_version(cfg.portal, cfg.api_port, &host);
            let default_mac = if stated.is_none() { mac.map(|(m, _)| m).zip(mac_colon.clone()) } else { None };
            let universal = match &version {
                Ok(v) if universal::supports_default_claim(v) => {
                    uefi::println!("engine      : stormblock {v}  (universal boot)");
                    default_mac.is_some()
                }
                Ok(v) => {
                    uefi::println!("engine      : stormblock {v}  (before universal boot)");
                    false
                }
                Err(e) => {
                    uefi::println!("engine      : version unknown ({e})");
                    false
                }
            };

            // 3b. The machine's DNS name, when the media states none (#23):
            //     DHCP option 12 on the interface that reached the engine,
            //     else the PTR of its address. The engine knows hosts by the
            //     first label. No name is not a failure; the MAC and the
            //     serial are still there.
            if stated.is_none() {
                if iface.is_none() {
                    uefi::println!("name        : the engine's interface is unknown, so no lease to read");
                }
                dns = iface.and_then(|i| network_name(&i));
                match &dns {
                    Some((full, _, from)) => uefi::println!("name        : {full}  (from {from})"),
                    None => uefi::println!("name        : none from DHCP or reverse DNS"),
                }
            }
            let dns_host = dns.as_ref().map(|(_, h, _)| h.clone());

            // The names the intent is read under, in the order the claim
            // below tries them: a stated tag alone; else the DNS name, the MAC
            // as twelve hex digits (which the engine resolves as an alias to
            // whatever the machine is called now), then the tag when the claim
            // would reach it. The engine resolves every one through the same
            // host table, so the first that is not a 404 is the machine's: a
            // DNS name the engine does not know yet (stormblock#204) must not
            // hide a `local` set on the host it knows by MAC.
            let mut intent_keys: Vec<String> = Vec::new();
            if stated.is_some() {
                intent_keys.push(tag.clone());
            } else {
                intent_keys.extend(dns_host.clone());
                let by_mac = default_mac.as_ref().filter(|_| universal);
                if let Some((m, _)) = by_mac {
                    intent_keys.push(
                        String::from_utf8_lossy(&universal::provisional_name(m)[4..]).into_owned(),
                    );
                }
                if by_mac.is_none() || serial.is_some() {
                    intent_keys.push(tag.clone());
                }
            }
            let mut seen: Vec<String> = Vec::new();
            intent_keys.retain(|k| {
                let new = !seen.iter().any(|s| s.eq_ignore_ascii_case(k));
                seen.push(k.clone());
                new
            });

            // 3a. What has this machine been told to do? Read before the
            //     claim, because the claim mints a clone and `local` is there so
            //     that nothing is minted. Every doubt reads as `auto`, which is
            //     what every boot did before intents existed (see intent.rs).
            //     Only a 404 moves on to the next name; any other answer is
            //     this machine's.
            let (intent_key, reply) = {
                let last = intent_keys.len().saturating_sub(1);
                let mut found = None;
                for (i, key) in intent_keys.iter().enumerate() {
                    let r = registry::boot_intent(cfg.portal, cfg.api_port, &host, key);
                    let not_found = matches!(&r, Ok((s, b)) if intent::from_reply(*s, b) == intent::Reply::NotFound);
                    if not_found && i < last {
                        uefi::println!("intent      : none under {key}");
                        continue;
                    }
                    found = Some((key.clone(), r));
                    break;
                }
                found.unwrap_or_else(|| (tag.clone(), Err("no name to ask under".to_string())))
            };
            let said = match &reply {
                Ok((status, body)) => intent::from_reply(*status, body),
                Err(_) => intent::Reply::Status(0),
            };
            let chosen = said.intent();
            match (&reply, said) {
                (_, intent::Reply::Stated(i)) => {
                    uefi::println!("intent      : {}", i.name())
                }
                (_, intent::Reply::NotFound) => uefi::println!(
                    "intent      : auto  (the engine has none for {intent_key}, or no intent route)"
                ),
                (Err(e), _) => {
                    uefi::println!("intent      : auto  (could not ask the engine: {e})")
                }
                (_, intent::Reply::Status(code)) => {
                    uefi::println!("intent      : auto  (the engine answered HTTP {code})")
                }
                (_, intent::Reply::Unreadable(Some(v))) => {
                    uefi::println!("intent      : auto  (unrecognised intent \"{v}\")")
                }
                (_, intent::Reply::Unreadable(None)) => {
                    uefi::println!("intent      : auto  (the reply stated no intent)")
                }
            }
            if !chosen.claims() {
                return Err(format!(
                    "boot intent for {intent_key} is `{}`: nothing claimed",
                    chosen.name()
                ));
            }

            // 3c. Claim, best name first. A 404 means "not under this name",
            //     so the next one is tried; anything else falls back to the
            //     resolved target rather than guessing at another identity.
            let claim_as = |name: &str, hints: bool| {
                uefi::println!("claim       : {}/{name} at {host}", registry::BOOTHOST_NS);
                let (m, s) = if hints { (mac_colon.as_deref(), serial.as_deref()) } else { (None, None) };
                registry::claim_boothost(cfg.portal, cfg.api_port, &host, name, m, s)
            };
            let claimed_ok = |a: registry::Attach| {
                match &a.host {
                    Some(h) => uefi::println!("  claimed a clone of {h}'s image"),
                    None => uefi::println!("  claimed a clone of this machine's image"),
                }
                Some(a)
            };
            let give_up = |e: &str| -> Option<registry::Attach> {
                uefi::println!("  {e}");
                uefi::println!("  falling back to the resolved target");
                None
            };
            let by_tag = || match claim_as(&tag, false) {
                Ok(a) => claimed_ok(a),
                Err((_, e)) => give_up(&e),
            };

            // A stated tag is the answer, and the only one tried.
            if stated.is_some() {
                by_tag()
            } else {
                let by_name = match &dns_host {
                    Some(h) => match claim_as(h, true) {
                        Ok(a) => Ok(claimed_ok(a)),
                        Err((404, e)) => {
                            uefi::println!("  {e}");
                            Err(())
                        }
                        Err((_, e)) => Ok(give_up(&e)),
                    },
                    None => Err(()),
                };
                match by_name {
                    Ok(done) => done,
                    Err(()) => match default_mac.as_ref().filter(|_| universal) {
                        Some((m, colon)) => {
                            let provisional = universal::provisional_name(m);
                            let provisional = core::str::from_utf8(&provisional).unwrap_or("mac-?");
                            uefi::println!(
                                "claim       : {}/default as {colon} at {host}",
                                registry::BOOTHOST_NS
                            );
                            match registry::claim_default(
                                cfg.portal,
                                cfg.api_port,
                                &host,
                                colon,
                                serial.as_deref(),
                            ) {
                                Ok(a) => {
                                    match (&a.host, a.provisional) {
                                        (Some(h), false) => uefi::println!(
                                            "  booting {h}'s image (this MAC is its alias)"
                                        ),
                                        (Some(h), true) => {
                                            uefi::println!("  booting the default image as {h}")
                                        }
                                        (None, _) => uefi::println!(
                                            "  booting the default image as {provisional}"
                                        ),
                                    }
                                    Some(a)
                                }
                                // No default to give: a machine the engine
                                // already knows by its serial can still boot
                                // that, and with no default the engine cannot
                                // mint a serial host.
                                Err((404, e)) if serial.is_some() => {
                                    uefi::println!("  {e}");
                                    by_tag()
                                }
                                Err((_, e)) => give_up(&e),
                            }
                        }
                        None => by_tag(),
                    },
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
            host: None,
            provisional: false,
        })
    };

    let [a, b, c, d] = attach.address;
    uefi::println!(
        "  portal    : {a}.{b}.{c}.{d}:{}  nsid {}",
        attach.port,
        attach.nsid
    );
    uefi::println!("  nqn       : {}", attach.nqn);

    // 4. Attach. The host NQN is derived from the machine's name so the target
    //    sees a stable initiator identity across reboots: the engine's name
    //    for it when the claim said one, else the tag. Not the serial for a
    //    machine booted as `mac-<hex>`, which may be the chassis serial seven
    //    other nodes share.
    let hostnqn = format!(
        "nqn.2026-09.lo.storm:host-{}",
        attach
            .host
            .as_deref()
            .or(dns.as_ref().map(|(_, h, _)| h.as_str()))
            .unwrap_or(&tag)
    );
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
    let reader = config::esp_reader();
    if reader != config::EspReader::Auto {
        uefi::println!("esp         : {reader:?} only (esp = in {})", config::CONF_PATH);
    }
    blockio::boot_attached(handle, reader)?;
    Err(String::from("the attached image did not boot; nothing to chain-load"))
}

/// The machine's DNS name from the network it booted on (#23): DHCP option 12
/// on this interface's lease (with option 15 for the domain), else the PTR of
/// the interface's address asked of the option 6 DNS server, or of `dns =` in
/// `stormboot.conf`. Returns the full name, the machine the engine knows it
/// by, and the source.
///
/// Each way this comes up empty says so on the console: on server1 it printed
/// only `none`, and which of four things had gone wrong took a reading of
/// microdns's source to find (#26). microdns sends no option 12 at all; it
/// puts the reservation's name in DNS, so the PTR is the path that matters.
///
/// A reservation names the NIC (`server1a`); the machine is `server1`
/// (`dnsname::machine_label`). The full name printed is the NIC's, as DNS
/// has it.
fn network_name(iface: &registry::Interface) -> Option<(String, String, &'static str)> {
    let (mac, mac_len, addr) = iface;
    let reply = dhcp4::reply_for(&mac[..], *mac_len);
    let msg: &[u8] = reply.as_deref().unwrap_or(&[]);
    let machine = |full: &str| -> Option<String> {
        let nic = dnsname::host_label(full)?;
        let m = dnsname::machine_label(nic);
        if m != nic {
            uefi::println!("name        : {nic} names this NIC; the machine is {m}");
        }
        Some(m.to_string())
    };
    if let Some(name) = dnsname::dhcp_option(msg, dnsname::OPT_HOST_NAME).and_then(dnsname::text) {
        match machine(name) {
            Some(host) => {
                let domain = dnsname::dhcp_option(msg, dnsname::OPT_DOMAIN)
                    .and_then(dnsname::text)
                    .filter(|d| dnsname::valid_name(d));
                let full = match domain {
                    Some(d) if !name.contains('.') => format!("{name}.{d}"),
                    _ => name.to_string(),
                };
                return Some((full, host, "DHCP"));
            }
            None => uefi::println!("name        : DHCP host name {name:?} is not a DNS name; ignored"),
        }
    }
    let from_lease = dnsname::dhcp_option(msg, dnsname::OPT_DNS).and_then(dnsname::first_dns);
    let server = match (from_lease, config::stated_dns()) {
        (Some(s), _) => s,
        (None, Some(s)) => {
            uefi::println!(
                "name        : {}; asking the dns = server",
                if reply.is_none() { "no DHCP reply readable" } else { "the DHCP reply names no DNS server" }
            );
            s
        }
        (None, None) => {
            uefi::println!(
                "name        : {}, and {} states no dns =",
                if reply.is_none() {
                    "no DHCP reply readable on this interface"
                } else {
                    "no host name and no DNS server in the DHCP reply"
                },
                config::CONF_PATH
            );
            return None;
        }
    };
    let [a, b, c, d] = *addr;
    let [s0, s1, s2, s3] = server;
    let full = match ptr_lookup(server, *addr) {
        Ok(f) => f,
        Err(e) => {
            uefi::println!("name        : PTR of {a}.{b}.{c}.{d} at {s0}.{s1}.{s2}.{s3}: {e}");
            return None;
        }
    };
    let host = machine(&full)?;
    Some((full, host, "reverse DNS"))
}

/// One PTR query over DNS/TCP. Any failure is "no name", never a failed
/// boot: the machine still has its MAC. The reason is returned for the
/// console.
fn ptr_lookup(server: [u8; 4], addr: [u8; 4]) -> Result<String, String> {
    let id = u16::from_be_bytes([addr[2] ^ 0x5b, addr[3]]);
    let mut query = [0u8; 64];
    let n = dnsname::ptr_query(addr, id, &mut query);
    let mut sock = tcp4::Tcp4Socket::connect_within(server, 53, 5)?;
    sock.send(&query[..n])?;
    let len = sock.read_exact(2)?;
    let msg = sock.read_exact(u16::from_be_bytes([len[0], len[1]]) as usize)?;
    let mut out = [0u8; 256];
    let k = dnsname::ptr_answer(&msg, id, &mut out).ok_or("no PTR record in the answer")?;
    core::str::from_utf8(&out[..k])
        .map(String::from)
        .map_err(|_| "the PTR answer is not text".to_string())
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
             and nothing booted from the network."
        );
        uefi::boot::stall(core::time::Duration::from_secs(30));
    }
    uefi::println!("============================================================");
    Status::ABORTED
}
