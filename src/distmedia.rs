//! A stormdist release medium (#198): the ISO 9660 it is, the signed
//! `BOOT.toml` that binds its boot payload, and the read-rate line.
//!
//! stormdist's full release ISO (stormdist#10, written by mkfs.iso.rs) is
//! bootable: El Torito UEFI and BIOS entries and a hybrid GPT, both pointing
//! at `/boot/esp.img`, the release's own ESP (stormuefi, its kernel and the
//! stormblock initramfs). Written to a stick it is the same bytes at
//! 512-byte blocks. At its root:
//!
//! ```text
//! /RELEASE.toml   version, flavours, members by digest
//! /RELEASE.sig    Ed25519 over RELEASE.toml's exact bytes (64 raw bytes)
//! /BOOT.toml      release_sha256, esp_sha256, bios_sha256 [, mbr_sha256]
//! /BOOT.sig       Ed25519 over BOOT.toml's exact bytes
//! /boot/esp.img
//! ```
//!
//! stormbootx finds such a medium, checks both signatures against the
//! release key (`DIST_KEYS`, stormcentral's `/api/v1/release/keys`: not the
//! boothelper key `manifest::RELEASE_KEYS`), checks `RELEASE.toml` and the
//! ESP against `BOOT.toml`'s digests, and starts the ESP's bootloader: the
//! release then installs itself from the medium (stormblock#442). Nothing
//! here lays a disk (the owner on #166: one layout writer, in the initramfs).
//!
//! The ISO is read through its primary volume's directory records, whose
//! names mkfs.iso.rs writes upper-cased with a version (`RELEASE.TOML;1`,
//! `BOOT`, `ESP.IMG;1`); Rock Ridge, Joliet and UDF are not needed for five
//! fixed names. Matching ignores case and the `;1`.
//!
//! `core` and `alloc` only, and nothing from the rest of the crate, so the
//! host tests run with `rustc --edition 2021 --test src/distmedia.rs`. The
//! signature check is `localinstall.rs`'s (ed25519-compact), the SHA-256
//! `sha256.rs`'s.

extern crate alloc;

use alloc::format;
use alloc::string::String;

/// stormcentral's release keys: what stormdist media are signed with
/// (`stormdist make --signing-key`, stormcentral `releasekey.rs`, #876,
/// #896), served open at `/api/v1/release/keys`. A rotation adds the next
/// key here a release before it signs anything.
pub const DIST_KEYS: &[[u8; 32]] = &[
    // 43d06026a4819f1eb362b6ceab13cf461878cb5d5f54d9168b45ea256248afd8
    [
        0x43, 0xd0, 0x60, 0x26, 0xa4, 0x81, 0x9f, 0x1e,
        0xb3, 0x62, 0xb6, 0xce, 0xab, 0x13, 0xcf, 0x46,
        0x18, 0x78, 0xcb, 0x5d, 0x5f, 0x54, 0xd9, 0x16,
        0x8b, 0x45, 0xea, 0x25, 0x62, 0x48, 0xaf, 0xd8,
    ],
];

/// The ISO 9660 logical sector, and where the volume descriptors start.
pub const SECTOR: u64 = 2048;
const FIRST_VD: u64 = 16;
/// Descriptors read before giving up on a terminator.
const MAX_VDS: u64 = 32;
/// A directory larger than this is not a release's root or `boot/`.
const MAX_DIR: u64 = 1 << 20;

/// The few small files are read whole; anything larger is refused.
pub const META_LIMIT: u64 = 16 << 20;
/// stormdist's own bound on `boot/esp.img` (`media.rs`).
pub const ESP_LIMIT: u64 = 128 << 20;

/// Byte reads from the medium at any offset.
pub trait Medium {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> bool;
}

/// Why a medium is not a stormdist one, or is one that fails a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No primary volume descriptor: not ISO 9660 at all (most disks).
    NotIso,
    /// ISO 9660 without a `RELEASE.TOML`: some other CD (this medium's own).
    NotRelease,
    /// A stormdist medium with no `BOOT.TOML`: data media (a diff, a backup).
    NotBootable,
    /// A read failed or a structure is broken.
    Broken(String),
}

