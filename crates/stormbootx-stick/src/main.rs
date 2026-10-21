//! stormbootx-stick (#151, stormcos#589): a running node updates its own boot
//! stick, with no hands.
//!
//! The medium's self-update (#83) cannot fix a stick whose updater is broken:
//! no stick at v0.26 or older ever updated (#148), and those sticks still boot
//! the node into stormcos. So the node, once it is up, does what the stick's
//! next boot would have done:
//!
//! 1. **find** the stick: a removable or USB disk whose FAT ESP carries
//!    `EFI/BOOT/BOOTX64.EFI` and `stormboot/`. Never a disk that is mounted
//!    (the root's, a pallet's), that something holds (dm, md), or that carries
//!    a stormblock slab (`STRMSLAB` at its start or at a partition's): the
//!    node's system disk is refused three ways before anything is opened
//!    for writing;
//! 2. **read** its `stormboot.conf` (`update =`, `version =`), `local.conf`
//!    and `state`; a stick that names no `update =` takes its family's
//!    boothelper (#161), and one with no `version =` is 0.0.0;
//! 3. **ask** stormcentral for `<update>/current` and `current.sig`, and
//!    verify the Ed25519 signature against the compiled-in release keys
//!    before a byte of the manifest is believed;
//! 4. **decide** with the self-update's own rules (`manifest::verdict`,
//!    `manifest::version_ok`): never backwards, never a serial that failed its
//!    trial on this medium, never during a trial, canaries only for them;
//! 5. **apply** exactly as the self-update does: every changed file checked
//!    against the signed size and SHA-256, written as `*.new`, the trial
//!    recorded in `state`, then the `*.prev` swap with the bootloader last,
//!    put back if the swap fails; a `name =`/`tag =` the new `stormboot.conf`
//!    would drop is kept in `local.conf`. The stick's next boot runs the new
//!    set on trial, and puts the old one back if it never attaches;
//! 6. **report** one line per step, an exit status (0 unless something
//!    failed) and `/run/stormbootx-stick/status.json`.
//!
//! The manifest, its rules and the release keys are `src/manifest.rs` and
//! `src/sha256.rs`, included by path: one implementation, the medium's.
//!
//! For tests, everything the machine provides is a root that can be pointed
//! elsewhere (`--sys`, `--proc`, `--dev`, `--efivars`), and `--esp DIR` takes
//! an ESP that is already there (a directory) instead of finding and mounting
//! a stick. Mounting needs root, which a node's boot.d task has.

use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[allow(dead_code)]
#[path = "../../../src/manifest.rs"]
mod manifest;
#[allow(dead_code)]
#[path = "../../../src/sha256.rs"]
mod sha256;

use manifest::{Entry, Manifest, Setting, State, Verdict};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const STATE: &str = "stormboot/state";
const CONF: &str = "stormboot/stormboot.conf";
const LOCAL_CONF: &str = "stormboot/local.conf";
const DRIVERS: &str = "stormboot/drivers";
/// stormblock's slab header magic (stormblock `drive/slab.rs`).
const SLAB_MAGIC: &[u8; 8] = b"STRMSLAB";
/// #76's vendor GUID, under which the medium keeps `StormBootMinSerial`.
const VENDOR: &str = "ab361f54-0166-44a4-a088-1ac22e98ab76";

/// A key the tests sign with, compiled in only when the build sets
/// `STORMBOOTX_UPDATE_TEST_KEY`, as for the medium (`selfupdate.rs`).
fn keys() -> Vec<[u8; 32]> {
    let mut k = manifest::RELEASE_KEYS.to_vec();
    k.extend(option_env!("STORMBOOTX_UPDATE_TEST_KEY").and_then(manifest::hex32));
    k
}

// ------------------------------------------------------------------ report

/// What happened, for the console and the status file.
#[derive(Default)]
struct Report {
    lines: Vec<String>,
    outcome: &'static str,
    stick: String,
    from: String,
    to: String,
    serial: u64,
}

impl Report {
    fn say(&mut self, step: &str, text: impl AsRef<str>) {
        let line = format!("{step:<7}: {}", text.as_ref());
        println!("stormbootx-stick: {line}");
        self.lines.push(line);
    }
    fn json(&self, dry_run: bool) -> String {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut s = String::from("{");
        let _ = write!(
            s,
            "\"outcome\":{},\"stick\":{},\"from\":{},\"to\":{},\"serial\":{},\"dry_run\":{},\"at\":{},\"agent\":{},\"lines\":[",
            q(self.outcome),
            q(&self.stick),
            q(&self.from),
            q(&self.to),
            self.serial,
            dry_run,
            at,
            q(&format!("stormbootx-stick {VERSION}"))
        );
        for (i, l) in self.lines.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&q(l));
        }
        s.push_str("]}\n");
        s
    }
}

