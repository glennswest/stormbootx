//! The boot medium updates itself from the current release (#83).
//!
//! A stick written once must not stay on that version forever (the X9 blades
//! boot from USB sticks because BMC virtual media is unreliable). So on every
//! boot, once the network is up and before the claim, stormbootx asks
//! stormcentral for the current release of its golden (`update =` in
//! `stormboot.conf`) and, if it is newer, rewrites its own medium. The design
//! is the owner's (2026-10-02, on #83); stormcentral's half is #279.
//!
//! - **Authentic.** The manifest (`manifest.rs`) is signed with Ed25519 by
//!   stormcentral when it promotes a golden, with a key that never leaves the
//!   stormcentral VM. The signature is checked against the public keys
//!   compiled in here **before anything in the manifest is read**, then every
//!   file's size and SHA-256 against the manifest before it is written. Plain
//!   HTTP is fine for that: whoever answers can only serve what was signed.
//!   Verify-only Ed25519 is `ed25519-compact`, not crypto written here.
//! - **Never backwards.** A manifest's `serial` must be above every serial
//!   this medium has carried, at or above the highest one this machine has
//!   passed a trial on (`StormBootMinSerial`, the one non-volatile variable
//!   stormbootx writes), and above any that failed its trial here. A deliberate
//!   rollback is a new serial.
//! - **Good, or put back.** New files are written as `*.new`, then each
//!   current file becomes `*.prev` and the new one takes its name, the
//!   bootloader last; a driver the release no longer carries becomes `*.prev`
//!   too. `\stormboot\state` records the trial, and the machine restarts into
//!   the new set. The new binary counts its starts. It passes its trial once
//!   it has attached an image (or the engine said `local`). A set that has
//!   started twice without that is put back: the `*.prev` files return, the
//!   serial is marked failed on this medium, and the previous binary is
//!   chain-loaded.
//! - **Told.** Linux gets `StormBootUpdate` (`handoff.rs`): `serial:<n>`,
//!   `trial:<n>:<start>`, `good:<n>`, or `failed:<n>`.
//! - **Only where it can.** A read-only medium (an ISO on virtual media) is
//!   skipped with one line, `update = off` pins a medium, and no `update =`
//!   at all (media built by hand) checks nothing. A failure anywhere before
//!   the swap leaves the medium as it was; nothing here stops a boot.
//!
//! `local.conf` is the medium's own: an update never writes it, and carries a
//! `name =`/`tag =` there when the release's `stormboot.conf` would drop it.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use uefi::boot::{self, LoadImageSource, ScopedProtocol};
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::proto::media::file::{
    Directory, File, FileAttribute, FileInfo, FileMode, FileSystemInfo, FileType, RegularFile,
};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::BootPolicy;
use uefi::runtime::VariableAttributes;
use uefi::{cstr16, CStr16, Handle, Status};

use crate::manifest::{self, Entry, Manifest, Setting, Start, State, Verdict};
use crate::{clock, config, handoff, net, sha256};

/// The medium's update state.
pub const STATE_PATH: &str = r"\stormboot\state";

/// The bootloader, which is this binary.
const BOOT_PATH: &str = r"\EFI\BOOT\BOOTX64.EFI";

/// stormcentral's release keys, current and next (stormcentral#279). Empty
/// until stormcentral has made its key: then nothing verifies, and nothing
/// updates.
const RELEASE_KEYS: &[[u8; 32]] = &[];

/// The serial on trial in this boot, 0 for none.
static TRIAL: AtomicU64 = AtomicU64::new(0);

/// A key the OVMF test signs with (`tests/update-ovmf.sh`), compiled in only
/// when the build sets `STORMBOOTX_UPDATE_TEST_KEY`. No golden build does.
fn test_key() -> Option<[u8; 32]> {
    option_env!("STORMBOOTX_UPDATE_TEST_KEY").and_then(manifest::hex32)
}

fn keys() -> Vec<[u8; 32]> {
    let mut k = RELEASE_KEYS.to_vec();
    k.extend(test_key());
    k
}

/// A manifest path as an EFI path: `\EFI\BOOT\BOOTX64.EFI`.
fn efi_path(p: &str) -> String {
    format!("\\{}", p.replace('/', "\\"))
}

