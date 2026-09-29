//! The attached image's ESP, read by stormbootx itself (#37).
//!
//! Volumes stay 4096-byte (owner, 2026-09-29: "512 will kill our
//! performance"), and Linux will only mount a FAT whose sector is at least the
//! device's logical block, so a 4K image carries a **4096-byte-sector FAT**.
//! Not every firmware's FAT driver can read one. server1 (Supermicro X9, AMI
//! Aptio 4) mounted the attached ESP and then answered `NOT_FOUND` for a
//! `\EFI\BOOT\BOOTX64.EFI` that is there (#33). A 512-byte view of the disk
//! would not help that driver: the boot sector still says 4096.
//!
//! So when the firmware cannot load the bootloader, stormbootx reads it here:
//! the GPT (CRC-checked), the EFI System Partition, and FAT12/16/32 at any
//! sector size from 512 to 4096, with 8.3 and long names. `blockio.rs` then
//! hands the bytes to `LoadImage`. Nothing is written. It needs no firmware
//! protocol at all, so a firmware's FAT bugs stop mattering.
//!
//! What it does not do: install `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`. The image
//! it starts (stormuefi) reads its pallets through whole-disk BlockIO and
//! never opens a file on the ESP, so nothing needs it.
//!
//! Same constraints as `dnsname.rs`, `intent.rs`, `sha256.rs` and
//! `universal.rs`: `core` only and no `crate::` item, so the tests run on the
//! host. They build real images with `mkfs.fat` and mtools, so those must be
//! on the `PATH`:
//!
//! ```text
//! rustc --edition 2021 --test src/esp.rs -o t/esp-test && ./t/esp-test
//! ```