/// A JSON string.
fn q(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

// ----------------------------------------------------------------- options

struct Opts {
    dry_run: bool,
    status: PathBuf,
    sys: PathBuf,
    proc_: PathBuf,
    dev: PathBuf,
    efivars: PathBuf,
    mnt: PathBuf,
    esp: Option<PathBuf>,
    update: Option<String>,
    boothelpers: String,
    mac: Option<String>,
}

/// stormcentral's open boothelper route (`deploy/build-golden.sh`'s
/// `update_url`): a stick that names no `update =` takes its family's from
/// here (#161).
const BOOTHELPERS: &str = "http://stormcentral.g8.lo/api/v1/boothelpers";

const USAGE: &str = "usage: stormbootx-stick [--dry-run] [--status FILE] [--update URL] [--mac MAC]
                        [--boothelpers URL] [--esp DIR] [--sys DIR] [--proc DIR] [--dev DIR] [--efivars DIR] [--mnt DIR]

Find this node's stormbootx boot stick and update it from stormcentral's signed
release, with the medium's own rules (stormbootx #151). --update is used only
when the stick names no `update =`; with neither, the stick's family's
boothelper under --boothelpers (default http://stormcentral.g8.lo/api/v1/boothelpers).
--esp updates an ESP already mounted there.";

fn opts() -> Result<Opts, String> {
    let mut o = Opts {
        dry_run: false,
        status: PathBuf::from("/run/stormbootx-stick/status.json"),
        sys: PathBuf::from("/sys"),
        proc_: PathBuf::from("/proc"),
        dev: PathBuf::from("/dev"),
        efivars: PathBuf::from("/sys/firmware/efi/efivars"),
        mnt: PathBuf::from("/run/stormbootx-stick/mnt"),
        esp: None,
        update: None,
        boothelpers: BOOTHELPERS.to_string(),
        mac: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or_else(|| format!("{a} needs a value\n{USAGE}"));
        match a.as_str() {
            "--dry-run" => o.dry_run = true,
            "--status" => o.status = val()?.into(),
            "--sys" => o.sys = val()?.into(),
            "--proc" => o.proc_ = val()?.into(),
            "--dev" => o.dev = val()?.into(),
            "--efivars" => o.efivars = val()?.into(),
            "--mnt" => o.mnt = val()?.into(),
            "--esp" => o.esp = Some(val()?.into()),
            "--update" => o.update = Some(val()?),
            "--boothelpers" => o.boothelpers = val()?.trim_end_matches('/').to_string(),
            "--mac" => o.mac = Some(val()?.to_ascii_lowercase()),
            "-h" | "--help" => return Err(USAGE.to_string()),
            _ => return Err(format!("unknown argument {a}\n{USAGE}")),
        }
    }
    Ok(o)
}

fn main() {
    let o = match opts() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let mut r = Report { outcome: "error", ..Default::default() };
    r.say("start", format!("stormbootx-stick {VERSION}{}", if o.dry_run { " (dry run: nothing is written)" } else { "" }));
    let ok = match run(&o, &mut r) {
        Ok(()) => true,
        Err(e) => {
            // A refusal (a bad signature, a file that does not match) keeps
            // its own outcome; anything else is an error.
            if r.outcome != "refused" {
                r.outcome = "error";
            }
            r.say("failed", e);
            false
        }
    };
    if let Some(dir) = o.status.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Err(e) = fs::write(&o.status, r.json(o.dry_run)) {
        eprintln!("stormbootx-stick: status not written to {}: {e}", o.status.display());
    }
    std::process::exit(if ok { 0 } else { 1 });
}

fn run(o: &Opts, r: &mut Report) -> Result<(), String> {
    if let Some(esp) = &o.esp {
        r.stick = esp.display().to_string();
        r.say("find", format!("the ESP at {} (--esp)", esp.display()));
        return update(o, esp, r);
    }
    let found = find(o, r)?;
    let Some(part) = found else {
        r.outcome = "no-stick";
        r.say("done", "no stormbootx stick on this node; nothing to do");
        return Ok(());
    };
    r.stick = part.display().to_string();
    let mnt = &o.mnt;
    let flags = if o.dry_run { MS_RDONLY } else { 0 } | MS_NOSUID | MS_NODEV | MS_NOEXEC;
    mount_vfat(&part, mnt, flags).map_err(|e| format!("mounting {} at {}: {e}", part.display(), mnt.display()))?;
    let result = update(o, mnt, r);
    unsafe { sync() };
    if let Err(e) = umount(mnt) {
        r.say("umount", format!("{}: {e}", mnt.display()));
    }
    result
}

// -------------------------------------------------------------------- find

/// The one stick, or `None`. More than one is refused: a node boots from one,
/// and which is not something to guess.
fn find(o: &Opts, r: &mut Report) -> Result<Option<PathBuf>, String> {
    let mounted = mounted_devs(&o.proc_);
    let block = o.sys.join("block");
    let mut names: Vec<String> = fs::read_dir(&block)
        .map_err(|e| format!("{}: {e}", block.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut found: Vec<PathBuf> = Vec::new();
    for name in names {
        if ["loop", "ram", "zram", "dm-", "md", "nbd", "ublk", "sr", "fd", "zd", "rbd"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            continue;
        }
        let d = block.join(&name);
        let removable = read_trim(&d.join("removable")).as_deref() == Some("1");
        let usb = fs::read_link(&d).map(|l| l.to_string_lossy().contains("/usb")).unwrap_or(false);
        if !removable && !usb {
            r.say("find", format!("{name}: not removable or USB; not a stick"));
            continue;
        }
        let parts = partitions(&d, &name);
        if let Some(why) = refused(o, &d, &name, &parts, &mounted) {
            r.say("find", format!("{name}: refused: {why}"));
            continue;
        }
        let looks: Vec<(String, PathBuf)> = if parts.is_empty() {
            vec![(name.clone(), o.dev.join(&name))]
        } else {
            parts.iter().map(|(p, _)| (p.clone(), o.dev.join(p))).collect()
        };
        for (pname, dev) in looks {
            match has_markers(&dev, &o.mnt) {
                Ok(true) => {
                    r.say("find", format!("{pname}: a stormbootx stick (EFI/BOOT/BOOTX64.EFI and stormboot/)"));
                    found.push(dev);
                }
                Ok(false) => r.say("find", format!("{pname}: no stormbootx ESP")),
                Err(e) => r.say("find", format!("{pname}: not read: {e}")),
            }
        }
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        n => Err(format!("{n} stormbootx sticks on this node; not choosing one")),
    }
}

/// `(name, start in bytes)` of each partition of a disk.
fn partitions(d: &Path, name: &str) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            (n.starts_with(name) && p.join("partition").exists())
                .then(|| (n, read_trim(&p.join("start")).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0) * 512))
        })
        .collect();
    v.sort();
    v
}