impl Error {
    pub fn describe(&self) -> String {
        match self {
            Error::NotIso => String::from("not ISO 9660"),
            Error::NotRelease => String::from("ISO 9660 with no RELEASE.toml"),
            Error::NotBootable => String::from("a stormdist medium with no BOOT.toml (data media: a diff or a backup)"),
            Error::Broken(s) => s.clone(),
        }
    }
}

fn broken(s: &str) -> Error {
    Error::Broken(String::from(s))
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A file's bytes on the medium: one extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub offset: u64,
    pub len: u64,
    pub dir: bool,
}

/// An ISO 9660 primary volume: its root directory.
#[derive(Debug, Clone, Copy)]
pub struct Iso {
    root: Extent,
}

impl Iso {
    /// The primary volume descriptor, from sector 16 on.
    pub fn open<M: Medium>(m: &mut M) -> Result<Iso, Error> {
        let mut vd = [0u8; SECTOR as usize];
        for i in 0..MAX_VDS {
            if !m.read((FIRST_VD + i) * SECTOR, &mut vd) {
                return Err(if i == 0 { Error::NotIso } else { broken("a volume descriptor could not be read") });
            }
            if &vd[1..6] != b"CD001" {
                return Err(if i == 0 { Error::NotIso } else { broken("a volume descriptor without CD001") });
            }
            match vd[0] {
                1 => {
                    if u16::from_le_bytes([vd[128], vd[129]]) as u64 != SECTOR {
                        return Err(broken("a logical block size other than 2048"));
                    }
                    let root = record(&vd[156..190]).ok_or_else(|| broken("the root directory record is broken"))?;
                    if !root.0.dir {
                        return Err(broken("the root directory record is not a directory"));
                    }
                    return Ok(Iso { root: root.0 });
                }
                255 => return Err(broken("no primary volume descriptor")),
                _ => {}
            }
        }
        Err(broken("no volume descriptor terminator"))
    }

    /// A path from the root (`BOOT/ESP.IMG`, any case).
    pub fn find<M: Medium>(&self, m: &mut M, path: &str) -> Result<Option<Extent>, Error> {
        let mut at = self.root;
        let mut parts = path.split('/').filter(|p| !p.is_empty()).peekable();
        while let Some(part) = parts.next() {
            if !at.dir {
                return Ok(None);
            }
            match lookup(m, at, part)? {
                Some(e) => at = e,
                None => return Ok(None),
            }
            if parts.peek().is_none() {
                return Ok(Some(at));
            }
        }
        Ok(Some(at))
    }
}

/// One directory record: its extent and its identifier's range in `r`.
fn record(r: &[u8]) -> Option<(Extent, usize, usize)> {
    let len = *r.first()? as usize;
    if len < 34 || len > r.len() {
        return None;
    }
    let name_len = r[32] as usize;
    if 33 + name_len > len {
        return None;
    }
    let flags = r[25];
    Some((
        Extent { offset: le32(r, 2) as u64 * SECTOR, len: le32(r, 10) as u64, dir: flags & 0x02 != 0 },
        33,
        33 + name_len,
    ))
}

/// Whether a recorded identifier names `want`: case ignored, the version
/// (`;1`) and a bare trailing dot dropped.
pub fn name_is(id: &[u8], want: &str) -> bool {
    let id = match id.iter().position(|&b| b == b';') {
        Some(p) => &id[..p],
        None => id,
    };
    let id = id.strip_suffix(b".").unwrap_or(id);
    id.eq_ignore_ascii_case(want.as_bytes())
}

fn lookup<M: Medium>(m: &mut M, dir: Extent, want: &str) -> Result<Option<Extent>, Error> {
    if dir.len > MAX_DIR {
        return Err(broken("a directory larger than 1 MiB"));
    }
    let mut buf = alloc::vec![0u8; dir.len as usize];
    if !m.read(dir.offset, &mut buf) {
        return Err(broken("a directory could not be read"));
    }
    // Records never cross a sector: a zero length means the rest of the
    // sector is padding.
    let mut at = 0usize;
    while at < buf.len() {
        let len = buf[at] as usize;
        if len == 0 {
            at = (at / SECTOR as usize + 1) * SECTOR as usize;
            continue;
        }
        let Some((e, a, b)) = record(&buf[at..]) else {
            return Err(broken("a directory record is broken"));
        };
        let id = &buf[at + a..at + b];
        // `.` and `..` are 0x00 and 0x01.
        if !(id == [0] || id == [1]) && name_is(id, want) {
            // A file of several extents (flag 0x80) is not one of these.
            if buf[at + 25] & 0x80 != 0 {
                return Err(Error::Broken(format!("{want} is recorded in several extents")));
            }
            return Ok(Some(e));
        }
        at += len;
    }
    Ok(None)
}