fn kb(n: u64) -> u64 {
    n.div_ceil(1024)
}

// ------------------------------------------------------------------- start

/// The first thing a boot does: count a trial's start, or put a set that
/// failed its trial back and chain-load the binary it restored. `Some` is the
/// status to return with (the chain-loaded binary's).
pub fn at_start() -> Option<Status> {
    let mut vol = Vol::mount().ok()?;
    let text = vol.read_text(STATE_PATH).ok()??;
    let state = State::parse(&text);
    match manifest::at_start(&state) {
        Start::Normal => {
            if state.serial > 0 {
                handoff::set_update(&format!("serial:{}", state.serial), true);
            }
            None
        }
        Start::Trial { serial, start } => {
            let next = State { trial: Some((serial, start)), ..state };
            match vol.write_state(&next) {
                Ok(()) => uefi::println!(
                    "update      : serial {serial} on trial, start {start} of {}; it must reach an attach",
                    manifest::TRIAL_STARTS
                ),
                Err(e) => uefi::println!("update      : serial {serial} on trial, and this start was not recorded ({e})"),
            }
            handoff::set_update(&format!("trial:{serial}:{start}"), false);
            TRIAL.store(serial, Ordering::Relaxed);
            None
        }
        Start::Revert { serial, back_to } => {
            uefi::println!(
                "update      : serial {serial} started {} times and never attached; putting serial {back_to} back",
                manifest::TRIAL_STARTS
            );
            let (done, failed) = revert(&mut vol, &state);
            for e in &failed {
                uefi::println!("              {e}");
            }
            let after = State {
                serial: back_to,
                failed: state.failed.max(serial),
                trial: None,
                prev: 0,
                files: "",
                ..state
            };
            if let Err(e) = vol.write_state(&after) {
                uefi::println!("              state not written ({e})");
            }
            drop(vol);
            uefi::println!("update      : {done} file(s) put back; starting the restored {BOOT_PATH}");
            handoff::set_update(&format!("failed:{serial}"), false);
            Some(chain_load())
        }
    }
}

/// Put every file the last update changed back: drop its `.new`, and return
/// its `.prev`, or remove it when it was new. Returns how many were put back
/// and what went wrong.
fn revert(vol: &mut Vol, state: &State) -> (usize, Vec<String>) {
    let mut done = 0;
    let mut errors = Vec::new();
    for (path, new) in state.changed() {
        let p = efi_path(path);
        let prev = format!("{p}.prev");
        let _ = vol.delete(&format!("{p}.new"));
        let r = match vol.exists(&prev) {
            true => vol.delete(&p).and_then(|()| vol.rename(&prev, base(&p))),
            false if new => vol.delete(&p),
            false => continue,
        };
        match r {
            Ok(()) => done += 1,
            Err(e) => errors.push(format!("{path}: {e}")),
        }
    }
    (done, errors)
}

fn base(p: &str) -> &str {
    p.rsplit('\\').next().unwrap_or(p)
}

/// Start `\EFI\BOOT\BOOTX64.EFI` from the boot volume. If it cannot even be
/// loaded, restart, so the firmware boots whatever is there.
fn chain_load() -> Status {
    let loaded = config::boot_volume()
        .and_then(|h| crate::blockio::device_path_of(h.as_ptr()))
        .ok_or_else(|| "the boot volume has no device path".to_string())
        .and_then(|v| load_image(v, BOOT_PATH));
    match loaded {
        Ok(image) => match boot::start_image(image) {
            Ok(()) => Status::SUCCESS,
            Err(e) => e.status(),
        },
        Err(e) => {
            uefi::println!("update      : could not load it ({e}); restarting");
            restart()
        }
    }
}

fn load_image(volume: &uefi::proto::device_path::DevicePath, path: &str) -> Result<Handle, String> {
    let mut buf = [0u16; 256];
    let path16 = CStr16::from_str_with_buf(path, &mut buf).map_err(|_| "path too long")?;
    let mut fbuf = Vec::new();
    let file_dp = DevicePathBuilder::with_vec(&mut fbuf)
        .push(&build::media::FilePath { path_name: path16 })
        .and_then(|b| b.finalize())
        .map_err(|e| format!("file path build failed: {e:?}"))?;
    let full = volume.append_path(file_dp).map_err(|e| format!("append_path failed: {e:?}"))?;
    boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromDevicePath { device_path: &full, boot_policy: BootPolicy::ExactMatch },
    )
    .map_err(|e| format!("LoadImage: {:?}", e.status()))
}