/// Why a removable disk must not be touched, or `None`.
fn refused(
    o: &Opts,
    d: &Path,
    name: &str,
    parts: &[(String, u64)],
    mounted: &[String],
) -> Option<String> {
    let devnum = |p: &Path| read_trim(&p.join("dev")).unwrap_or_default();
    let mut nodes = vec![(name.to_string(), devnum(d))];
    nodes.extend(parts.iter().map(|(p, _)| (p.clone(), devnum(&d.join(p)))));
    for (n, num) in &nodes {
        if !num.is_empty() && mounted.iter().any(|m| m == num) {
            return Some(format!("{n} ({num}) is mounted (the root's, a pallet's, or someone's)"));
        }
    }
    let held = |p: &Path| fs::read_dir(p.join("holders")).map(|mut i| i.next().is_some()).unwrap_or(false);
    if held(d) || parts.iter().any(|(p, _)| held(&d.join(p))) {
        return Some("something holds it (dm, md, …)".into());
    }
    let dev = o.dev.join(name);
    let mut f = match fs::File::open(&dev) {
        Ok(f) => f,
        Err(e) => return Some(format!("{} cannot be read to check for a slab: {e}", dev.display())),
    };
    for at in std::iter::once(0).chain(parts.iter().map(|(_, s)| *s)) {
        let mut m = [0u8; 8];
        if f.seek(SeekFrom::Start(at)).is_ok() && f.read_exact(&mut m).is_ok() && &m == SLAB_MAGIC {
            return Some(format!("it holds a stormblock slab (STRMSLAB at byte {at})"));
        }
    }
    None
}

/// The `major:minor` of everything mounted.
fn mounted_devs(proc_: &Path) -> Vec<String> {
    fs::read_to_string(proc_.join("self/mountinfo"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2).map(String::from))
        .collect()
}