/// Where a stormdist medium keeps what the boot checks.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub release_toml: Extent,
    pub release_sig: Extent,
    pub boot_toml: Extent,
    pub boot_sig: Extent,
    pub esp: Extent,
}

/// Find a bootable stormdist medium's files. Errors say how far it got:
/// `NotIso` and `NotRelease` are ordinary disks and CDs.
pub fn layout<M: Medium>(m: &mut M) -> Result<Layout, Error> {
    let iso = Iso::open(m)?;
    let file = |m: &mut M, path: &str| -> Result<Option<Extent>, Error> {
        match iso.find(m, path)? {
            Some(e) if e.dir => Err(Error::Broken(format!("{path} is a directory"))),
            other => Ok(other),
        }
    };
    let release_toml = file(m, "RELEASE.TOML")?.ok_or(Error::NotRelease)?;
    let boot_toml = file(m, "BOOT.TOML")?.ok_or(Error::NotBootable)?;
    let need = |e: Option<Extent>, what: &str| e.ok_or_else(|| Error::Broken(format!("a bootable stormdist medium with no {what}")));
    let release_sig = need(file(m, "RELEASE.SIG")?, "RELEASE.sig")?;
    let boot_sig = need(file(m, "BOOT.SIG")?, "BOOT.sig")?;
    let esp = need(file(m, "BOOT/ESP.IMG")?, "boot/esp.img")?;
    for (e, what, limit) in [
        (release_toml, "RELEASE.toml", META_LIMIT),
        (boot_toml, "BOOT.toml", META_LIMIT),
        (esp, "boot/esp.img", ESP_LIMIT),
    ] {
        if e.len == 0 || e.len > limit {
            return Err(Error::Broken(format!("{what} is {} bytes (1..={limit})", e.len)));
        }
    }
    for (e, what) in [(release_sig, "RELEASE.sig"), (boot_sig, "BOOT.sig")] {
        if e.len != 64 {
            return Err(Error::Broken(format!("{what} is {} bytes, not a 64-byte Ed25519 signature", e.len)));
        }
    }
    Ok(Layout { release_toml, release_sig, boot_toml, boot_sig, esp })
}

/// `BOOT.toml`: the digests that bind the boot payload to the release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boot {
    pub release_sha256: [u8; 32],
    pub esp_sha256: [u8; 32],
    pub bios_sha256: [u8; 32],
    pub mbr_sha256: Option<[u8; 32]>,
}

/// Parse `BOOT.toml` as stormdist writes it (`key = "<64 hex>"` per line).
/// Strict, as its reader is (`deny_unknown_fields`): an unknown key, a key
/// twice or a missing one is refused. Parsed only after its signature
/// verified.
pub fn parse_boot(text: &str) -> Result<Boot, String> {
    let (mut release, mut esp, mut bios, mut mbr) = (None, None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line.split_once('=').ok_or_else(|| format!("BOOT.toml: not `key = value`: {line}"))?;
        let (k, v) = (k.trim(), v.trim());
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or_else(|| format!("BOOT.toml: {k} is not a string"))?;
        let h = hex32(v).ok_or_else(|| format!("BOOT.toml: {k} is not 64 hex digits"))?;
        let slot = match k {
            "release_sha256" => &mut release,
            "esp_sha256" => &mut esp,
            "bios_sha256" => &mut bios,
            "mbr_sha256" => &mut mbr,
            _ => return Err(format!("BOOT.toml: unknown key {k}")),
        };
        if slot.replace(h).is_some() {
            return Err(format!("BOOT.toml: {k} twice"));
        }
    }
    let need = |v: Option<[u8; 32]>, k: &str| v.ok_or_else(|| format!("BOOT.toml: no {k}"));
    Ok(Boot {
        release_sha256: need(release, "release_sha256")?,
        esp_sha256: need(esp, "esp_sha256")?,
        bios_sha256: need(bios, "bios_sha256")?,
        mbr_sha256: mbr,
    })
}

