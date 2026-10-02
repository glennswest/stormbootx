//! The self-update's data and decisions (#83): the signed manifest, the
//! `\stormboot\state` file, when to update, when a trial has failed, and the
//! HTTP framing of what stormcentral serves. The fetching, the signature, the
//! files and the firmware calls are `selfupdate.rs`'s.
//!
//! **The manifest** is text, one item per line, served by stormcentral at
//! `GET /api/v1/boothelpers/<golden>/current` and signed (Ed25519, over these
//! exact bytes) at `…/current.sig` (stormcentral#279):
//!
//! ```text
//! stormbootx-manifest 1
//! version 0.13.0
//! commit 1f27df3
//! golden golden-stormbootx-rustnic-0123456789abcdef
//! serial 42
//! canary 52:54:00:12:34:56
//! file <sha256 hex> <size> EFI/BOOT/BOOTX64.EFI
//! file <sha256 hex> <size> stormboot/stormboot.conf
//! file <sha256 hex> <size> stormboot/drivers/stormnic-ixgbe.efi
//! file <sha256 hex> <size> startup.nsh
//! ```
//!
//! - `serial` is stormcentral's promotion counter. It only goes up, and a
//!   medium never takes a serial at or below one it has run, or one that
//!   failed its trial on it, so a captured old manifest cannot be replayed to
//!   downgrade a fleet. A deliberate rollback is a new serial.
//! - `canary` lines, when there are any, name the only machines (by MAC) that
//!   take this serial.
//! - `file` paths are relative to the medium's root, with `/`. Only
//!   `startup.nsh`, `EFI/BOOT/*` and `stormboot/*` (not `state` or
//!   `local.conf`, which are the medium's own), and `BOOTX64.EFI` must be
//!   there.
//! - Unknown keys are ignored, so a later stormcentral can add one. A key that
//!   changes what a medium must do comes with a new format number, which this
//!   refuses.
//!
//! Text rather than JSON for the same reason as the claim reply in
//! `registry.rs`: nothing here grows a JSON parser, and this text is the one
//! a boot path must parse exactly.
//!
//! **The state file** (`\stormboot\state`) is `key = value` lines, written by
//! the update and read at every start: `serial` (the files in place), `min`
//! (the highest serial that passed its trial here), `failed` (the highest that
//! failed), `trial = <serial> <starts>` and `prev` while a trial runs, and
//! `files`, the paths the last update changed (`+` for one that had no
//! earlier copy). Absent on a medium nothing has updated: all zero.
//!
//! Same constraints as `sntp.rs`, `esp.rs` and the rest: `core` only and no
//! `crate::` item, so the tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/manifest.rs -o t/manifest-test
//! ```

/// The format this binary reads.
pub const FORMAT: &str = "stormbootx-manifest 1";

/// The most files a manifest may name.
pub const MAX_FILES: usize = 32;

/// The most canary MACs a manifest may name.
pub const MAX_CANARIES: usize = 32;

/// The largest file a manifest may name. The binary is ~250 KB and a NIC
/// driver a few hundred; this is room, not a target.
pub const MAX_FILE: u64 = 8 << 20;

/// The most bytes one update may carry, all files together.
pub const MAX_TOTAL: u64 = 24 << 20;

/// The largest manifest read.
pub const MAX_MANIFEST: usize = 16 << 10;

/// Starts a new set gets to reach an attach before it is put back.
pub const TRIAL_STARTS: u32 = 2;

/// The bootloader every manifest must carry.
pub const BOOTLOADER: &str = "EFI/BOOT/BOOTX64.EFI";

/// stormcentral's release keys, current and next: the Ed25519 public halves
/// a manifest's signature must verify against (stormcentral#279, #86). The
/// private half never leaves the stormcentral VM; stormcentral serves these
/// open at `/api/v1/stormbootx/keys`. A new key is added here as the next
/// one a release before it signs anything, and the old one dropped a release
/// after.
pub const RELEASE_KEYS: &[[u8; 32]] = &[
    // 4fe28c027b8cf1e8a0a2b6195f10ef8faef0cc49df212638e205eee10a171bce
    [
        0x4f, 0xe2, 0x8c, 0x02, 0x7b, 0x8c, 0xf1, 0xe8,
        0xa0, 0xa2, 0xb6, 0x19, 0x5f, 0x10, 0xef, 0x8f,
        0xae, 0xf0, 0xcc, 0x49, 0xdf, 0x21, 0x26, 0x38,
        0xe2, 0x05, 0xee, 0xe1, 0x0a, 0x17, 0x1b, 0xce,
    ],
];