/// Mount read-only, look for both markers, unmount.
fn has_markers(dev: &Path, mnt: &Path) -> Result<bool, String> {
    mount_vfat(dev, mnt, MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC)?;
    let ok = find_ci(mnt, "EFI/BOOT/BOOTX64.EFI").is_some_and(|p| p.is_file())
        && find_ci(mnt, "stormboot").is_some_and(|p| p.is_dir());
    umount(mnt)?;
    Ok(ok)
}

/// A path under `root`, matched without case, as the FAT is.
fn find_ci(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut at = root.to_path_buf();
    for part in rel.split('/') {
        let exact = at.join(part);
        if exact.exists() {
            at = exact;
            continue;
        }
        at = fs::read_dir(&at)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?
            .path();
    }
    Some(at)
}

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

// ------------------------------------------------------------------- mount

const MS_RDONLY: u64 = 1;
const MS_NOSUID: u64 = 2;
const MS_NODEV: u64 = 4;
const MS_NOEXEC: u64 = 8;

extern "C" {
    #[link_name = "mount"]
    fn sys_mount(src: *const i8, target: *const i8, fstype: *const i8, flags: u64, data: *const u8) -> i32;
    fn umount2(target: *const i8, flags: i32) -> i32;
    fn sync();
    fn statvfs(path: *const i8, buf: *mut StatVfs) -> i32;
}

/// musl's `struct statvfs` on 64-bit Linux, with room to spare.
#[repr(C)]
struct StatVfs {
    f_bsize: u64,
    f_frsize: u64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_favail: u64,
    f_fsid: u64,
    f_flag: u64,
    f_namemax: u64,
    spare: [u64; 16],
}

fn cpath(p: &Path) -> Vec<u8> {
    let mut v = p.as_os_str().as_bytes().to_vec();
    v.push(0);
    v
}

fn mount_vfat(dev: &Path, mnt: &Path, flags: u64) -> Result<(), String> {
    fs::create_dir_all(mnt).map_err(|e| format!("{}: {e}", mnt.display()))?;
    let (d, m) = (cpath(dev), cpath(mnt));
    let rc = unsafe { sys_mount(d.as_ptr().cast(), m.as_ptr().cast(), c"vfat".as_ptr(), flags, std::ptr::null()) };
    if rc != 0 {
        return Err(format!("mount {}: {}", dev.display(), std::io::Error::last_os_error()));
    }
    Ok(())
}

fn umount(mnt: &Path) -> Result<(), String> {
    let m = cpath(mnt);
    if unsafe { umount2(m.as_ptr().cast(), 0) } != 0 {
        return Err(format!("umount {}: {}", mnt.display(), std::io::Error::last_os_error()));
    }
    Ok(())
}

fn free_space(root: &Path) -> Option<u64> {
    let p = cpath(root);
    let mut s: StatVfs = unsafe { std::mem::zeroed() };
    (unsafe { statvfs(p.as_ptr().cast(), &mut s) } == 0).then(|| s.f_bavail.saturating_mul(s.f_frsize.max(1)))
}

// ------------------------------------------------------------------ medium

/// The stick's ESP, at a directory.
struct Medium<'a> {
    root: &'a Path,
}