/// Bytes at an absolute byte offset of the disk, any offset and any length.
/// `false` is an I/O error.
pub trait Disk {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The disk refused a read.
    Io,
    /// No usable GPT: what was wrong with it.
    NoGpt(&'static str),
    /// A valid GPT with no EFI System Partition in it.
    NoEsp,
    /// The ESP does not hold a FAT this reader accepts: why.
    NotFat(&'static str),
    /// A path component is not there (or a file was used as a directory).
    NotFound,
    /// The path names a directory.
    NotAFile,
    /// The filesystem contradicts itself: where.
    Corrupt(&'static str),
}

impl Error {
    pub fn describe(&self) -> &'static str {
        match self {
            Error::Io => "a read from the disk failed",
            Error::NoGpt(why) | Error::NotFat(why) | Error::Corrupt(why) => *why,
            Error::NoEsp => "the GPT has no EFI System Partition",
            Error::NotFound => "no such file on the ESP",
            Error::NotAFile => "the path is a directory",
        }
    }
}

/// `C12A7328-F81F-11D2-BA4B-00A0C93EC93B`, as the GPT stores it.
const ESP_TYPE: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn le64(b: &[u8], at: usize) -> u64 {
    (le32(b, at) as u64) | ((le32(b, at + 4) as u64) << 32)
}

/// CRC-32 (IEEE, reflected), as the GPT uses it. `state` starts at `!0` and
/// the result is `!state`.
fn crc32_update(mut state: u32, data: &[u8]) -> u32 {
    for &b in data {
        state ^= b as u32;
        for _ in 0..8 {
            state = if state & 1 != 0 { (state >> 1) ^ 0xEDB8_8320 } else { state >> 1 };
        }
    }
    state
}

pub fn crc32(data: &[u8]) -> u32 {
    !crc32_update(!0, data)
}

/// One GPT partition, in the disk's own blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    /// 1-based, as a HardDrive device-path node numbers it.
    pub number: u32,
    pub first_lba: u64,
    pub last_lba: u64,
    /// The partition's unique GUID, as stored.
    pub guid: [u8; 16],
}

impl Partition {
    pub fn blocks(&self) -> u64 {
        self.last_lba - self.first_lba + 1
    }
}

/// The first EFI System Partition in the primary GPT of a disk with
/// `block_size`-byte blocks. The header and the entry array are both
/// CRC-checked: a table that fails its own checksum is not read.
pub fn find_esp<D: Disk>(disk: &mut D, block_size: u32) -> Result<Partition, Error> {
    let bs = block_size as u64;
    let mut hdr = [0u8; 512];
    if !disk.read(bs, &mut hdr) {
        return Err(Error::Io);
    }
    if &hdr[0..8] != b"EFI PART" {
        return Err(Error::NoGpt("no GPT header at LBA 1"));
    }
    let size = le32(&hdr, 12) as usize;
    if !(92..=512).contains(&size) {
        return Err(Error::NoGpt("the GPT header's size is out of range"));
    }
    let want = le32(&hdr, 16);
    let mut zeroed = hdr;
    zeroed[16..20].fill(0);
    if crc32(&zeroed[..size]) != want {
        return Err(Error::NoGpt("the GPT header fails its CRC"));
    }
    let entries_at = le64(&hdr, 72).checked_mul(bs).ok_or(Error::NoGpt("entry LBA"))?;
    let count = le32(&hdr, 80) as u64;
    let esize = le32(&hdr, 84) as u64;
    // 128 x 2^n, and never more than a chunk, so no entry straddles one.
    if esize < 128 || !esize.is_power_of_two() || esize > 4096 {
        return Err(Error::NoGpt("the GPT entry size is out of range"));
    }
    let total = count * esize;
    if total > 1 << 20 {
        return Err(Error::NoGpt("the GPT entry array is implausibly large"));
    }

    let mut chunk = [0u8; 4096];
    let mut state = !0u32;
    let mut found: Option<Partition> = None;
    let mut off = 0u64;
    while off < total {
        let n = (total - off).min(chunk.len() as u64) as usize;
        if !disk.read(entries_at + off, &mut chunk[..n]) {
            return Err(Error::Io);
        }
        state = crc32_update(state, &chunk[..n]);
        for (i, e) in chunk[..n].chunks_exact(esize as usize).enumerate() {
            // Nothing in an entry is judged until the array passes its CRC.
            if found.is_none() && e[0..16] == ESP_TYPE {
                let first = le64(e, 32);
                let last = le64(e, 40);
                let mut guid = [0u8; 16];
                guid.copy_from_slice(&e[16..32]);
                found = Some(Partition {
                    number: ((off / esize) + i as u64 + 1) as u32,
                    first_lba: first,
                    last_lba: last,
                    guid,
                });
            }
        }
        off += n as u64;
    }
    if !state != le32(&hdr, 88) {
        return Err(Error::NoGpt("the GPT entry array fails its CRC"));
    }
    let p = found.ok_or(Error::NoEsp)?;
    if p.first_lba == 0 || p.last_lba < p.first_lba {
        return Err(Error::Corrupt("the ESP's GPT entry has an empty range"));
    }
    Ok(p)
}

/// One sector of the filesystem, kept so the FAT and directories aren't read
/// over the network 4 or 32 bytes at a time.
struct Cache {
    at: u64,
    buf: [u8; 4096],
}

impl Cache {
    const fn new() -> Self {
        Cache { at: u64::MAX, buf: [0; 4096] }
    }
}

/// A directory or file found by `Fat::lookup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub first_cluster: u32,
    pub size: u32,
    pub dir: bool,
}

#[derive(Clone, Copy)]
enum DirRef {
    /// FAT12/16's fixed root area.
    Root,
    Chain(u32),
}

/// A mounted FAT volume. Offsets are absolute on the disk.
pub struct Fat {
    base: u64,
    sector: u64,
    cluster: u64,
    kind: u8,
    fat_at: u64,
    root_at: u64,
    root_len: u64,
    root_cluster: u32,
    data_at: u64,
    clusters: u32,
    fat_cache: Cache,
    dir_cache: Cache,
}