/// One file the manifest names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub sha256: [u8; 32],
    pub size: u64,
    pub path: &'a str,
}

const NO_ENTRY: Entry<'static> = Entry { sha256: [0; 32], size: 0, path: "" };

/// A parsed manifest, borrowing the text it came from.
#[derive(Debug)]
pub struct Manifest<'a> {
    pub version: &'a str,
    pub commit: &'a str,
    pub golden: &'a str,
    pub serial: u64,
    files: [Entry<'a>; MAX_FILES],
    n_files: usize,
    canaries: [&'a str; MAX_CANARIES],
    n_canaries: usize,
}

impl<'a> Manifest<'a> {
    pub fn files(&self) -> &[Entry<'a>] {
        &self.files[..self.n_files]
    }

    pub fn canaries(&self) -> &[&'a str] {
        &self.canaries[..self.n_canaries]
    }

    /// Is this machine (its MAC, `aa:bb:…`) one the manifest is for? Every
    /// machine when it names no canaries.
    pub fn for_machine(&self, mac: Option<&str>) -> bool {
        self.n_canaries == 0
            || mac.is_some_and(|m| self.canaries().iter().any(|c| c.eq_ignore_ascii_case(m)))
    }

    /// The entry for `path`, compared as FAT compares names.
    pub fn file(&self, path: &str) -> Option<&Entry<'a>> {
        self.files().iter().find(|f| f.path.eq_ignore_ascii_case(path))
    }
}

/// Why a manifest was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bad {
    NotText,
    Format,
    NoSerial,
    NoVersion,
    BadLine,
    BadPath,
    Duplicate,
    TooMany,
    TooBig,
    NoBootloader,
}

impl Bad {
    pub fn text(self) -> &'static str {
        match self {
            Bad::NotText => "not text",
            Bad::Format => "not a stormbootx-manifest 1",
            Bad::NoSerial => "no serial",
            Bad::NoVersion => "no version",
            Bad::BadLine => "a file line is not <sha256> <size> <path>",
            Bad::BadPath => "a file path is outside what an update may write",
            Bad::Duplicate => "a file is named twice",
            Bad::TooMany => "too many files or canaries",
            Bad::TooBig => "a file or the whole update is too big",
            Bad::NoBootloader => "no EFI/BOOT/BOOTX64.EFI",
        }
    }
}

/// Parse a manifest. Call only on bytes whose signature has been verified.
pub fn parse(bytes: &[u8]) -> Result<Manifest<'_>, Bad> {
    let text = core::str::from_utf8(bytes).map_err(|_| Bad::NotText)?;
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    if lines.next() != Some(FORMAT) {
        return Err(Bad::Format);
    }
    let mut m = Manifest {
        version: "",
        commit: "",
        golden: "",
        serial: 0,
        files: [NO_ENTRY; MAX_FILES],
        n_files: 0,
        canaries: [""; MAX_CANARIES],
        n_canaries: 0,
    };
    let mut total = 0u64;
    for line in lines {
        let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        match key {
            "version" => m.version = rest,
            "commit" => m.commit = rest,
            "golden" => m.golden = rest,
            "serial" => m.serial = rest.parse().map_err(|_| Bad::NoSerial)?,
            "canary" => {
                if m.n_canaries == MAX_CANARIES {
                    return Err(Bad::TooMany);
                }
                m.canaries[m.n_canaries] = rest;
                m.n_canaries += 1;
            }
            "file" => {
                let mut f = rest.split_whitespace();
                let (Some(h), Some(n), Some(p), None) = (f.next(), f.next(), f.next(), f.next()) else {
                    return Err(Bad::BadLine);
                };
                let sha256 = hex32(h).ok_or(Bad::BadLine)?;
                let size: u64 = n.parse().map_err(|_| Bad::BadLine)?;
                if !path_ok(p) {
                    return Err(Bad::BadPath);
                }
                if m.file(p).is_some() {
                    return Err(Bad::Duplicate);
                }
                if m.n_files == MAX_FILES {
                    return Err(Bad::TooMany);
                }
                if size > MAX_FILE {
                    return Err(Bad::TooBig);
                }
                total += size;
                if total > MAX_TOTAL {
                    return Err(Bad::TooBig);
                }
                m.files[m.n_files] = Entry { sha256, size, path: p };
                m.n_files += 1;
            }
            _ => {}
        }
    }
    if m.serial == 0 {
        return Err(Bad::NoSerial);
    }
    if m.version.is_empty() {
        return Err(Bad::NoVersion);
    }
    if m.file(BOOTLOADER).is_none() {
        return Err(Bad::NoBootloader);
    }
    Ok(m)
}