impl Medium<'_> {
    fn path(&self, rel: &str) -> PathBuf {
        find_ci(self.root, rel).unwrap_or_else(|| self.root.join(rel))
    }
    fn read(&self, rel: &str, limit: usize) -> Result<Option<Vec<u8>>, String> {
        let p = self.path(rel);
        match fs::File::open(&p) {
            Ok(f) => {
                let mut v = Vec::new();
                f.take(limit as u64).read_to_end(&mut v).map_err(|e| format!("read {rel}: {e}"))?;
                Ok(Some(v))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("open {rel}: {e}")),
        }
    }
    fn text(&self, rel: &str) -> Result<Option<String>, String> {
        Ok(self.read(rel, 64 << 10)?.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }
    /// Write and flush to the device, then read it back.
    fn write(&self, rel: &str, data: &[u8]) -> Result<(), String> {
        let p = self.path(rel);
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut f = fs::File::create(&p).map_err(|e| format!("create {rel}: {e}"))?;
        f.write_all(data).map_err(|e| format!("write {rel}: {e}"))?;
        f.sync_all().map_err(|e| format!("sync {rel}: {e}"))?;
        drop(f);
        match fs::read(&p) {
            Ok(back) if back == data => Ok(()),
            Ok(_) => Err(format!("{rel} reads back different from what was written")),
            Err(e) => Err(format!("read back {rel}: {e}")),
        }
    }
    fn exists(&self, rel: &str) -> bool {
        find_ci(self.root, rel).is_some()
    }
    fn delete(&self, rel: &str) -> Result<(), String> {
        match find_ci(self.root, rel) {
            Some(p) => fs::remove_file(&p).map_err(|e| format!("delete {rel}: {e}")),
            None => Ok(()),
        }
    }
    /// Rename `from` to `to` in the same directory.
    fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let src = find_ci(self.root, from).ok_or_else(|| format!("rename {from}: not there"))?;
        let dst = src.with_file_name(to.rsplit('/').next().unwrap_or(to));
        fs::rename(&src, &dst).map_err(|e| format!("rename {from} -> {to}: {e}"))
    }
    fn size(&self, rel: &str) -> Option<u64> {
        find_ci(self.root, rel).and_then(|p| fs::metadata(p).ok()).map(|m| m.len())
    }
    fn list(&self, rel: &str) -> Vec<String> {
        let mut v: Vec<String> = find_ci(self.root, rel)
            .and_then(|p| fs::read_dir(p).ok())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
    fn write_state(&self, s: &State) -> Result<(), String> {
        let mut buf = vec![0u8; 4096];
        let n = s.render(&mut buf).ok_or("the state does not fit")?;
        self.write(STATE, &buf[..n])
    }
}

// ------------------------------------------------------------------ update

fn kb(n: u64) -> u64 {
    n.div_ceil(1024)
}

/// The machine's lowest physical NIC MAC, as the medium picks it (#15), for a
/// canary line.
fn machine_mac(o: &Opts) -> Option<String> {
    if let Some(m) = &o.mac {
        return Some(m.clone());
    }
    let net = o.sys.join("class/net");
    fs::read_dir(net)
        .ok()?
        .flatten()
        .filter(|e| e.path().join("device").exists())
        .filter_map(|e| read_trim(&e.path().join("address")))
        .filter(|a| a.len() == 17 && a != "00:00:00:00:00:00")
        .map(|a| a.to_ascii_lowercase())
        .min()
}

/// `StormBootMinSerial`, the highest serial that passed a trial on this
/// machine (efivarfs: four bytes of attributes, then the decimal text).
fn nv_min(o: &Opts) -> u64 {
    fs::read(o.efivars.join(format!("StormBootMinSerial-{VENDOR}")))
        .ok()
        .and_then(|b| b.get(4..).map(|d| String::from_utf8_lossy(d).trim().parse().unwrap_or(0)))
        .unwrap_or(0)
}