fn restart() -> ! {
    boot::stall(core::time::Duration::from_secs(1));
    uefi::runtime::reset(uefi::runtime::ResetType::WARM, Status::SUCCESS, None)
}

/// The image is attached (or the engine said `local`): a set on trial has
/// proved its network path, and stays. Records the serial on the medium and
/// in the machine's `StormBootMinSerial`. Nothing at all when no trial runs,
/// so an ordinary boot opens nothing on the medium after the attach (#46).
pub fn mark_good(why: &str) {
    let serial = TRIAL.swap(0, Ordering::Relaxed);
    if serial == 0 {
        return;
    }
    let mut vol = match Vol::mount() {
        Ok(v) => v,
        Err(e) => {
            uefi::println!("update      : serial {serial} {why}, and the medium cannot record it ({e})");
            return;
        }
    };
    let text = vol.read_text(STATE_PATH).ok().flatten().unwrap_or_default();
    let state = State::parse(&text);
    if state.trial.map(|t| t.0) != Some(serial) {
        return;
    }
    let next = State {
        serial,
        min: state.min.max(serial),
        trial: None,
        prev: 0,
        files: "",
        ..state
    };
    let written = vol.write_state(&next);
    drop(vol);
    if serial > nv_min() {
        set_nv_min(serial);
    }
    match written {
        Ok(()) => uefi::println!("update      : serial {serial} passed its trial ({why}); it stays"),
        Err(e) => uefi::println!("update      : serial {serial} {why}, and the state was not written ({e})"),
    }
    handoff::set_update(&format!("good:{serial}"), false);
}

/// `StormBootMinSerial`: the highest serial that passed a trial on this
/// machine, from any medium. 0 when unset.
fn nv_min() -> u64 {
    let mut buf = [0u8; 32];
    match uefi::runtime::get_variable(cstr16!("StormBootMinSerial"), &handoff::VENDOR, &mut buf) {
        Ok((v, _)) => core::str::from_utf8(v).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0),
        Err(_) => 0,
    }
}

fn set_nv_min(serial: u64) {
    let attrs = VariableAttributes::NON_VOLATILE
        | VariableAttributes::BOOTSERVICE_ACCESS
        | VariableAttributes::RUNTIME_ACCESS;
    let value = format!("{serial}");
    if let Err(e) = uefi::runtime::set_variable(cstr16!("StormBootMinSerial"), &handoff::VENDOR, attrs, value.as_bytes()) {
        uefi::println!("update      : StormBootMinSerial not set ({:?})", e.status());
    }
}

// ------------------------------------------------------------------- check

/// Ask for the current release and, if it is newer and verifies, write it to
/// the medium and restart into it. Returns when there is nothing to do or it
/// could not be done; every outcome is one console line.
pub fn check(mac: Option<&str>) {
    if let Err(line) = try_update(mac) {
        uefi::println!("update      : {line}");
    }
}