/// May an update write this path? `startup.nsh`, `EFI/BOOT/<file>`, or
/// `stormboot/<file>` and `stormboot/<dir>/<file>`, but never the medium's own
/// `stormboot/state` or `stormboot/local.conf`, and never a name the update
/// itself uses (`*.new`, `*.prev`). Components are `[A-Za-z0-9._-]`, start
/// with neither `.` nor `-`.
pub fn path_ok(p: &str) -> bool {
    if p.is_empty() || p.len() > 128 {
        return false;
    }
    let mut parts = [""; 4];
    let mut n = 0;
    for c in p.split('/') {
        if n == parts.len() || c.is_empty() || c.starts_with(['.', '-']) {
            return false;
        }
        if !c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-') {
            return false;
        }
        parts[n] = c;
        n += 1;
    }
    let last = parts[n - 1];
    let suffix = |s: &str| last.len() > s.len() && last[last.len() - s.len()..].eq_ignore_ascii_case(s);
    if suffix(".new") || suffix(".prev") {
        return false;
    }
    let is = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    match n {
        1 => is(parts[0], "startup.nsh"),
        3 if is(parts[0], "EFI") => is(parts[1], "BOOT"),
        2 | 3 if is(parts[0], "stormboot") => {
            !(n == 2 && (is(parts[1], "state") || is(parts[1], "local.conf")))
        }
        _ => false,
    }
}

/// 64 hex digits.
pub fn hex32(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    hex_into(s, &mut out)?;
    Some(out)
}

fn hex_into(s: &str, out: &mut [u8]) -> Option<()> {
    let b = s.as_bytes();
    if b.len() != out.len() * 2 {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    for (i, o) in out.iter_mut().enumerate() {
        *o = nib(b[2 * i])? << 4 | nib(b[2 * i + 1])?;
    }
    Some(())
}

/// The signature as served: 64 raw bytes, or 128 hex digits (whitespace
/// around them ignored).
pub fn signature(body: &[u8]) -> Option<[u8; 64]> {
    let mut out = [0u8; 64];
    if body.len() == 64 {
        out.copy_from_slice(body);
        return Some(out);
    }
    hex_into(core::str::from_utf8(body).ok()?.trim(), &mut out)?;
    Some(out)
}

/// The medium's update state (`\stormboot\state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State<'a> {
    /// The serial of the files in place; 0 when nothing has updated them.
    pub serial: u64,
    /// The highest serial that passed its trial here.
    pub min: u64,
    /// The highest serial that failed its trial here.
    pub failed: u64,
    /// The serial on trial and the starts it has had.
    pub trial: Option<(u64, u32)>,
    /// The serial the trial replaced, to go back to.
    pub prev: u64,
    /// The paths the last update changed; `+` marks one that was new.
    pub files: &'a str,
}