fn update(o: &Opts, root: &Path, r: &mut Report) -> Result<(), String> {
    let vol = Medium { root };
    if !vol.exists(manifest::BOOTLOADER) || !vol.exists("stormboot") {
        return Err(format!("{} carries no stormbootx (EFI/BOOT/BOOTX64.EFI and stormboot/)", root.display()));
    }
    let main_conf = vol.text(CONF)?.unwrap_or_default();
    let local = vol.text(LOCAL_CONF)?.unwrap_or_default();
    let conf = format!("{local}\n{main_conf}");
    // The stick's stormbootx, from the line every medium since v0.19 carries
    // (#94); one without it is older than any release that could be offered.
    let running = manifest::conf_field(&main_conf, "version").unwrap_or("0.0.0").to_string();
    r.from = running.clone();
    let state_text = vol.text(STATE)?.unwrap_or_default();
    let state = State::parse(&state_text);
    r.say(
        "read",
        format!(
            "stormbootx {running} (commit {}), media {}, serial {}{}{}",
            manifest::conf_field(&main_conf, "commit").unwrap_or("?"),
            manifest::conf_field(&conf, "media").unwrap_or("?"),
            state.serial,
            match state.trial {
                Some((s, n)) => format!(", serial {s} on trial after {n} start(s)"),
                None => String::new(),
            },
            if manifest::conf_field(&main_conf, "version").is_none() {
                "; no version =, so older than any release"
            } else {
                ""
            }
        ),
    );

    let stated = manifest::conf_field(&conf, "update").map(String::from);
    let url = match (&stated, &o.update) {
        (Some(u), _) => u.clone(),
        (None, Some(u)) => {
            r.say("read", format!("the stick names no update =; using {u} (--update)"));
            u.clone()
        }
        (None, None) => {
            // A stick that names no `update =` (hand-built, or an exe
            // swapped in by hand) still updates: from its family's
            // boothelper (#161). Only `update = off` pins a stick.
            let (family, why) = family(&vol, &main_conf);
            let u = format!("{}/{family}", o.boothelpers);
            r.say("read", format!("the stick names no update =; using {u} ({why})"));
            u
        }
    };
    let (host, port, path) = match manifest::setting(&url) {
        Some(Setting::Off) => {
            r.outcome = "off";
            r.say("done", format!("update = {url}: this stick is pinned; left alone"));
            return Ok(());
        }
        Some(Setting::At { host, port, path }) => (host.to_string(), port, path.to_string()),
        None => return Err(format!("update = {url:?} is not http://host[:port]/path")),
    };
    if let Some((serial, _)) = state.trial {
        r.outcome = "current";
        r.say("done", format!("serial {serial} is on trial; the stick's next boot judges it, nothing new until then"));
        return Ok(());
    }

    // The signature, before a byte of the manifest is believed.
    let host_hdr = if port == 80 { host.clone() } else { format!("{host}:{port}") };
    let base = format!("http://{host_hdr}{path}");
    let get = |what: &str, limit: usize| -> Result<Vec<u8>, String> {
        match http_get(&host, port, &host_hdr, &format!("{path}/{what}"), limit)? {
            (200, body) => Ok(body),
            (404, _) if what == "current" => Err(format!("no current release at {base}")),
            (status, _) => Err(format!("{base}/{what} answered HTTP {status}")),
        }
    };
    let text = get("current", manifest::MAX_MANIFEST)?;
    let sig = manifest::signature(&get("current.sig", 1024)?)
        .ok_or_else(|| format!("{base}/current.sig is not an Ed25519 signature"))?;
    let sig = ed25519_compact::Signature::new(sig);
    let keys = keys();
    if !keys.iter().any(|k| ed25519_compact::PublicKey::new(*k).verify(&text, &sig).is_ok()) {
        r.outcome = "refused";
        return Err(format!(
            "the manifest at {base} does not verify against the {} compiled-in key(s); nothing taken",
            keys.len()
        ));
    }
    let m = manifest::parse(&text).map_err(|b| format!("signed manifest refused: {}", b.text()))?;
    r.serial = m.serial;
    r.to = m.version.to_string();
    r.say("ask", format!("{base}: serial {}, v{} ({}), signature verified", m.serial, m.version, m.golden));

    let mac = machine_mac(o);
    match manifest::verdict(m.serial, &state, nv_min(o), m.for_machine(mac.as_deref())) {
        Verdict::Update => {}
        Verdict::Current => return current(r, &format!("the stick carries serial {} (v{})", m.serial, m.version)),
        Verdict::Older { have } => {
            return current(r, &format!("serial {have} here; serial {} is older", m.serial));
        }
        Verdict::FailedHere => {
            r.outcome = "declined";
            r.say("decide", format!("serial {} failed its trial on this stick; staying on serial {}", m.serial, state.serial));
            return Ok(());
        }
        Verdict::NotACanary => {
            r.outcome = "declined";
            r.say(
                "decide",
                format!(
                    "serial {} is for its {} canaries only; not this machine ({})",
                    m.serial,
                    m.canaries().len(),
                    mac.as_deref().unwrap_or("no MAC")
                ),
            );
            return Ok(());
        }
        Verdict::InTrial => return current(r, "a trial is running"),
    }
    match manifest::version_ok(m.version, &running, m.downgrade) {
        manifest::Version::Ok => {}
        manifest::Version::Older => {
            if !o.dry_run {
                vol.write_state(&State { min: state.min.max(m.serial), ..state })?;
            }
            r.outcome = "declined";
            r.say("decide", format!("the stick runs v{running}; serial {} offers v{}, which is older; not taken", m.serial, m.version));
            return Ok(());
        }
        manifest::Version::Unreadable => {
            return Err(format!("serial {} names version {:?}, which is not MAJOR.MINOR.PATCH; nothing taken", m.serial, m.version));
        }
    }

    // What differs, fetched and checked against the signed manifest before
    // anything is written.
    let mut changed: Vec<(&Entry, Vec<u8>, bool)> = Vec::new();
    for e in m.files() {
        let have = vol.read(e.path, e.size as usize + 1)?;
        if have.as_deref().is_some_and(|h| h.len() as u64 == e.size && sha256::digest(h).as_bytes() == &e.sha256) {
            continue;
        }
        let body = get(&format!("files/{}", e.path), e.size as usize)?;
        if body.len() as u64 != e.size || sha256::digest(&body).as_bytes() != &e.sha256 {
            r.outcome = "refused";
            return Err(format!(
                "{} from {base} does not match the signed manifest ({} bytes, sha256 {}); nothing written",
                e.path,
                body.len(),
                sha256::digest(&body)
            ));
        }
        changed.push((e, body, have.is_none()));
    }
    let retired: Vec<String> = vol
        .list(DRIVERS)
        .into_iter()
        .filter(|n| n.len() > 4 && n.as_bytes()[n.len() - 4..].eq_ignore_ascii_case(b".efi"))
        .map(|n| format!("{DRIVERS}/{n}"))
        .filter(|p| manifest::path_ok(p) && m.file(p).is_none())
        .collect();
    if changed.is_empty() && retired.is_empty() {
        if !o.dry_run {
            vol.write_state(&State { serial: m.serial, ..state })?;
        }
        return current(r, &format!("the stick already carries serial {}'s files (v{})", m.serial, m.version));
    }
    changed.sort_by_key(|(e, _, _)| e.path.eq_ignore_ascii_case(manifest::BOOTLOADER));

    let mut reclaim = 0u64;
    for p in changed.iter().map(|(e, _, _)| e.path).chain(retired.iter().map(String::as_str)) {
        reclaim += vol.size(&format!("{p}.prev")).unwrap_or(0);
    }
    let need: u64 = changed.iter().map(|(e, _, _)| e.size + 8192).sum::<u64>() + 8192;
    let free = free_space(root).unwrap_or(u64::MAX);
    if need > free.saturating_add(reclaim) {
        return Err(format!(
            "serial {} needs {} KB and the stick has {} KB free; nothing written (a raw image is #154)",
            m.serial,
            kb(need),
            kb(free + reclaim)
        ));
    }
    let bytes: u64 = changed.iter().map(|(e, _, _)| e.size).sum();
    let what = format!(
        "v{running} -> v{} (serial {}), {} file(s), {} KB{}",
        m.version,
        m.serial,
        changed.len(),
        kb(bytes),
        if retired.is_empty() { String::new() } else { format!(", {} driver(s) retired", retired.len()) }
    );
    r.say("decide", format!("update: {what}"));
    for (e, _, new) in &changed {
        r.say("decide", format!("  {}{}", e.path, if *new { " (new)" } else { "" }));
    }
    if o.dry_run {
        r.outcome = "would-update";
        r.say("done", "dry run: nothing written");
        return Ok(());
    }
    apply(&vol, &m, &state, &changed, &retired, r)?;
    r.outcome = "updated";
    r.say("done", format!("{what}; the stick's next boot runs it on trial"));
    Ok(())
}