/// `Err` is the console line for every outcome but a restart.
fn try_update(mac: Option<&str>) -> Result<(), String> {
    let stated = config::stated_update()
        .ok_or_else(|| format!("not configured (no update = in {})", config::CONF_PATH))?;
    let (host, port, path) = match manifest::setting(&stated) {
        Some(Setting::Off) => return Err(format!("off (update = {stated}); this medium is pinned")),
        Some(Setting::At { host, port, path }) => (host, port, path),
        None => return Err(format!("update = {stated:?} is not http://host[:port]/path; ignored")),
    };
    let keys = keys();
    if keys.is_empty() {
        return Err("no release key compiled in yet (stormcentral#279); not checked".to_string());
    }
    if test_key().is_some() {
        uefi::println!("update      : a TEST key is compiled in (STORMBOOTX_UPDATE_TEST_KEY)");
    }

    let mut vol = Vol::mount().map_err(|e| format!("the boot medium cannot be opened ({e})"))?;
    let info = vol.fs_info()?;
    if info.read_only() {
        return Err("the boot medium is read-only (an ISO or virtual media); not updated".to_string());
    }
    let state_text = vol.read_text(STATE_PATH)?.unwrap_or_default();
    let state = State::parse(&state_text);
    if let Some((serial, _)) = state.trial {
        return Err(format!("serial {serial} is on trial; nothing new until it ends"));
    }

    let lease = net::leased_reply().unwrap_or_default();
    let addr = clock::lookup(host, &lease)?;
    let host_hdr = if port == 80 { host.to_string() } else { format!("{host}:{port}") };
    let url = format!("http://{host_hdr}{path}");
    let get = |what: &str, limit: usize| -> Result<Vec<u8>, String> {
        match http_get(addr, port, &host_hdr, &format!("{path}/{what}"), limit)? {
            (200, body) => Ok(body),
            (404, _) if what == "current" => Err(format!("no current release at {url}")),
            (status, _) => Err(format!("{url}/{what} answered HTTP {status}")),
        }
    };

    // The signature, before a byte of the manifest is believed.
    let text = get("current", manifest::MAX_MANIFEST)?;
    let sig = manifest::signature(&get("current.sig", 1024)?)
        .ok_or_else(|| format!("{url}/current.sig is not an Ed25519 signature"))?;
    let sig = ed25519_compact::Signature::new(sig);
    if !keys.iter().any(|k| ed25519_compact::PublicKey::new(*k).verify(&text, &sig).is_ok()) {
        return Err(format!(
            "the manifest at {url} does not verify against the {} compiled-in key(s); nothing taken",
            keys.len()
        ));
    }
    let m = manifest::parse(&text).map_err(|b| format!("signed manifest refused: {}", b.text()))?;
    let running = env!("CARGO_PKG_VERSION");

    match manifest::verdict(m.serial, &state, nv_min(), m.for_machine(mac)) {
        Verdict::Current => return Err(format!("current (serial {}, v{})", m.serial, m.version)),
        Verdict::Older { have } => {
            return Err(format!("current (serial {have} here; {url} offers serial {}, which is older)", m.serial))
        }
        Verdict::FailedHere => {
            return Err(format!(
                "serial {} failed its trial on this medium; staying on serial {} until a newer one",
                m.serial, state.serial
            ))
        }
        Verdict::NotACanary => {
            return Err(format!(
                "serial {} is for its {} canaries only; not this machine",
                m.serial,
                m.canaries().len()
            ))
        }
        Verdict::InTrial => return Err("a trial is running".to_string()),
        Verdict::Update => {}
    }

    // What differs from the medium, fetched and checked against the signed
    // manifest before anything is written.
    let mut changed: Vec<(&Entry, Vec<u8>, bool)> = Vec::new();
    for e in m.files() {
        let p = efi_path(e.path);
        let have = vol.read(&p, e.size as usize + 1)?;
        if have.as_deref().is_some_and(|h| h.len() as u64 == e.size && sha256::digest(h).as_bytes() == &e.sha256) {
            continue;
        }
        let body = get(&format!("files/{}", e.path), e.size as usize)?;
        if body.len() as u64 != e.size || sha256::digest(&body).as_bytes() != &e.sha256 {
            return Err(format!(
                "{} from {url} does not match the signed manifest ({} bytes, sha256 {}); nothing written",
                e.path,
                body.len(),
                sha256::digest(&body)
            ));
        }
        changed.push((e, body, have.is_none()));
    }
    // A loadable driver the release no longer carries is retired, or the
    // media would go on loading it.
    let retired: Vec<String> = vol
        .list(r"\stormboot\drivers")
        .into_iter()
        .filter(|n| n.len() > 4 && n.as_bytes()[n.len() - 4..].eq_ignore_ascii_case(b".efi"))
        .map(|n| format!("stormboot/drivers/{n}"))
        .filter(|p| manifest::path_ok(p) && m.file(p).is_none())
        .collect();

    if changed.is_empty() && retired.is_empty() {
        let next = State { serial: m.serial, ..state };
        vol.write_state(&next)?;
        return Err(format!("current (the medium already carries serial {}, v{})", m.serial, m.version));
    }
    // The bootloader is swapped last.
    changed.sort_by_key(|(e, _, _)| e.path.eq_ignore_ascii_case(manifest::BOOTLOADER));

    // Room: every new file is written beside the one it replaces, and the
    // `.prev` copies about to be dropped come back.
    let mut reclaim = 0u64;
    for p in changed.iter().map(|(e, _, _)| e.path).chain(retired.iter().map(String::as_str)) {
        reclaim += vol.size(&format!("{}.prev", efi_path(p))).unwrap_or(0);
    }
    let need: u64 = changed.iter().map(|(e, _, _)| e.size + 8192).sum::<u64>() + 8192;
    if need > info.free_space() + reclaim {
        return Err(format!(
            "serial {} needs {} KB and the medium has {} KB free; nothing written",
            m.serial,
            kb(need),
            kb(info.free_space() + reclaim)
        ));
    }

    apply(&mut vol, &m, &state, &changed, &retired)?;
    let bytes: u64 = changed.iter().map(|(e, _, _)| e.size).sum();
    uefi::println!(
        "update      : v{running} -> v{} ({}, serial {}), {} file(s), {} KB{}; restarting into it",
        m.version,
        if m.golden.is_empty() { "no golden named" } else { m.golden },
        m.serial,
        changed.len(),
        kb(bytes),
        if retired.is_empty() { String::new() } else { format!(", {} driver(s) retired", retired.len()) }
    );
    drop(vol);
    net::release();
    restart()
}