/// What the console says of a release: its `version`, `kind` and the
/// `bytes` of chunks it carries, read from `RELEASE.toml`'s top-level keys
/// (before its first table). Only after its signature verified, and only
/// for the console: nothing decides on them here.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Option<String>,
    pub kind: Option<String>,
    pub bytes: Option<u64>,
}

pub fn parse_release(text: &str) -> Release {
    let mut r = Release::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        let s = v.strip_prefix('"').and_then(|v| v.strip_suffix('"'));
        match (k, s) {
            ("version", Some(s)) if printable(s) => r.version = Some(String::from(s)),
            ("kind", Some(s)) if printable(s) => r.kind = Some(String::from(s)),
            ("bytes", None) => r.bytes = v.parse().ok(),
            _ => {}
        }
    }
    r
}

fn printable(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_graphic())
}

pub fn hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (nib(b[2 * i])? << 4) | nib(b[2 * i + 1])?;
    }
    Some(out)
}

/// MiB/s from bytes and microseconds; 0 when nothing was timed.
pub fn mib_per_s(bytes: u64, us: u64) -> u64 {
    if us == 0 {
        return 0;
    }
    ((bytes as u128 * 1_000_000) / (us as u128 * (1 << 20))) as u64
}

/// What a read rate says about a USB medium. USB 2.0 tops out near 40 MB/s
/// (480 Mb/s less its framing), so a stick reading above 40 MiB/s is on a
/// 3.x port; below it is a 2.0 port, or a slow 3.x stick. Only USB is
/// judged; a CD or a disk is named by its bus alone.
pub fn usb_hint(bus: &str, mib_s: u64) -> &'static str {
    if bus != "usb" {
        ""
    } else if mib_s > 40 {
        ": a USB 3.x rate"
    } else {
        ": a USB 2.0 rate, or a slow stick"
    }
}