impl Fat {
    /// Mount the FAT that starts at byte `base` and may use `len` bytes.
    /// The sector size comes from the boot sector, never from the disk.
    pub fn mount<D: Disk>(disk: &mut D, base: u64, len: u64) -> Result<Fat, Error> {
        let mut b = [0u8; 512];
        if !disk.read(base, &mut b) {
            return Err(Error::Io);
        }
        let bps = le16(&b, 11) as u64;
        if !bps.is_power_of_two() || !(512..=4096).contains(&bps) {
            return Err(Error::NotFat("bytes per sector is not 512..4096"));
        }
        let spc = b[13] as u64;
        if spc == 0 || !spc.is_power_of_two() {
            return Err(Error::NotFat("sectors per cluster is not a power of two"));
        }
        let reserved = le16(&b, 14) as u64;
        let nfats = b[16] as u64;
        if reserved == 0 || nfats == 0 {
            return Err(Error::NotFat("no reserved sectors or no FAT"));
        }
        let root_entries = le16(&b, 17) as u64;
        let total = match le16(&b, 19) {
            0 => le32(&b, 32) as u64,
            n => n as u64,
        };
        let fat16_size = le16(&b, 22) as u64;
        let fat_size = if fat16_size != 0 { fat16_size } else { le32(&b, 36) as u64 };
        if total == 0 || fat_size == 0 {
            return Err(Error::NotFat("no sector count or no FAT size"));
        }
        let root_sectors = (root_entries * 32).div_ceil(bps);
        let meta = reserved + nfats * fat_size + root_sectors;
        if total <= meta {
            return Err(Error::NotFat("the volume has no data area"));
        }
        if total * bps > len {
            return Err(Error::NotFat("the volume is larger than its partition"));
        }
        let clusters = (total - meta) / spc;
        // The width follows from the cluster count, as the specification (and
        // EDK2) decide it, never from the label in the boot sector.
        let kind = if clusters < 4085 {
            12
        } else if clusters < 65525 {
            16
        } else {
            32
        };
        let (active, root_cluster) = if kind == 32 {
            if root_entries != 0 || fat16_size != 0 {
                return Err(Error::NotFat("a FAT32 with a FAT16 root or FAT size"));
            }
            let flags = le16(&b, 40);
            // Mirroring off: only the one active FAT is current.
            let active = if flags & 0x80 != 0 { (flags & 0x0F) as u64 } else { 0 };
            if active >= nfats {
                return Err(Error::Corrupt("the active FAT does not exist"));
            }
            (active, le32(&b, 44))
        } else {
            if root_entries == 0 {
                return Err(Error::NotFat("a FAT12/16 with no root directory"));
            }
            (0, 0)
        };
        let root_at = base + (reserved + nfats * fat_size) * bps;
        let root_len = root_sectors * bps;
        let fat = Fat {
            base,
            sector: bps,
            cluster: spc * bps,
            kind,
            fat_at: base + (reserved + active * fat_size) * bps,
            root_at,
            root_len,
            root_cluster,
            data_at: root_at + root_len,
            clusters: clusters as u32,
            fat_cache: Cache::new(),
            dir_cache: Cache::new(),
        };
        if kind == 32 && !fat.valid(root_cluster) {
            return Err(Error::Corrupt("the FAT32 root cluster is out of range"));
        }
        Ok(fat)
    }

    /// FAT12, 16 or 32.
    pub fn kind(&self) -> u8 {
        self.kind
    }

    /// Bytes per sector, from the boot sector.
    pub fn sector_size(&self) -> u32 {
        self.sector as u32
    }

    fn valid(&self, c: u32) -> bool {
        c >= 2 && (c as u64) < self.clusters as u64 + 2
    }

    fn cluster_at(&self, c: u32) -> u64 {
        self.data_at + (c as u64 - 2) * self.cluster
    }

    /// Bytes at `off` through one of the sector caches.
    fn cached<D: Disk>(
        disk: &mut D,
        cache: &mut Cache,
        base: u64,
        sector: u64,
        mut off: u64,
        out: &mut [u8],
    ) -> Result<(), Error> {
        let mut done = 0;
        while done < out.len() {
            let at = base + (off - base) / sector * sector;
            if cache.at != at {
                cache.at = u64::MAX;
                if !disk.read(at, &mut cache.buf[..sector as usize]) {
                    return Err(Error::Io);
                }
                cache.at = at;
            }
            let from = (off - at) as usize;
            let n = (sector as usize - from).min(out.len() - done);
            out[done..done + n].copy_from_slice(&cache.buf[from..from + n]);
            done += n;
            off += n as u64;
        }
        Ok(())
    }

    /// The cluster after `c`, or `None` at the end of the chain.
    fn next<D: Disk>(&mut self, disk: &mut D, c: u32) -> Result<Option<u32>, Error> {
        let (off, width) = match self.kind {
            12 => (c as u64 + c as u64 / 2, 2),
            16 => (c as u64 * 2, 2),
            _ => (c as u64 * 4, 4),
        };
        let mut raw = [0u8; 4];
        Self::cached(disk, &mut self.fat_cache, self.base, self.sector, self.fat_at + off, &mut raw[..width])?;
        let v = u32::from_le_bytes(raw);
        let (v, end) = match self.kind {
            12 => (if c & 1 != 0 { v >> 4 } else { v & 0xFFF }, 0xFF8),
            16 => (v, 0xFFF8),
            _ => (v & 0x0FFF_FFFF, 0x0FFF_FFF8),
        };
        if v >= end {
            return Ok(None);
        }
        if !self.valid(v) {
            return Err(Error::Corrupt("a cluster chain leaves the volume"));
        }
        Ok(Some(v))
    }