impl<'a> State<'a> {
    /// Read a state file. Anything unreadable reads as 0, which can only make
    /// the medium more willing to take a signed update, never a replay of
    /// one it has seen fail (that is also in the NV variable).
    pub fn parse(text: &'a str) -> State<'a> {
        let mut s = State::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "serial" => s.serial = v.parse().unwrap_or(0),
                "min" => s.min = v.parse().unwrap_or(0),
                "failed" => s.failed = v.parse().unwrap_or(0),
                "prev" => s.prev = v.parse().unwrap_or(0),
                "files" => s.files = v,
                "trial" => {
                    let mut t = v.split_whitespace();
                    s.trial = match (t.next().map(str::parse), t.next().map(str::parse)) {
                        (Some(Ok(serial)), Some(Ok(starts))) => Some((serial, starts)),
                        _ => None,
                    };
                }
                _ => {}
            }
        }
        s
    }

    /// The paths the last update changed, each with whether it was new.
    pub fn changed(&self) -> impl Iterator<Item = (&'a str, bool)> {
        self.files
            .split_whitespace()
            .map(|p| match p.strip_prefix('+') {
                Some(p) => (p, true),
                None => (p, false),
            })
            .filter(|(p, _)| path_ok(p))
    }

    /// The file's text. `out` must hold it; 160 bytes plus the file list.
    pub fn render(&self, out: &mut [u8]) -> Option<usize> {
        let mut w = Buf { out, len: 0 };
        use core::fmt::Write;
        write!(w, "# stormbootx self-update state (#83). Written by stormbootx; do not edit.\n").ok()?;
        write!(w, "serial = {}\nmin = {}\nfailed = {}\n", self.serial, self.min, self.failed).ok()?;
        if let Some((serial, starts)) = self.trial {
            write!(w, "trial = {serial} {starts}\nprev = {}\n", self.prev).ok()?;
        }
        if !self.files.is_empty() {
            write!(w, "files = {}\n", self.files).ok()?;
        }
        Some(w.len)
    }
}

struct Buf<'b> {
    out: &'b mut [u8],
    len: usize,
}

impl core::fmt::Write for Buf<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        if end > self.out.len() {
            return Err(core::fmt::Error);
        }
        self.out[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// What a start does about a trial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// No trial: boot on.
    Normal,
    /// A trial, and this is its `start`th start (1-based). The caller records
    /// it before going on.
    Trial { serial: u64, start: u32 },
    /// The set on trial had its starts and never reached an attach: put the
    /// previous set back.
    Revert { serial: u64, back_to: u64 },
}

pub fn at_start(s: &State) -> Start {
    match s.trial {
        None => Start::Normal,
        Some((serial, starts)) if starts >= TRIAL_STARTS => Start::Revert { serial, back_to: s.prev },
        Some((serial, starts)) => Start::Trial { serial, start: starts + 1 },
    }
}

/// What to do with a verified manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The medium already carries this serial.
    Current,
    /// Older than what this medium (or this machine's NV) has run.
    Older { have: u64 },
    /// This serial (or a later one) failed its trial on this medium.
    FailedHere,
    /// Canaries only, and this machine is not one.
    NotACanary,
    /// A trial is still running; nothing changes until it ends.
    InTrial,
    /// Take it.
    Update,
}

/// Decide on a verified manifest's serial. `nv_min` is the machine's
/// `StormBootMinSerial` (the highest serial that passed a trial on it, on any
/// medium), `canary` whether the manifest is for this machine.
pub fn verdict(serial: u64, s: &State, nv_min: u64, canary: bool) -> Verdict {
    if s.trial.is_some() {
        return Verdict::InTrial;
    }
    if serial == s.serial {
        return Verdict::Current;
    }
    // The medium never takes a serial at or below one it has carried. The
    // machine's NV minimum stops a replay below what the machine has run, but
    // a stale stick may still take the serial the machine already runs.
    let medium = s.serial.max(s.min);
    if serial <= medium {
        return Verdict::Older { have: medium };
    }
    if serial < nv_min {
        return Verdict::Older { have: nv_min };
    }
    if serial <= s.failed {
        return Verdict::FailedHere;
    }
    if !canary {
        return Verdict::NotACanary;
    }
    Verdict::Update
}