/// Write the new set beside the old and swap them, recording the trial first
/// so an interrupted swap is put back by the starts that follow.
fn apply(
    vol: &mut Vol,
    m: &Manifest,
    state: &State,
    changed: &[(&Entry, Vec<u8>, bool)],
    retired: &[String],
) -> Result<(), String> {
    for p in changed.iter().map(|(e, _, _)| e.path).chain(retired.iter().map(String::as_str)) {
        vol.delete(&format!("{}.prev", efi_path(p)))?;
    }
    for (e, body, _) in changed {
        if let Err(err) = vol.write(&format!("{}.new", efi_path(e.path)), body) {
            for (e, _, _) in changed {
                let _ = vol.delete(&format!("{}.new", efi_path(e.path)));
            }
            return Err(format!("{}: {err}; nothing changed", e.path));
        }
    }
    carry_identity(vol, changed);

    let mut files: Vec<String> = retired.to_vec();
    files.extend(changed.iter().map(|(e, _, new)| format!("{}{}", if *new { "+" } else { "" }, e.path)));
    let files = files.join(" ");
    let trial = State {
        serial: m.serial,
        prev: state.serial,
        trial: Some((m.serial, 0)),
        files: &files,
        ..*state
    };
    vol.write_state(&trial)?;

    let mut swap = || -> Result<(), String> {
        for p in retired {
            let p = efi_path(p);
            vol.rename(&p, &format!("{}.prev", base(&p)))?;
        }
        for (e, _, new) in changed {
            let p = efi_path(e.path);
            if !new && vol.exists(&p) {
                vol.rename(&p, &format!("{}.prev", base(&p)))?;
            }
            vol.rename(&format!("{p}.new"), base(&p))?;
        }
        Ok(())
    };
    if let Err(e) = swap() {
        let (_, errors) = revert(vol, &trial);
        let _ = vol.write_state(state);
        return Err(format!(
            "the swap failed ({e}); the previous set is back{}",
            if errors.is_empty() { String::new() } else { format!(" except {}", errors.join("; ")) }
        ));
    }
    Ok(())
}