    /// The entry called `name` in `dir`.
    fn find_in<D: Disk>(&mut self, disk: &mut D, dir: DirRef, name: &str) -> Result<Entry, Error> {
        let short = short_name(name);
        let mut lfn = Lfn::new();
        let (mut cluster, mut at, mut end) = match dir {
            DirRef::Root => (0, self.root_at, self.root_at + self.root_len),
            DirRef::Chain(c) => {
                if !self.valid(c) {
                    return Err(Error::Corrupt("a directory starts outside the volume"));
                }
                (c, self.cluster_at(c), self.cluster_at(c) + self.cluster)
            }
        };
        let mut steps = 0u32;
        loop {
            if at >= end {
                let DirRef::Chain(_) = dir else { return Err(Error::NotFound) };
                match self.next(disk, cluster)? {
                    None => return Err(Error::NotFound),
                    Some(n) => {
                        steps += 1;
                        if steps > self.clusters {
                            return Err(Error::Corrupt("a directory's chain loops"));
                        }
                        cluster = n;
                        at = self.cluster_at(n);
                        end = at + self.cluster;
                    }
                }
            }
            let mut e = [0u8; 32];
            Self::cached(disk, &mut self.dir_cache, self.base, self.sector, at, &mut e)?;
            at += 32;
            match e[0] {
                0x00 => return Err(Error::NotFound),
                0xE5 => {
                    lfn.reset();
                    continue;
                }
                _ => {}
            }
            let attr = e[11];
            if attr & 0x3F == 0x0F {
                lfn.push(&e);
                continue;
            }
            if attr & 0x08 != 0 {
                // A volume label.
                lfn.reset();
                continue;
            }
            let mut sfn = [0u8; 11];
            sfn.copy_from_slice(&e[0..11]);
            if sfn[0] == 0x05 {
                sfn[0] = 0xE5;
            }
            let long = lfn.name(checksum(&e[0..11]));
            let hit = short.map_or(false, |s| s == sfn) || long.map_or(false, |l| lfn_eq(l, name));
            lfn.reset();
            if hit {
                let hi = if self.kind == 32 { (le16(&e, 20) as u32) << 16 } else { 0 };
                return Ok(Entry {
                    first_cluster: hi | le16(&e, 26) as u32,
                    size: le32(&e, 28),
                    dir: attr & 0x10 != 0,
                });
            }
        }
    }

    /// Walk `path` from the root. `\` and `/` both separate, case is ignored
    /// (ASCII), and empty components are skipped.
    pub fn lookup<D: Disk>(&mut self, disk: &mut D, path: &str) -> Result<Entry, Error> {
        let root = if self.kind == 32 { DirRef::Chain(self.root_cluster) } else { DirRef::Root };
        let mut cur = Entry { first_cluster: 0, size: 0, dir: true };
        let mut dir = root;
        for part in path.split(['\\', '/']).filter(|p| !p.is_empty()) {
            if !cur.dir {
                return Err(Error::NotFound);
            }
            cur = self.find_in(disk, dir, part)?;
            // `..` of a first-level directory names the root as cluster 0.
            dir = if cur.first_cluster == 0 { root } else { DirRef::Chain(cur.first_cluster) };
        }
        Ok(cur)
    }

