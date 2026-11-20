//! The boot record (#229): which stormbootx the medium's launcher starts.
//!
//! A writable medium's removable-media bootloader (`BOOTX64.EFI`,
//! `BOOTAA64.EFI`) is `stormbootx-launch`, which nothing renames or rewrites.
//! stormbootx itself is a payload beside it: `stormboot/stormbootx.efi` as a
//! medium is built, `stormboot/payload/<serial>.efi` once the self-update has
//! written one. The launcher starts the payload the newest valid record
//! names, so taking a new release is: write the new payload under a new name,
//! then write a record. Nothing before the record changes what boots, and
//! the record write is the one step that does.
//!
//! FAT cannot rename over a file, so the record is two files,
//! `\stormboot\boot.a` and `\stormboot\boot.b`, and each write goes to the
//! one that is invalid or older, with the next `seq`. A write cut short is a
//! record whose `check` does not match; the other one, untouched, still
//! names the payload that booted before. The record also carries its
//! payload's SHA-256, so a payload cut short is skipped too.
//!
//! ```text
//! stormbootx-boot 1
//! seq 7
//! payload stormboot/payload/12.efi
//! sha256 <64 hex digits>
//! check <64 hex digits: SHA-256 of the lines above, newlines included>
//! ```
//!
//! `core` only, and the digest is handed in, so this compiles on its own and
//! its tests run on the build host (`rustc --edition 2021 --test
//! src/bootrec.rs`). The launcher, stormbootx and stormbootx-stick share it.

/// The first line of every record.
pub const FORMAT: &str = "stormbootx-boot 1";

/// The two record files, as the medium's paths (forward slashes, no leading
/// one, as the manifest names files).
pub const SLOTS: [&str; 2] = ["stormboot/boot.a", "stormboot/boot.b"];

/// The payload a medium is built with, and the one started when no record
/// is valid (a medium no update has written).
pub const SHIPPED: &str = "stormboot/stormbootx.efi";

/// Where the self-update writes payloads: `<serial>.efi`.
pub const PAYLOAD_DIR: &str = "stormboot/payload";

/// The most a record file holds; anything longer is not one.
pub const MAX_RECORD: usize = 512;

/// SHA-256, from whichever crate holds it.
pub type Digest = fn(&[u8]) -> [u8; 32];

/// One valid record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record<'a> {
    pub seq: u64,
    pub payload: &'a str,
    pub sha256: [u8; 32],
}

/// A path a record may name: a `.efi` under `stormboot/`, plain ASCII, no
/// `..`, no backslash, no empty component.
pub fn payload_ok(p: &str) -> bool {
    p.len() <= 96
        && p.starts_with("stormboot/")
        && p.len() > 4
        && p.as_bytes()[p.len() - 4..].eq_ignore_ascii_case(b".efi")
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
        && p.rsplit('/').next().is_some_and(|name| name.len() > 4)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// 64 lowercase hex digits.
fn unhex(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (hex_digit(b[2 * i])? << 4) | hex_digit(b[2 * i + 1])?;
    }
    Some(out)
}

/// Lowercase hex of 32 bytes.
pub fn hex(d: &[u8; 32]) -> [u8; 64] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 64];
    for (byte, pair) in d.iter().zip(out.chunks_exact_mut(2)) {
        pair[0] = HEX[(byte >> 4) as usize];
        pair[1] = HEX[(byte & 0x0f) as usize];
    }
    out
}