/// The release's `stormboot.conf` replaces the medium's. A `name =` or
/// `tag =` the old one set and the new one does not is this machine's, so it
/// moves to `local.conf`, which is read first and never updated.
fn carry_identity(vol: &mut Vol, changed: &[(&Entry, Vec<u8>, bool)]) {
    let Some((_, new, _)) = changed.iter().find(|(e, _, _)| e.path.eq_ignore_ascii_case("stormboot/stormboot.conf")) else {
        return;
    };
    let Ok(Some(old)) = vol.read_text(config::CONF_PATH) else { return };
    let new = String::from_utf8_lossy(new);
    let mut local = vol.read_text(config::LOCAL_CONF_PATH).ok().flatten().unwrap_or_default();
    let mut carried = Vec::new();
    for key in ["name", "tag"] {
        if let Some(v) = config::field(&old, key) {
            if config::field(&new, key).is_none() && config::field(&local, key).is_none() {
                if !local.is_empty() && !local.ends_with('\n') {
                    local.push('\n');
                }
                local.push_str(&format!("{key} = {v}\n"));
                carried.push(format!("{key} = {v}"));
            }
        }
    }
    if carried.is_empty() {
        return;
    }
    match vol.write(config::LOCAL_CONF_PATH, local.as_bytes()) {
        Ok(()) => uefi::println!("update      : kept {} in {}", carried.join(", "), config::LOCAL_CONF_PATH),
        Err(e) => uefi::println!("update      : could not keep {} in {} ({e})", carried.join(", "), config::LOCAL_CONF_PATH),
    }
}

/// One GET, `Connection: close`; the status and the body, chunking undone.
fn http_get(addr: [u8; 4], port: u16, host: &str, path: &str, limit: usize) -> Result<(u16, Vec<u8>), String> {
    let mut sock = net::TcpSocket::connect_within(addr, port, 10)?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: stormbootx/{}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    );
    sock.send(req.as_bytes())?;
    let mut raw = sock.read_to_end(limit + 16 * 1024)?;
    let (status, body) =
        manifest::response(&mut raw).ok_or_else(|| format!("GET {path}: not a whole HTTP response"))?;
    Ok((status, body.to_vec()))
}

// ------------------------------------------------------------------ medium

/// The volume this image was loaded from, open for files. Only one at a
/// time, and none of `config`'s readers while it is open: both open the file
/// system exclusively.
struct Vol {
    // Dropped first: the file handle closes before the protocol does.
    root: Directory,
    _fs: ScopedProtocol<SimpleFileSystem>,
}

impl Vol {
    fn mount() -> Result<Vol, String> {
        let h = config::boot_volume().ok_or("LoadedImage names no boot volume")?;
        let mut fs = config::open_fs(h).ok_or("the boot volume has no file system")?;
        let root = fs.open_volume().map_err(|e| format!("open_volume: {:?}", e.status()))?;
        Ok(Vol { root, _fs: fs })
    }

    fn fs_info(&mut self) -> Result<alloc::boxed::Box<FileSystemInfo>, String> {
        self.root
            .get_boxed_info::<FileSystemInfo>()
            .map_err(|e| format!("the medium's file system info: {:?}", e.status()))
    }

    /// A regular file, `None` when there is none.
    fn open(&mut self, path: &str, mode: FileMode) -> Result<Option<RegularFile>, String> {
        let mut buf = [0u16; 260];
        let p16 = CStr16::from_str_with_buf(path, &mut buf).map_err(|_| format!("{path}: not a usable name"))?;
        match self.root.open(p16, mode, FileAttribute::empty()) {
            Ok(h) => match h.into_type() {
                Ok(FileType::Regular(f)) => Ok(Some(f)),
                Ok(FileType::Dir(_)) => Err(format!("{path} is a directory")),
                Err(e) => Err(format!("{path}: {:?}", e.status())),
            },
            Err(e) if e.status() == Status::NOT_FOUND => Ok(None),
            Err(e) => Err(format!("open {path}: {:?}", e.status())),
        }
    }

    fn exists(&mut self, path: &str) -> bool {
        matches!(self.open(path, FileMode::Read), Ok(Some(_)))
    }

    fn size(&mut self, path: &str) -> Option<u64> {
        let mut f = self.open(path, FileMode::Read).ok()??;
        Some(f.get_boxed_info::<FileInfo>().ok()?.file_size())
    }