    /// Read the whole of a file `lookup` found into `out`, which must be
    /// exactly its size. Runs of consecutive clusters go in one read.
    pub fn read_file<D: Disk>(&mut self, disk: &mut D, file: &Entry, out: &mut [u8]) -> Result<(), Error> {
        if file.dir {
            return Err(Error::NotAFile);
        }
        let size = file.size as u64;
        if out.len() as u64 != size {
            return Err(Error::Corrupt("the buffer is not the file's size"));
        }
        if size == 0 {
            return Ok(());
        }
        let mut c = file.first_cluster;
        if !self.valid(c) {
            return Err(Error::Corrupt("a file starts outside the volume"));
        }
        let mut done = 0u64;
        let mut steps = 0u32;
        loop {
            let start = c;
            let mut run = 1u64;
            let mut after = None;
            while done + run * self.cluster < size {
                let n = self
                    .next(disk, c)?
                    .ok_or(Error::Corrupt("a file's chain ends before the file does"))?;
                steps += 1;
                if steps > self.clusters {
                    return Err(Error::Corrupt("a file's chain loops"));
                }
                if n == c + 1 {
                    run += 1;
                    c = n;
                } else {
                    after = Some(n);
                    break;
                }
            }
            let n = (run * self.cluster).min(size - done);
            let dst = &mut out[done as usize..(done + n) as usize];
            if !disk.read(self.cluster_at(start), dst) {
                return Err(Error::Io);
            }
            done += n;
            match after {
                Some(next) if done < size => c = next,
                _ => return Ok(()),
            }
        }
    }
}

/// `name` as a padded, upper-case 8.3 entry, or `None` if it has no 8.3 form
/// (then only a long name can match it).
fn short_name(name: &str) -> Option<[u8; 11]> {
    match name {
        "." => return Some(*b".          "),
        ".." => return Some(*b"..         "),
        _ => {}
    }
    let b = name.as_bytes();
    let (stem, ext) = match b.iter().rposition(|&c| c == b'.') {
        Some(0) => return None,
        Some(i) => (&b[..i], &b[i + 1..]),
        None => (b, &b[..0]),
    };
    if stem.is_empty() || stem.len() > 8 || ext.len() > 3 {
        return None;
    }
    let ok = |c: u8| c > b' ' && c < 0x7F && !b"\"*+,./:;<=>?[\\]|".contains(&c);
    if !stem.iter().chain(ext).all(|&c| ok(c)) {
        return None;
    }
    let mut out = [b' '; 11];
    for (i, &c) in stem.iter().enumerate() {
        out[i] = c.to_ascii_uppercase();
    }
    for (i, &c) in ext.iter().enumerate() {
        out[8 + i] = c.to_ascii_uppercase();
    }
    Some(out)
}

/// The short-name checksum every LFN entry carries.
fn checksum(sfn: &[u8]) -> u8 {
    sfn.iter().fold(0u8, |s, &c| ((s & 1) << 7).wrapping_add(s >> 1).wrapping_add(c))
}

/// A UCS-2 long name against a path component, ignoring ASCII case.
fn lfn_eq(long: &[u16], name: &str) -> bool {
    let mut it = long.iter();
    for u in name.encode_utf16() {
        match it.next() {
            Some(&l) if l == u => {}
            Some(&l) if l < 0x80 && u < 0x80 && (l as u8).eq_ignore_ascii_case(&(u as u8)) => {}
            _ => return false,
        }
    }
    it.next().is_none()
}

/// A long name being assembled from its entries, which come last part first.
struct Lfn {
    units: [u16; 260],
    parts: u8,
    expect: u8,
    sum: u8,
    ok: bool,
}

impl Lfn {
    fn new() -> Self {
        Lfn { units: [0xFFFF; 260], parts: 0, expect: 0, sum: 0, ok: false }
    }

    fn reset(&mut self) {
        self.ok = false;
    }