/// The console's read line: `66 MiB in 1.40 s, 47 MiB/s (usb: a USB 3.x
/// rate); the release's 3.2 GiB at this rate: ~70 s`.
pub fn rate_line(bytes: u64, us: u64, bus: &str, release_bytes: Option<u64>) -> String {
    let rate = mib_per_s(bytes, us);
    let mut s = format!(
        "{} MiB in {}.{:02} s, {} MiB/s ({}{})",
        bytes >> 20,
        us / 1_000_000,
        (us % 1_000_000) / 10_000,
        rate,
        bus,
        usb_hint(bus, rate)
    );
    if let (Some(total), true) = (release_bytes, rate > 0) {
        let secs = total / (rate << 20).max(1);
        s.push_str(&format!(
            "; the release's {}.{} GiB at this rate: ~{} s",
            total >> 30,
            (total % (1 << 30)) * 10 >> 30,
            secs
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    struct Mem(Vec<u8>);
    impl Medium for Mem {
        fn read(&mut self, offset: u64, buf: &mut [u8]) -> bool {
            let o = offset as usize;
            if o + buf.len() > self.0.len() {
                return false;
            }
            buf.copy_from_slice(&self.0[o..o + buf.len()]);
            true
        }
    }

    fn rec(name: &[u8], lba: u32, len: u32, dir: bool) -> Vec<u8> {
        let mut r = vec![0u8; 33 + name.len() + (name.len() + 1) % 2];
        r[0] = r.len() as u8;
        r[2..6].copy_from_slice(&lba.to_le_bytes());
        r[6..10].copy_from_slice(&lba.to_be_bytes());
        r[10..14].copy_from_slice(&len.to_le_bytes());
        r[14..18].copy_from_slice(&len.to_be_bytes());
        r[25] = if dir { 2 } else { 0 };
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    fn put(img: &mut Vec<u8>, lba: u32, bytes: &[u8]) {
        let o = lba as usize * 2048;
        if img.len() < o + bytes.len() {
            img.resize((o + bytes.len()).div_ceil(2048) * 2048, 0);
        }
        img[o..o + bytes.len()].copy_from_slice(bytes);
    }

    /// An ISO as mkfs.iso.rs lays one down: a boot record (El Torito) before
    /// the primary descriptor, upper-case versioned names, `BOOT` beside
    /// `BOOT.TOML;1`, and a `boot` directory whose records cross into a
    /// second sector (padding then more records).
    fn iso(files: &[(&[u8], &[u8])], esp: &[u8], sig_len: usize) -> Vec<u8> {
        let mut img = vec![0u8; 40 * 2048];
        // 16: El Torito boot record, 17: primary, 18: terminator.
        let mut br = vec![0u8; 2048];
        br[1..6].copy_from_slice(b"CD001");
        br[6] = 1;
        put(&mut img, 16, &br);
        let mut lba = 30u32;
        let mut root = Vec::new();
        root.extend(rec(&[0], 20, 2048, true));
        root.extend(rec(&[1], 20, 2048, true));
        root.extend(rec(b"BOOT", 21, 4096, true));
        for (name, body) in files {
            let mut b = body.to_vec();
            if name.ends_with(b"SIG;1") {
                b.resize(sig_len, 7);
            }
            root.extend(rec(name, lba, b.len() as u32, false));
            put(&mut img, lba, &b);
            lba += 2;
        }
        put(&mut img, 20, &root);
        let mut boot = Vec::new();
        boot.extend(rec(&[0], 21, 4096, true));
        boot.extend(rec(&[1], 20, 2048, true));
        boot.extend(rec(b"ELTORITO.IMG;1", 26, 2048, false));
        boot.resize(2048, 0);
        boot.extend(rec(b"ESP.IMG;1", lba, esp.len() as u32, false));
        put(&mut img, 21, &boot);
        put(&mut img, lba, esp);
        let mut pvd = vec![0u8; 2048];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
        pvd[156..190].copy_from_slice(&rec(&[0], 20, 2048, true));
        put(&mut img, 17, &pvd);
        let mut term = vec![0u8; 2048];
        term[0] = 255;
        term[1..6].copy_from_slice(b"CD001");
        put(&mut img, 18, &term);
        img
    }

    const REL: &[u8] = b"format = 2\nversion = \"12.74.6\"\nkind = \"full\"\nchunks = 9\nbytes = 3456106496\n\n[[flavour]]\nname = \"cilium\"\nversion = \"no\"\n";

    fn release_files() -> Vec<(&'static [u8], &'static [u8])> {
        vec![
            (b"README.TXT;1", b"what this is"),
            (b"RELEASE.SIG;1", b""),
            (b"RELEASE.TOML;1", REL),
            (b"BOOT.SIG;1", b""),
            (b"BOOT.TOML;1", b"release_sha256 = \"..\"\n"),
        ]
    }

    #[test]
    fn finds_a_release_and_its_esp() {
        let esp = vec![0xeb; 5000];
        let mut m = Mem(iso(&release_files(), &esp, 64));
        let l = layout(&mut m).unwrap();
        assert_eq!(l.release_toml.len, REL.len() as u64);
        assert_eq!(l.esp.len, 5000);
        let mut b = vec![0u8; 5000];
        assert!(m.read(l.esp.offset, &mut b));
        assert_eq!(b, esp);
        let mut r = vec![0u8; REL.len()];
        assert!(m.read(l.release_toml.offset, &mut r));
        assert_eq!(r, REL);
    }

    #[test]
    fn other_disks_and_cds_are_not_releases() {
        assert_eq!(layout(&mut Mem(vec![0u8; 64 * 2048])).unwrap_err(), Error::NotIso);
        // A short medium (a 1 MiB stick image) is not ISO 9660 either.
        assert_eq!(layout(&mut Mem(vec![0u8; 4096])).unwrap_err(), Error::NotIso);
        // stormbootx's own ISO: ISO 9660, no RELEASE.toml.
        let own = iso(&[(b"STARTUP.NSH;1", b"x")], b"esp", 64);
        assert_eq!(layout(&mut Mem(own)).unwrap_err(), Error::NotRelease);
        // A diff or a backup: no BOOT.toml.
        let data = iso(&[(b"RELEASE.TOML;1", REL), (b"RELEASE.SIG;1", b"")], b"esp", 64);
        assert_eq!(layout(&mut Mem(data)).unwrap_err(), Error::NotBootable);
    }

    #[test]
    fn a_short_signature_is_refused() {
        let e = layout(&mut Mem(iso(&release_files(), b"esp", 63))).unwrap_err();
        assert!(matches!(e, Error::Broken(ref s) if s.contains("64-byte")), "{e:?}");
    }

    #[test]
    fn names_match_as_recorded() {
        assert!(name_is(b"RELEASE.TOML;1", "release.toml"));
        assert!(name_is(b"BOOT", "BOOT"));
        assert!(!name_is(b"BOOT.TOML;1", "BOOT"));
        assert!(!name_is(b"BOOT", "BOOT.TOML"));
        assert!(name_is(b"README.;1", "readme"));
    }

    const H1: &str = "0101010101010101010101010101010101010101010101010101010101010101";
    const H2: &str = "abababababababababababababababababababababababababababababababab";

    #[test]
    fn boot_toml_as_stormdist_writes_it() {
        let t = format!("release_sha256 = \"{H1}\"\nesp_sha256 = \"{H2}\"\nbios_sha256 = \"{H1}\"\n");
        let b = parse_boot(&t).unwrap();
        assert_eq!(b.release_sha256, [1; 32]);
        assert_eq!(b.esp_sha256, [0xab; 32]);
        assert_eq!(b.mbr_sha256, None);
        let t2 = format!("{t}mbr_sha256 = \"{H2}\"\n");
        assert_eq!(parse_boot(&t2).unwrap().mbr_sha256, Some([0xab; 32]));
    }

    #[test]
    fn boot_toml_is_strict() {
        let ok = format!("release_sha256 = \"{H1}\"\nesp_sha256 = \"{H2}\"\nbios_sha256 = \"{H1}\"\n");
        assert!(parse_boot(&format!("{ok}extra = \"{H1}\"\n")).unwrap_err().contains("unknown key"));
        assert!(parse_boot(&format!("{ok}esp_sha256 = \"{H1}\"\n")).unwrap_err().contains("twice"));
        assert!(parse_boot(&format!("release_sha256 = \"{H1}\"\nbios_sha256 = \"{H1}\"\n")).unwrap_err().contains("no esp_sha256"));
        assert!(parse_boot(&format!("release_sha256 = \"{H1}\"\nesp_sha256 = \"{}\"\nbios_sha256 = \"{H1}\"\n", &H2[..62])).is_err());
        assert!(parse_boot(&format!("release_sha256 = {H1}\n")).unwrap_err().contains("not a string"));
    }

    #[test]
    fn release_fields_for_the_console() {
        let r = parse_release(core::str::from_utf8(REL).unwrap());
        assert_eq!(r.version.as_deref(), Some("12.74.6"));
        assert_eq!(r.kind.as_deref(), Some("full"));
        assert_eq!(r.bytes, Some(3456106496));
    }

    #[test]
    fn the_rate_and_what_it_says() {
        assert_eq!(mib_per_s(66 << 20, 1_400_000), 47);
        assert_eq!(mib_per_s(1, 0), 0);
        let l = rate_line(66 << 20, 1_400_000, "usb", Some(3456106496));
        assert_eq!(l, "66 MiB in 1.40 s, 47 MiB/s (usb: a USB 3.x rate); the release's 3.2 GiB at this rate: ~70 s");
        assert!(rate_line(66 << 20, 2_200_000, "usb", None).ends_with("30 MiB/s (usb: a USB 2.0 rate, or a slow stick)"));
        assert!(rate_line(66 << 20, 600_000, "scsi", None).ends_with("(scsi)"));
    }

    #[test]
    fn the_release_key_is_stormcentrals() {
        assert_eq!(DIST_KEYS.len(), 1);
        assert_eq!(DIST_KEYS[0], hex32("43d06026a4819f1eb362b6ceab13cf461878cb5d5f54d9168b45ea256248afd8").unwrap());
    }
}