/// The golden a stick with no `update =` belongs to (#161), and why: its
/// `media =` line where it has one (`rustnic …` or `fw`), else the drivers it
/// carries (a medium with NIC drivers is a rustnic one; the iPXE `normal`
/// media's drivers went with #91), else the firmware-drivers medium.
fn family(vol: &Medium, conf: &str) -> (&'static str, String) {
    match manifest::conf_field(conf, "media").map(|m| m.split_whitespace().next().unwrap_or("")) {
        Some("rustnic") => return ("stormbootx-rustnic", "media = rustnic".into()),
        Some("fw") => return ("stormbootx", "media = fw".into()),
        _ => {}
    }
    let drivers = vol
        .list(DRIVERS)
        .into_iter()
        .filter(|n| n.len() > 4 && n.as_bytes()[n.len() - 4..].eq_ignore_ascii_case(b".efi"))
        .count();
    if drivers > 0 {
        ("stormbootx-rustnic", format!("no media =; {drivers} NIC driver(s) on the stick"))
    } else {
        ("stormbootx", "no media = and no NIC drivers on the stick".into())
    }
}

fn current(r: &mut Report, why: &str) -> Result<(), String> {
    r.outcome = "current";
    r.say("done", format!("current: {why}"));
    Ok(())
}

/// `selfupdate::apply`: the new set beside the old, the trial recorded, then
/// the swap, the bootloader last, put back if it fails.
fn apply(
    vol: &Medium,
    m: &Manifest,
    state: &State,
    changed: &[(&Entry, Vec<u8>, bool)],
    retired: &[String],
    r: &mut Report,
) -> Result<(), String> {
    for p in changed.iter().map(|(e, _, _)| e.path).chain(retired.iter().map(String::as_str)) {
        vol.delete(&format!("{p}.prev"))?;
    }
    for (e, body, _) in changed {
        if let Err(err) = vol.write(&format!("{}.new", e.path), body) {
            for (e, _, _) in changed {
                let _ = vol.delete(&format!("{}.new", e.path));
            }
            return Err(format!("{}: {err}; nothing changed", e.path));
        }
    }
    r.say("apply", format!("{} file(s) written as .new and read back", changed.len()));
    carry_identity(vol, changed, r);

    let mut files: Vec<String> = retired.to_vec();
    files.extend(changed.iter().map(|(e, _, new)| format!("{}{}", if *new { "+" } else { "" }, e.path)));
    let files = files.join(" ");
    let trial = State { serial: m.serial, prev: state.serial, trial: Some((m.serial, 0)), files: &files, ..*state };
    vol.write_state(&trial)?;
    r.say("apply", format!("state: serial {} on trial, prev {}", m.serial, state.serial));

    let swap = || -> Result<(), String> {
        for p in retired {
            vol.rename(p, &format!("{p}.prev"))?;
        }
        for (e, _, new) in changed {
            if !new && vol.exists(e.path) {
                vol.rename(e.path, &format!("{}.prev", e.path))?;
            }
            vol.rename(&format!("{}.new", e.path), e.path)?;
        }
        Ok(())
    };
    if let Err(e) = swap() {
        let errors = revert(vol, &trial);
        let _ = vol.write_state(state);
        return Err(format!(
            "the swap failed ({e}); the previous set is back{}",
            if errors.is_empty() { String::new() } else { format!(" except {}", errors.join("; ")) }
        ));
    }
    r.say("apply", "swapped: the old files are .prev, the bootloader last");
    Ok(())
}