/// A record file's bytes, if they are a whole, valid record. Anything else,
/// a torn write included, is `None`.
pub fn parse(bytes: &[u8], digest: Digest) -> Option<Record<'_>> {
    if bytes.len() > MAX_RECORD {
        return None;
    }
    let text = core::str::from_utf8(bytes).ok()?;
    let at = text.rfind("check ")?;
    let (body, check) = text.split_at(at);
    if !body.ends_with('\n') {
        return None;
    }
    let check = check.strip_prefix("check ")?.strip_suffix('\n')?;
    if unhex(check)? != digest(body.as_bytes()) {
        return None;
    }
    let mut lines = body.lines();
    if lines.next()? != FORMAT {
        return None;
    }
    let seq = lines.next()?.strip_prefix("seq ")?.parse().ok()?;
    let payload = lines.next()?.strip_prefix("payload ")?;
    let sha256 = unhex(lines.next()?.strip_prefix("sha256 ")?)?;
    if lines.next().is_some() || !payload_ok(payload) {
        return None;
    }
    Some(Record { seq, payload, sha256 })
}

/// The record's file text into `out`; its length, or `None` when it does
/// not fit or names a path no record may.
pub fn render(r: &Record, digest: Digest, out: &mut [u8]) -> Option<usize> {
    if !payload_ok(r.payload) {
        return None;
    }
    let mut w = Buf { out, len: 0 };
    use core::fmt::Write;
    let sha = hex(&r.sha256);
    write!(w, "{FORMAT}\nseq {}\npayload {}\nsha256 {}\n", r.seq, r.payload, core::str::from_utf8(&sha).ok()?).ok()?;
    let body_len = w.len;
    let check = hex(&digest(&w.out[..body_len]));
    write!(w, "check {}\n", core::str::from_utf8(&check).ok()?).ok()?;
    Some(w.len)
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

/// Where the next record goes, and its `seq`: the slot that holds no valid
/// record (`boot.a` when neither does), else the older one. Never the one
/// that names what boots now.
pub fn next_slot(slots: &[Option<Record>; 2]) -> (usize, u64) {
    let seq = slots.iter().flatten().map(|r| r.seq).max().map_or(1, |s| s + 1);
    let slot = match (slots[0], slots[1]) {
        (None, _) => 0,
        (Some(_), None) => 1,
        (Some(a), Some(b)) => usize::from(a.seq >= b.seq),
    };
    (slot, seq)
}

/// The payloads the launcher tries, in order: each valid record's, newest
/// first, with the SHA-256 it must have, then `SHIPPED` with none.
pub fn order<'a>(slots: &[Option<Record<'a>>; 2]) -> [Option<(&'a str, Option<[u8; 32]>)>; 3] {
    let mut recs = [slots[0], slots[1]];
    if let [Some(a), Some(b)] = recs {
        if b.seq > a.seq {
            recs.swap(0, 1);
        }
    }
    let mut out = [None; 3];
    let mut n = 0;
    for r in recs.iter().flatten() {
        out[n] = Some((r.payload, Some(r.sha256)));
        n += 1;
    }
    if !recs.iter().flatten().any(|r| r.payload == SHIPPED) {
        out[n] = Some((SHIPPED, None));
    }
    out
}

/// The newest valid record, which is what the launcher starts when its
/// payload loads.
pub fn newest<'a>(slots: &[Option<Record<'a>>; 2]) -> Option<Record<'a>> {
    slots.iter().flatten().copied().max_by_key(|r| r.seq)
}

#[cfg(test)]
#[path = "sha256.rs"]
mod sha256;

#[cfg(test)]
mod tests {
    use super::*;

    fn d(b: &[u8]) -> [u8; 32] {
        *sha256::digest(b).as_bytes()
    }