    fn push(&mut self, e: &[u8; 32]) {
        let seq = e[0] & 0x1F;
        if e[0] & 0x40 != 0 {
            self.units = [0xFFFF; 260];
            self.parts = seq;
            self.sum = e[13];
            self.ok = (1..=20).contains(&seq);
        } else if !(self.ok && seq == self.expect && e[13] == self.sum) {
            self.ok = false;
            return;
        }
        if !self.ok {
            return;
        }
        self.expect = seq - 1;
        let at = (seq as usize - 1) * 13;
        for (k, o) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].iter().enumerate() {
            self.units[at + k] = le16(e, *o);
        }
    }

    /// The finished name, if every part arrived and belongs to this entry.
    fn name(&self, sum: u8) -> Option<&[u16]> {
        if !self.ok || self.expect != 0 || self.sum != sum {
            return None;
        }
        let all = &self.units[..self.parts as usize * 13];
        let len = all.iter().position(|&u| u == 0 || u == 0xFFFF).unwrap_or(all.len());
        Some(&all[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};
    use std::path::PathBuf;
    use std::process::Command;

    /// A GPT disk image in memory up to the partition, and a FAT image file
    /// from `mkfs.fat` as the partition, so a 300 MB FAT32 needs no 300 MB
    /// buffer.
    struct TestDisk {
        head: Vec<u8>,
        part_at: u64,
        part: fs::File,
        reads: usize,
    }

    impl Disk for TestDisk {
        fn read(&mut self, offset: u64, buf: &mut [u8]) -> bool {
            self.reads += 1;
            for (i, b) in buf.iter_mut().enumerate() {
                let at = offset + i as u64;
                if at < self.part_at {
                    *b = self.head.get(at as usize).copied().unwrap_or(0);
                }
            }
            if offset + buf.len() as u64 > self.part_at {
                let from = offset.max(self.part_at);
                let skip = (from - offset) as usize;
                self.part.seek(SeekFrom::Start(from - self.part_at)).unwrap();
                let got = read_all(&mut self.part, &mut buf[skip..]);
                buf[skip + got..].fill(0);
            }
            true
        }
    }

    fn read_all(f: &mut fs::File, buf: &mut [u8]) -> usize {
        let mut n = 0;
        while n < buf.len() {
            match f.read(&mut buf[n..]).unwrap() {
                0 => break,
                k => n += k,
            }
        }
        n
    }

    fn workdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("stormbootx-esp-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(cmd: &str, args: &[&str]) {
        let out = Command::new(cmd)
            .args(args)
            .env("MTOOLS_SKIP_CHECK", "1")
            .output()
            .unwrap_or_else(|e| panic!("{cmd} is needed for these tests: {e}"));
        assert!(
            out.status.success(),
            "{cmd} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Deterministic bytes, so a misread shows as a mismatch, not a zero page.
    fn content(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    struct Image {
        disk: TestDisk,
        block: u32,
        files: Vec<(&'static str, Vec<u8>)>,
    }

    /// A FAT of `mb` MiB at `sector` bytes a sector and `bits` wide, holding
    /// a bootloader, a long name, a nested directory and a file that
    /// `mdel` has made fragment, on a GPT disk of `block`-byte blocks.
    fn image(tag: &str, block: u32, sector: u32, bits: u32, mb: u32, spc: u32) -> Image {
        let d = workdir(tag);
        let fat = d.join("esp.img");
        let fat_s = fat.to_str().unwrap();
        run(
            "mkfs.fat",
            &[
                "-C", fat_s, &(mb * 1024).to_string(),
                "-S", &sector.to_string(),
                "-F", &bits.to_string(),
                "-s", &spc.to_string(),
                "-n", "TESTESP",
            ],
        );
        let mut files: Vec<(&'static str, Vec<u8>)> = vec![
            ("\\EFI\\BOOT\\BOOTX64.EFI", content(150_001, 1)),
            ("\\EFI\\BOOT\\a rather long file name.txt", content(5_000, 2)),
            ("\\deep\\er\\than\\that.bin", content(777, 3)),
            ("\\empty.txt", Vec::new()),
        ];
        run("mmd", &["-i", fat_s, "::/EFI", "::/EFI/BOOT", "::/deep", "::/deep/er", "::/deep/er/than"]);
        let cl = (sector * spc) as usize;
        // Fragment on purpose: fill, free a hole in the middle, and write a
        // file bigger than the hole.
        for (name, bytes) in [("hole1.bin", content(3 * cl, 4)), ("pin.bin", content(cl, 5))] {
            let p = d.join(name);
            fs::write(&p, bytes).unwrap();
            run("mcopy", &["-o", "-i", fat_s, p.to_str().unwrap(), &format!("::/{name}")]);
        }
        run("mdel", &["-i", fat_s, "::/hole1.bin"]);
        files.push(("\\frag.bin", content(10 * cl + 17, 6)));
        for (name, bytes) in &files {
            let p = d.join("f");
            fs::write(&p, bytes).unwrap();
            let dst = format!("::{}", name.replace('\\', "/"));
            run("mcopy", &["-o", "-i", fat_s, p.to_str().unwrap(), &dst]);
        }
        files.push(("\\pin.bin", content(cl, 5)));
        // Many root entries, so a lookup crosses directory sectors.
        for i in 0..70 {
            let p = d.join("f");
            fs::write(&p, [i as u8]).unwrap();
            run("mcopy", &["-o", "-i", fat_s, p.to_str().unwrap(), &format!("::/filler{i:02}.x")]);
        }

        let part_bytes = fs::metadata(&fat).unwrap().len();
        let bs = block as u64;
        let first = (1 << 20) / bs;
        let last = first + part_bytes.div_ceil(bs) - 1;
        Image { disk: gpt(block, first, last, fat), block, files }
    }

    /// A primary GPT with the ESP as entry 3 of 128, so the number is checked.
    fn gpt(block: u32, first: u64, last: u64, fat: PathBuf) -> TestDisk {
        let bs = block as usize;
        let mut head = vec![0u8; bs * 2 + 128 * 128];
        let mut entries = vec![0u8; 128 * 128];
        let e = &mut entries[2 * 128..3 * 128];
        e[0..16].copy_from_slice(&ESP_TYPE);
        e[16..32].copy_from_slice(&[0xAB; 16]);
        e[32..40].copy_from_slice(&first.to_le_bytes());
        e[40..48].copy_from_slice(&last.to_le_bytes());
        // A partition of another type before it.
        entries[0] = 0x11;
        entries[32..40].copy_from_slice(&(last + 1).to_le_bytes());
        entries[40..48].copy_from_slice(&(last + 2).to_le_bytes());
        let h = &mut head[bs..bs + 92];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&1u64.to_le_bytes());
        h[72..80].copy_from_slice(&2u64.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&crc32(&entries).to_le_bytes());
        let c = crc32(h);
        h[16..20].copy_from_slice(&c.to_le_bytes());
        head[bs * 2..].copy_from_slice(&entries);
        TestDisk { head, part_at: first * block as u64, part: fs::File::open(fat).unwrap(), reads: 0 }
    }

    fn check(img: &mut Image, bits: u8, sector: u32) {
        let p = find_esp(&mut img.disk, img.block).unwrap();
        assert_eq!(p.number, 3);
        assert_eq!(p.guid, [0xAB; 16]);
        let bs = img.block as u64;
        let mut fat = Fat::mount(&mut img.disk, p.first_lba * bs, p.blocks() * bs).unwrap();
        assert_eq!(fat.kind(), bits);
        assert_eq!(fat.sector_size(), sector);
        for (name, want) in img.files.clone() {
            let e = fat.lookup(&mut img.disk, name).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(e.size as usize, want.len(), "{name}");
            let mut got = vec![0u8; want.len()];
            fat.read_file(&mut img.disk, &e, &mut got).unwrap();
            assert!(got == want, "{name}: the bytes differ");
        }
        // Case, separators and the path the firmware would pass.
        let want = &img.files[0].1;
        for alias in ["/efi/boot/bootx64.efi", "EFI\\Boot\\BootX64.efi", "\\\\EFI\\\\BOOT\\BOOTX64.EFI"] {
            let e = fat.lookup(&mut img.disk, alias).unwrap();
            let mut got = vec![0u8; e.size as usize];
            fat.read_file(&mut img.disk, &e, &mut got).unwrap();
            assert!(&got == want, "{alias}");
        }
        let e = fat.lookup(&mut img.disk, "\\EFI\\BOOT\\A RATHER LONG FILE NAME.TXT").unwrap();
        assert_eq!(e.size, 5_000);
        assert_eq!(fat.lookup(&mut img.disk, "\\filler69.x").unwrap().size, 1);
        assert_eq!(fat.lookup(&mut img.disk, "\\EFI\\BOOT\\BOOTIA32.EFI"), Err(Error::NotFound));
        assert_eq!(fat.lookup(&mut img.disk, "\\hole1.bin"), Err(Error::NotFound));
        assert_eq!(fat.lookup(&mut img.disk, "\\EFI\\BOOT\\BOOTX64.EFI\\x"), Err(Error::NotFound));
        assert_eq!(fat.lookup(&mut img.disk, "\\nope\\BOOTX64.EFI"), Err(Error::NotFound));
        let dir = fat.lookup(&mut img.disk, "\\EFI\\BOOT").unwrap();
        assert!(dir.dir);
        assert_eq!(fat.read_file(&mut img.disk, &dir, &mut []), Err(Error::NotAFile));
        // `..` of a first-level directory is the root.
        assert!(fat.lookup(&mut img.disk, "\\EFI\\..\\deep\\er").unwrap().dir);
    }

    #[test]
    fn fat16_4k_sectors_on_a_4k_disk() {
        // The shape stormblock lays on a 4K release disk: the #33 case.
        let mut img = image("16-4k", 4096, 4096, 16, 64, 1);
        check(&mut img, 16, 4096);
    }

    #[test]
    fn fat32_4k_sectors_on_a_4k_disk() {
        let mut img = image("32-4k", 4096, 4096, 32, 300, 1);
        check(&mut img, 32, 4096);
    }

    #[test]
    fn fat16_4k_sectors_with_bigger_clusters() {
        let mut img = image("16-4k-c", 4096, 4096, 16, 128, 4);
        check(&mut img, 16, 4096);
    }

    #[test]
    fn fat12_512_sectors_on_a_512_disk() {
        let mut img = image("12-512", 512, 512, 12, 2, 1);
        check(&mut img, 12, 512);
    }

    #[test]
    fn fat32_512_sectors_on_a_512_disk() {
        let mut img = image("32-512", 512, 512, 32, 40, 1);
        check(&mut img, 32, 512);
    }

    #[test]
    fn fat16_512_sectors_on_a_4k_disk() {
        // The sector comes from the boot sector, never from the disk.
        let mut img = image("16-512-on-4k", 4096, 512, 16, 16, 1);
        check(&mut img, 16, 512);
    }

    #[test]
    fn the_fat_and_directories_are_read_a_sector_at_a_time() {
        let mut img = image("reads", 4096, 4096, 16, 64, 1);
        let p = find_esp(&mut img.disk, 4096).unwrap();
        let mut fat = Fat::mount(&mut img.disk, p.first_lba * 4096, p.blocks() * 4096).unwrap();
        img.disk.reads = 0;
        let e = fat.lookup(&mut img.disk, "\\EFI\\BOOT\\BOOTX64.EFI").unwrap();
        let mut got = vec![0u8; e.size as usize];
        fat.read_file(&mut img.disk, &e, &mut got).unwrap();
        // 37 clusters of 4 KiB: a read per cluster would be 37 or more, and
        // over NVMe/TCP each is a round trip.
        assert!(img.disk.reads < 12, "{} reads", img.disk.reads);
    }

    #[test]
    fn a_gpt_that_fails_its_crc_is_not_read() {
        let mut img = image("crc", 4096, 4096, 16, 64, 1);
        img.disk.head[4096 + 40] ^= 1;
        assert_eq!(find_esp(&mut img.disk, 4096), Err(Error::NoGpt("the GPT header fails its CRC")));
        img.disk.head[4096 + 40] ^= 1;
        img.disk.head[8192 + 2 * 128 + 33] ^= 1;
        assert_eq!(find_esp(&mut img.disk, 4096), Err(Error::NoGpt("the GPT entry array fails its CRC")));
    }

    #[test]
    fn the_gpt_is_read_at_the_disks_block_size() {
        // A 4K GPT read as if the disk were 512 finds no header at LBA 1.
        let mut img = image("bs", 4096, 4096, 16, 64, 1);
        assert_eq!(find_esp(&mut img.disk, 512), Err(Error::NoGpt("no GPT header at LBA 1")));
    }

    #[test]
    fn no_esp() {
        let mut img = image("noesp", 512, 512, 12, 2, 1);
        img.disk.head[1024 + 2 * 128] = 0;
        let crc = crc32(&img.disk.head[1024..1024 + 128 * 128]);
        img.disk.head[512 + 88..512 + 92].copy_from_slice(&crc.to_le_bytes());
        img.disk.head[512 + 16..512 + 20].fill(0);
        let h = crc32(&img.disk.head[512..512 + 92]);
        img.disk.head[512 + 16..512 + 20].copy_from_slice(&h.to_le_bytes());
        assert_eq!(find_esp(&mut img.disk, 512), Err(Error::NoEsp));
    }

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn short_names() {
        assert_eq!(short_name("BOOTX64.EFI"), Some(*b"BOOTX64 EFI"));
        assert_eq!(short_name("efi"), Some(*b"EFI        "));
        assert_eq!(short_name("a rather long file name.txt"), None);
        assert_eq!(short_name(".hidden"), None);
        assert_eq!(short_name("toolongname.x"), None);
        assert_eq!(short_name(".."), Some(*b"..         "));
    }
}