/// An HTTP/1.1 response: status, and the body with chunking undone in place.
/// `None` for anything that is not one, or a body shorter than its
/// Content-Length (a connection that dropped).
pub fn response(raw: &mut [u8]) -> Option<(u16, &[u8])> {
    let head_end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = core::str::from_utf8(&raw[..head_end]).ok()?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let mut chunked = false;
    let mut length = None;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked") {
            chunked = true;
        } else if k.eq_ignore_ascii_case("content-length") {
            length = Some(v.parse::<usize>().ok()?);
        }
    }
    let body = &mut raw[head_end + 4..];
    if chunked {
        let n = dechunk(body)?;
        return Some((status, &body[..n]));
    }
    match length {
        Some(n) if body.len() < n => None,
        Some(n) => Some((status, &body[..n])),
        None => Some((status, body)),
    }
}

/// Undo chunked transfer coding in place; the length of what is left.
fn dechunk(buf: &mut [u8]) -> Option<usize> {
    let (mut rd, mut wr) = (0usize, 0usize);
    loop {
        let eol = buf[rd..].windows(2).position(|w| w == b"\r\n")? + rd;
        let line = core::str::from_utf8(&buf[rd..eol]).ok()?;
        let size = usize::from_str_radix(line.split(';').next()?.trim(), 16).ok()?;
        rd = eol + 2;
        if size == 0 {
            return Some(wr);
        }
        if buf.len() < rd + size + 2 || &buf[rd + size..rd + size + 2] != b"\r\n" {
            return None;
        }
        buf.copy_within(rd..rd + size, wr);
        wr += size;
        rd += size + 2;
    }
}

/// `update =` from `stormboot.conf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting<'a> {
    /// `off`, `false`, `no`, `0`: this medium is pinned.
    Off,
    /// `http://host[:port]/path`: where `current`, `current.sig` and `files/`
    /// are.
    At { host: &'a str, port: u16, path: &'a str },
}