    fn rec(seq: u64, payload: &str) -> Record<'_> {
        Record { seq, payload, sha256: d(payload.as_bytes()) }
    }

    fn text(r: &Record) -> ([u8; MAX_RECORD], usize) {
        let mut buf = [0u8; MAX_RECORD];
        let n = render(r, d, &mut buf).expect("a record renders");
        (buf, n)
    }

    #[test]
    fn a_record_reads_back() {
        let r = rec(7, "stormboot/payload/12.efi");
        let (buf, n) = text(&r);
        let s = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(s.starts_with("stormbootx-boot 1\nseq 7\npayload stormboot/payload/12.efi\nsha256 "));
        assert!(s.ends_with('\n'));
        assert_eq!(parse(&buf[..n], d), Some(r));
    }

    /// A write cut short anywhere, or a byte changed, is no record: the
    /// launcher takes the other slot.
    #[test]
    fn a_torn_or_changed_record_is_none() {
        let r = rec(3, SHIPPED);
        let (buf, n) = text(&r);
        for cut in 0..n {
            assert_eq!(parse(&buf[..cut], d), None, "cut at {cut}");
        }
        for at in 0..n {
            let mut b = buf;
            b[at] ^= 0x01;
            assert_eq!(parse(&b[..n], d), None, "byte {at} flipped");
        }
        // A FAT write cut short can leave zeros past what was written.
        let mut z = [0u8; MAX_RECORD];
        z[..n / 2].copy_from_slice(&buf[..n / 2]);
        assert_eq!(parse(&z[..n], d), None);
        assert_eq!(parse(b"", d), None);
        assert_eq!(parse(&[0u8; 600], d), None);
    }

    #[test]
    fn only_payload_paths() {
        for ok in [SHIPPED, "stormboot/payload/5.efi", "stormboot/payload/5.EFI"] {
            assert!(payload_ok(ok), "{ok}");
        }
        for bad in [
            "EFI/BOOT/BOOTX64.EFI",
            "stormboot/../EFI/BOOT/BOOTX64.EFI",
            "stormboot/payload/5.exe",
            "stormboot//5.efi",
            "stormboot\\payload\\5.efi",
            "/stormboot/payload/5.efi",
            "stormboot/payload/ 5.efi",
            "stormboot/.efi",
        ] {
            assert!(!payload_ok(bad), "{bad}");
        }
        let mut buf = [0u8; MAX_RECORD];
        assert_eq!(render(&rec(1, "EFI/BOOT/BOOTX64.EFI"), d, &mut buf), None);
    }

    /// Each write goes to the slot that does not name what boots now.
    #[test]
    fn the_next_write_spares_the_newest() {
        assert_eq!(next_slot(&[None, None]), (0, 1));
        let a = rec(1, SHIPPED);
        assert_eq!(next_slot(&[Some(a), None]), (1, 2));
        let b = rec(2, "stormboot/payload/5.efi");
        assert_eq!(next_slot(&[Some(a), Some(b)]), (0, 3));
        let a3 = rec(3, "stormboot/payload/6.efi");
        assert_eq!(next_slot(&[Some(a3), Some(b)]), (1, 4));
        // The newest slot torn: the write goes there, past the other's seq.
        assert_eq!(next_slot(&[Some(a), None]), (1, 2));
        assert_eq!(next_slot(&[None, Some(b)]), (0, 3));
    }

    #[test]
    fn the_launcher_tries_newest_then_older_then_shipped() {
        let a = rec(4, "stormboot/payload/5.efi");
        let b = rec(5, "stormboot/payload/6.efi");
        let o = order(&[Some(a), Some(b)]);
        assert_eq!(o[0], Some(("stormboot/payload/6.efi", Some(b.sha256))));
        assert_eq!(o[1], Some(("stormboot/payload/5.efi", Some(a.sha256))));
        assert_eq!(o[2], Some((SHIPPED, None)));
        assert_eq!(newest(&[Some(a), Some(b)]), Some(b));

        // No record: the shipped payload alone.
        assert_eq!(order(&[None, None]), [Some((SHIPPED, None)), None, None]);
        assert_eq!(newest(&[None, None]), None);

        // A record naming the shipped payload keeps its hash, and it is not
        // tried twice.
        let s = rec(2, SHIPPED);
        let o = order(&[Some(s), Some(a)]);
        assert_eq!(o, [Some(("stormboot/payload/5.efi", Some(a.sha256))), Some((SHIPPED, Some(s.sha256))), None]);
    }
}