    /// Up to `limit` bytes of a file.
    fn read(&mut self, path: &str, limit: usize) -> Result<Option<Vec<u8>>, String> {
        let Some(mut f) = self.open(path, FileMode::Read)? else { return Ok(None) };
        let mut out = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        while out.len() < limit {
            let n = f.read(&mut chunk).map_err(|e| format!("read {path}: {:?}", e.status()))?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        out.truncate(limit);
        Ok(Some(out))
    }

    fn read_text(&mut self, path: &str) -> Result<Option<String>, String> {
        Ok(self.read(path, 64 * 1024)?.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// Replace a file's contents, making its directories first.
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        let mut dir = String::new();
        let parts: Vec<&str> = path.trim_start_matches('\\').split('\\').collect();
        for d in &parts[..parts.len().saturating_sub(1)] {
            dir.push('\\');
            dir.push_str(d);
            let mut buf = [0u16; 260];
            let d16 = CStr16::from_str_with_buf(&dir, &mut buf).map_err(|_| "path too long")?;
            let _ = self.root.open(d16, FileMode::CreateReadWrite, FileAttribute::DIRECTORY);
        }
        let mut buf = [0u16; 260];
        let p16 = CStr16::from_str_with_buf(path, &mut buf).map_err(|_| "path too long")?;
        let h = self
            .root
            .open(p16, FileMode::CreateReadWrite, FileAttribute::empty())
            .map_err(|e| format!("create {path}: {:?}", e.status()))?;
        let mut f = match h.into_type().map_err(|e| format!("{path}: {:?}", e.status()))? {
            FileType::Regular(f) => f,
            FileType::Dir(_) => return Err(format!("{path} is a directory")),
        };
        f.set_position(0).map_err(|e| format!("seek {path}: {:?}", e.status()))?;
        f.write(data).map_err(|e| format!("write {path}: {:?}", e.status()))?;
        config::truncate(&mut f, data.len() as u64)?;
        f.flush().map_err(|e| format!("flush {path}: {:?}", e.status()))
    }

    /// Remove a file; one that is not there is already removed.
    fn delete(&mut self, path: &str) -> Result<(), String> {
        match self.open(path, FileMode::ReadWrite)? {
            Some(f) => f.delete().map_err(|e| format!("delete {path}: {:?}", e.status())),
            None => Ok(()),
        }
    }

    /// Rename a file within its directory: `SetInfo` with a new `FileName`
    /// (see `config::truncate` for why the rest is carried across).
    fn rename(&mut self, path: &str, to: &str) -> Result<(), String> {
        let mut f = self.open(path, FileMode::ReadWrite)?.ok_or_else(|| format!("{path} is not there"))?;
        let info = f.get_boxed_info::<FileInfo>().map_err(|e| format!("{path}: {:?}", e.status()))?;
        let mut nbuf = [0u16; 128];
        let name = CStr16::from_str_with_buf(to, &mut nbuf).map_err(|_| "name too long")?;
        let mut storage = [0u64; 48];
        let bytes = unsafe {
            core::slice::from_raw_parts_mut(storage.as_mut_ptr().cast::<u8>(), core::mem::size_of_val(&storage))
        };
        let renamed = FileInfo::new(
            bytes,
            info.file_size(),
            0,
            *info.create_time(),
            *info.last_access_time(),
            *info.modification_time(),
            info.attribute(),
            name,
        )
        .map_err(|e| format!("rename {path}: {e:?}"))?;
        f.set_info(renamed).map_err(|e| format!("rename {path} to {to}: {:?}", e.status()))?;
        f.flush().map_err(|e| format!("flush {to}: {:?}", e.status()))
    }

    /// The regular files in a directory.
    fn list(&mut self, path: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut buf = [0u16; 260];
        let Ok(p16) = CStr16::from_str_with_buf(path, &mut buf) else { return names };
        let Ok(h) = self.root.open(p16, FileMode::Read, FileAttribute::empty()) else { return names };
        let Ok(FileType::Dir(mut dir)) = h.into_type() else { return names };
        while names.len() < 64 {
            match dir.read_entry_boxed() {
                Ok(Some(info)) if !info.attribute().contains(FileAttribute::DIRECTORY) => {
                    names.push(info.file_name().to_string())
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        names
    }

    fn write_state(&mut self, s: &State) -> Result<(), String> {
        let mut buf = vec![0u8; 4096];
        let n = s.render(&mut buf).ok_or("the state does not fit")?;
        self.write(STATE_PATH, &buf[..n])
    }
}