/// Read an `update =` value. `None` for one that is neither.
pub fn setting(v: &str) -> Option<Setting<'_>> {
    let v = v.trim();
    if matches!(v, "off" | "false" | "no" | "0") {
        return Some(Setting::Off);
    }
    let rest = v.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
        None => (rest, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, 80),
    };
    if host.is_empty() || port == 0 || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        return None;
    }
    Some(Setting::At { host, port, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(c: char) -> std::string::String {
        core::iter::repeat(c).take(64).collect()
    }

    fn sample() -> std::string::String {
        format!(
            "stormbootx-manifest 1\n\
             version 0.13.0\n\
             commit 1f27df3\n\
             golden golden-stormbootx-rustnic-0123\n\
             serial 42\n\
             future-key whatever\n\
             file {} 241664 EFI/BOOT/BOOTX64.EFI\n\
             file {} 812 stormboot/stormboot.conf\n\
             file {} 160000 stormboot/drivers/stormnic-ixgbe.efi\n\
             file {} 410 startup.nsh\n",
            sha('a'),
            sha('B'),
            sha('3'),
            sha('0')
        )
    }

    #[test]
    fn a_manifest_reads() {
        let text = sample();
        let m = parse(text.as_bytes()).unwrap();
        assert_eq!((m.version, m.commit, m.serial), ("0.13.0", "1f27df3", 42));
        assert_eq!(m.golden, "golden-stormbootx-rustnic-0123");
        assert_eq!(m.files().len(), 4);
        assert_eq!(m.files()[0].sha256, [0xaa; 32]);
        assert_eq!(m.files()[1].sha256, [0xbb; 32]);
        assert_eq!(m.file("efi/boot/bootx64.efi").unwrap().size, 241664);
        assert!(m.for_machine(None), "no canaries: every machine");
    }

    #[test]
    fn canaries_name_the_machines() {
        let text = sample() + "canary 52:54:00:12:34:56\ncanary AC:1F:6B:8A:A4:5C\n";
        let m = parse(text.as_bytes()).unwrap();
        assert!(m.for_machine(Some("ac:1f:6b:8a:a4:5c")));
        assert!(m.for_machine(Some("52:54:00:12:34:56")));
        assert!(!m.for_machine(Some("52:54:00:12:34:57")));
        assert!(!m.for_machine(None));
    }

    #[test]
    fn bad_manifests_are_refused() {
        let good = sample();
        let with = |from: &str, to: &str| good.replace(from, to);
        assert_eq!(parse(b"\xff\xfe").unwrap_err(), Bad::NotText);
        assert_eq!(parse(with("manifest 1", "manifest 2").as_bytes()).unwrap_err(), Bad::Format);
        assert_eq!(parse(with("serial 42", "serial x").as_bytes()).unwrap_err(), Bad::NoSerial);
        assert_eq!(parse(with("serial 42\n", "").as_bytes()).unwrap_err(), Bad::NoSerial);
        assert_eq!(parse(with("version 0.13.0\n", "").as_bytes()).unwrap_err(), Bad::NoVersion);
        assert_eq!(parse(with(" 812 ", " 8x2 ").as_bytes()).unwrap_err(), Bad::BadLine);
        assert_eq!(parse(with(" 812 stormboot/stormboot.conf", " 812").as_bytes()).unwrap_err(), Bad::BadLine);
        assert_eq!(
            parse(with("stormboot/stormboot.conf", "stormboot/state").as_bytes()).unwrap_err(),
            Bad::BadPath
        );
        assert_eq!(parse(with("startup.nsh", "../startup.nsh").as_bytes()).unwrap_err(), Bad::BadPath);
        assert_eq!(parse(with("startup.nsh", "EFI/BOOT/bootx64.efi").as_bytes()).unwrap_err(), Bad::Duplicate);
        assert_eq!(parse(with("160000", "9000000").as_bytes()).unwrap_err(), Bad::TooBig);
        assert_eq!(
            parse(with("EFI/BOOT/BOOTX64.EFI", "EFI/BOOT/GRUBX64.EFI").as_bytes()).unwrap_err(),
            Bad::NoBootloader
        );
        let mut many = good.clone();
        for i in 0..MAX_FILES {
            many += &format!("file {} 1 stormboot/drivers/d{i}.efi\n", sha('c'));
        }
        assert_eq!(parse(many.as_bytes()).unwrap_err(), Bad::TooMany);
    }

    #[test]
    fn only_the_media_files_may_be_written() {
        for ok in [
            "EFI/BOOT/BOOTX64.EFI",
            "EFI/BOOT/bootx64.efi",
            "startup.nsh",
            "stormboot/stormboot.conf",
            "stormboot/drivers/stormnic-ixgbe.efi",
            "stormboot/drivers/stormnic-mlx4.efi.off",
            "stormboot/drivers/STORMNIC-SOURCE.txt",
        ] {
            assert!(path_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "/EFI/BOOT/BOOTX64.EFI",
            "EFI/BOOT/",
            "EFI//BOOTX64.EFI",
            "EFI/Microsoft/Boot/bootmgfw.efi",
            "EFI/BOOT/BOOTX64.EFI.new",
            "EFI/BOOT/BOOTX64.EFI.prev",
            "stormboot/state",
            "STORMBOOT/LOCAL.CONF",
            "stormboot/a/b/c",
            "stormboot/../EFI/x",
            "stormboot/.hidden",
            "stormboot/-x",
            "stormboot/drivers/a b.efi",
            "stormboot\\drivers\\x.efi",
            "other.nsh",
            "esp.img",
        ] {
            assert!(!path_ok(bad), "{bad}");
        }
    }

    #[test]
    fn signatures_read_raw_or_hex() {
        assert_eq!(signature(&[7u8; 64]), Some([7u8; 64]));
        let hex: std::string::String = core::iter::repeat("0f").take(64).collect();
        assert_eq!(signature(format!("{hex}\n").as_bytes()), Some([0x0f; 64]));
        assert_eq!(signature(&hex.as_bytes()[..126]), None);
        assert_eq!(signature(b"not a signature"), None);
    }

    #[test]
    fn state_round_trips() {
        let s = State {
            serial: 5,
            min: 5,
            failed: 3,
            trial: Some((6, 1)),
            prev: 5,
            files: "EFI/BOOT/BOOTX64.EFI +stormboot/drivers/new.efi",
        };
        let mut buf = [0u8; 512];
        let n = s.render(&mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert_eq!(State::parse(text), s);
        let changed: std::vec::Vec<_> = s.changed().collect();
        assert_eq!(changed, [("EFI/BOOT/BOOTX64.EFI", false), ("stormboot/drivers/new.efi", true)]);
        assert_eq!(State::parse(""), State::default());
        assert_eq!(State::parse("trial = 6\nserial = x").trial, None);
        assert!(s.render(&mut [0u8; 40]).is_none(), "a short buffer is an error, not a cut file");
    }

    #[test]
    fn a_trial_gets_two_starts() {
        let mut s = State { serial: 6, prev: 5, trial: Some((6, 0)), ..State::default() };
        assert_eq!(at_start(&s), Start::Trial { serial: 6, start: 1 });
        s.trial = Some((6, 1));
        assert_eq!(at_start(&s), Start::Trial { serial: 6, start: 2 });
        s.trial = Some((6, 2));
        assert_eq!(at_start(&s), Start::Revert { serial: 6, back_to: 5 });
        assert_eq!(at_start(&State::default()), Start::Normal);
    }

    #[test]
    fn serials_only_go_up() {
        let s = State { serial: 5, min: 5, failed: 6, ..State::default() };
        assert_eq!(verdict(5, &s, 0, true), Verdict::Current);
        assert_eq!(verdict(4, &s, 0, true), Verdict::Older { have: 5 });
        assert_eq!(verdict(6, &s, 0, true), Verdict::FailedHere);
        assert_eq!(verdict(7, &s, 0, true), Verdict::Update);
        assert_eq!(verdict(7, &s, 0, false), Verdict::NotACanary);
        // The machine's NV minimum counts too: a fresh stick in a machine that
        // has run serial 9 does not take an older, validly signed 7.
        let fresh = State::default();
        assert_eq!(verdict(7, &fresh, 9, true), Verdict::Older { have: 9 });
        assert_eq!(verdict(9, &fresh, 9, true), Verdict::Update, "a stale stick catches up");
        assert_eq!(verdict(10, &fresh, 9, true), Verdict::Update);
        let trial = State { trial: Some((7, 1)), ..s };
        assert_eq!(verdict(8, &trial, 0, true), Verdict::InTrial);
    }

    #[test]
    fn responses_frame() {
        let mut raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_vec();
        assert_eq!(response(&mut raw), Some((200, &b"hello"[..])));
        let mut short = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nhello".to_vec();
        assert_eq!(response(&mut short), None, "a body cut short is not a body");
        let mut chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n7;x=1\r\n, world\r\n0\r\n\r\n".to_vec();
        assert_eq!(response(&mut chunked), Some((200, &b"hello, world"[..])));
        let mut torn = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel".to_vec();
        assert_eq!(response(&mut torn), None);
        let mut nf = b"HTTP/1.1 404 Not Found\r\n\r\n{}".to_vec();
        assert_eq!(response(&mut nf), Some((404, &b"{}"[..])));
        assert_eq!(response(&mut b"garbage".to_vec()), None);
    }

    #[test]
    fn update_settings() {
        assert_eq!(setting("off"), Some(Setting::Off));
        assert_eq!(setting("false"), Some(Setting::Off));
        assert_eq!(
            setting("http://stormcentral.g8.lo/api/v1/boothelpers/stormbootx-rustnic/"),
            Some(Setting::At {
                host: "stormcentral.g8.lo",
                port: 80,
                path: "/api/v1/boothelpers/stormbootx-rustnic"
            })
        );
        assert_eq!(
            setting("http://10.0.2.2:8080/x"),
            Some(Setting::At { host: "10.0.2.2", port: 8080, path: "/x" })
        );
        assert_eq!(setting("https://stormcentral.g8.lo/x"), None);
        assert_eq!(setting("http://:80/x"), None);
        assert_eq!(setting("http://a b/x"), None);
        assert_eq!(setting("yes"), None);
    }

    #[test]
    fn the_release_key_is_stormcentrals() {
        // As posted on #86 and served at /api/v1/stormbootx/keys.
        assert_eq!(
            RELEASE_KEYS,
            &[hex32("4fe28c027b8cf1e8a0a2b6195f10ef8faef0cc49df212638e205eee10a171bce").unwrap()]
        );
    }
}