/// `selfupdate::revert`.
fn revert(vol: &Medium, state: &State) -> Vec<String> {
    let mut errors = Vec::new();
    for (path, new) in state.changed() {
        let prev = format!("{path}.prev");
        let _ = vol.delete(&format!("{path}.new"));
        let res = match vol.exists(&prev) {
            true => vol.delete(path).and_then(|()| vol.rename(&prev, path)),
            false if new => vol.delete(path),
            false => continue,
        };
        if let Err(e) = res {
            errors.push(format!("{path}: {e}"));
        }
    }
    errors
}

/// `selfupdate::carry_identity`: a `name =`/`tag =` the new `stormboot.conf`
/// drops moves to `local.conf`, which no update replaces.
fn carry_identity(vol: &Medium, changed: &[(&Entry, Vec<u8>, bool)], r: &mut Report) {
    let Some((_, new, _)) = changed.iter().find(|(e, _, _)| e.path.eq_ignore_ascii_case(CONF)) else {
        return;
    };
    let Ok(Some(old)) = vol.text(CONF) else { return };
    let new = String::from_utf8_lossy(new);
    let mut local = vol.text(LOCAL_CONF).ok().flatten().unwrap_or_default();
    let mut carried = Vec::new();
    for key in ["name", "tag"] {
        if let Some(v) = manifest::conf_field(&old, key) {
            if manifest::conf_field(&new, key).is_none() && manifest::conf_field(&local, key).is_none() {
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
    match vol.write(LOCAL_CONF, local.as_bytes()) {
        Ok(()) => r.say("apply", format!("kept {} in {LOCAL_CONF}", carried.join(", "))),
        Err(e) => r.say("apply", format!("could not keep {} in {LOCAL_CONF} ({e})", carried.join(", "))),
    }
}

/// One GET, `Connection: close`: the status and the body.
fn http_get(host: &str, port: u16, host_hdr: &str, path: &str, limit: usize) -> Result<(u16, Vec<u8>), String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .next()
        .ok_or_else(|| format!("{host}: no address"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| format!("{addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_hdr}\r\nUser-Agent: stormbootx-stick/{VERSION}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(|e| format!("{addr}: {e}"))?;
    let mut raw = Vec::new();
    s.take((limit + (64 << 10)) as u64).read_to_end(&mut raw).map_err(|e| format!("{addr}{path}: {e}"))?;
    let (status, body) = manifest::response(&mut raw).ok_or_else(|| format!("{addr}{path}: not an HTTP response"))?;
    if body.len() > limit {
        return Err(format!("{path}: more than {limit} bytes"));
    }
    Ok((status, body.to_vec()))
}
