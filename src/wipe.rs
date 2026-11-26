//! The fresh install's wipe (#262, stormcentral#1823): which drives a `fresh`
//! boot clears before the installer runs, and what is zeroed on each.
//!
//! Owner, 2026-11-26: "have two new drives, can you build your wipe into
//! stormbootx? (And make sure it does not wipe itself)". A `fresh` boot (the
//! engine's intent `fresh`, or `wipe: fresh` in the media's
//! `install-config.yaml` on an install) claims as `install` does and then,
//! before the image's bootloader (the installer) starts, wipes every fixed
//! drive the install will take. A plain `install` wipes nothing: the full
//! install keeps user data.
//!
//! **Never wiped**, in the order they are checked (the first that holds is
//! the one the console names):
//!
//! 1. the device stormbootx was loaded from (`boot media`);
//! 2. a removable medium, or anything on USB (`removable`, `usb`);
//! 3. a device carrying a stormbootx signature: a `\stormboot` directory on
//!    any FAT volume of it, or a stormdist release medium (`stormbootx
//!    media`, `stormdist media`);
//! 4. a device the install-config's `storage: drives:` list does not name
//!    (`not in storage.drives`); `drives: all`, or no `storage:`, takes all;
//! 5. one that cannot be written (`read-only`, `no medium`).
//!
//! If stormbootx cannot tell which device is its own medium, nothing is
//! wiped and the intent is refused (`hardware::wipe_fresh`).
//!
//! **What is zeroed** (`plan`): the first and the last 64 MiB of the whole
//! disk (the MBR, both GPTs, LVM/MD/ZFS/slab headers at either end), and on
//! every partition the table named, its first and last 1 MiB and the 64 KiB
//! at 64 MiB (btrfs's first mirror): every signature `wipefs` knows. Each
//! range is read back and must be zero.
//!
//! `core` only and no `crate::` item, like `intent.rs`, so the tests run on
//! the host:
//!
//! ```text
//! rustc --edition 2021 --test src/wipe.rs -o t/wipe-test
//! ```

/// The whole disk's ends.
pub const DISK_END_BYTES: u64 = 64 * 1024 * 1024;
/// Each partition's ends.
pub const PART_END_BYTES: u64 = 1024 * 1024;
/// btrfs's first superblock mirror, from the partition's start.
pub const BTRFS_MIRROR: (u64, u64) = (64 * 1024 * 1024, 64 * 1024);

/// What `install-config.yaml` says about a fresh install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wipe {
    /// `wipe: fresh`.
    Fresh,
    /// No `wipe:` key, or any other value: nothing is wiped.
    None,
}

/// The top-level `wipe:` of an install-config.
pub fn wipe_word(cfg: &str) -> Wipe {
    match top_value(cfg, "wipe") {
        Some(v) if unquote(v).eq_ignore_ascii_case("fresh") => Wipe::Fresh,
        _ => Wipe::None,
    }
}

/// A top-level key's inline value (`key: value`), comments stripped.
fn top_value<'a>(cfg: &'a str, key: &str) -> Option<&'a str> {
    cfg.lines().find_map(|l| {
        if l.starts_with([' ', '\t']) {
            return None;
        }
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| strip_comment(v).trim())
    })
}

fn strip_comment(v: &str) -> &str {
    match v.find(" #") {
        Some(i) => &v[..i],
        None if v.trim_start().starts_with('#') => "",
        None => v,
    }
}

fn unquote(v: &str) -> &str {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v)
}

/// Whether `storage: drives:` takes the drive with this serial (the schema
/// of stormcos `docs/STORAGE-LAYOUT.md`: `all`, the default, or a list of
/// serials or bays). A bay cannot be known here, so a list takes only the
/// drives whose serial it names, and a drive with no serial is not taken by
/// a list. Anything unreadable in the section takes nothing.
pub fn selected(cfg: &str, serial: Option<&str>) -> bool {
    match drives(cfg) {
        Drives::All => true,
        Drives::Unreadable => false,
        Drives::Listed(body) => match serial.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => list_items(body).any(|i| i.eq_ignore_ascii_case(s)),
            None => false,
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Drives<'a> {
    All,
    /// The list: an inline `[a, b]`, or the block of `- a` lines after it.
    Listed(&'a str),
    Unreadable,
}

/// `storage:`'s `drives:`.
fn drives(cfg: &str) -> Drives<'_> {
    let mut in_storage = false;
    let mut after_drives: Option<(usize, usize)> = None; // (byte offset, indent of `drives:`)
    let mut offset = 0;
    for l in cfg.split_inclusive('\n') {
        let start = offset;
        offset += l.len();
        let line = l.trim_end_matches(['\r', '\n']);
        let body = strip_comment(line);
        if body.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if let Some((from, din)) = after_drives {
            if indent > din || body.trim_start().starts_with('-') && indent >= din {
                continue;
            }
            return Drives::Listed(&cfg[from..start]);
        }
        if indent == 0 {
            in_storage = body.split_once(':').is_some_and(|(k, _)| k.trim() == "storage");
            continue;
        }
        if !in_storage {
            continue;
        }
        let Some((k, v)) = body.split_once(':') else { continue };
        if k.trim() != "drives" {
            continue;
        }
        let v = unquote(v);
        if v.eq_ignore_ascii_case("all") {
            return Drives::All;
        }
        if v.starts_with('[') {
            return if v.ends_with(']') { Drives::Listed(v) } else { Drives::Unreadable };
        }
        if !v.is_empty() {
            return Drives::Unreadable;
        }
        after_drives = Some((offset, indent));
    }
    match after_drives {
        Some((from, _)) => Drives::Listed(&cfg[from..]),
        None => Drives::All,
    }
}

/// The items of an inline `[a, b]` or of `- a` lines.
fn list_items(body: &str) -> impl Iterator<Item = &str> {
    let inline = body.trim_start().starts_with('[');
    let text = if inline { body.trim().trim_start_matches('[').trim_end_matches(']') } else { body };
    let sep: &[char] = if inline { &[','] } else { &['\n'] };
    text.split(sep)
        .map(|i| unquote(strip_comment(i).trim().trim_start_matches('-')))
        .filter(|i| !i.is_empty())
}

/// What is known about one drive, gathered by `hardware.rs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Facts<'a> {
    pub boot_media: bool,
    pub removable: bool,
    pub usb: bool,
    pub stormbootx: bool,
    pub stormdist: bool,
    pub read_only: bool,
    pub present: bool,
    pub serial: Option<&'a str>,
}

/// Why a drive is not wiped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    BootMedia,
    Removable,
    Usb,
    Stormbootx,
    Stormdist,
    NotSelected,
    ReadOnly,
    NoMedium,
}

impl Skip {
    pub fn text(self) -> &'static str {
        match self {
            Skip::BootMedia => "boot media",
            Skip::Removable => "removable",
            Skip::Usb => "usb",
            Skip::Stormbootx => "stormbootx media",
            Skip::Stormdist => "stormdist media",
            Skip::NotSelected => "not in storage.drives",
            Skip::ReadOnly => "read-only",
            Skip::NoMedium => "no medium",
        }
    }
}

/// Whether a drive is wiped: `Ok` to wipe, else the first reason not to.
pub fn verdict(f: &Facts, cfg: &str) -> Result<(), Skip> {
    if f.boot_media {
        Err(Skip::BootMedia)
    } else if f.removable {
        Err(Skip::Removable)
    } else if f.usb {
        Err(Skip::Usb)
    } else if f.stormbootx {
        Err(Skip::Stormbootx)
    } else if f.stormdist {
        Err(Skip::Stormdist)
    } else if !selected(cfg, f.serial) {
        Err(Skip::NotSelected)
    } else if !f.present {
        Err(Skip::NoMedium)
    } else if f.read_only {
        Err(Skip::ReadOnly)
    } else {
        Ok(())
    }
}

/// A range of blocks, `[first, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    pub first: u64,
    pub end: u64,
}

impl Range {
    pub fn blocks(&self) -> u64 {
        self.end - self.first
    }
}

/// How many ranges `plan` may need: the disk's two ends and three per
/// partition, before merging.
pub const fn plan_capacity(partitions: usize) -> usize {
    2 + 3 * partitions
}

/// The block ranges a drive of `blocks` blocks of `block` bytes is zeroed
/// over, given its partitions as `[first, end)` block ranges: sorted, merged,
/// each inside the disk. Returns how many of `out` it filled; a partition
/// outside the disk is clipped to it.
pub fn plan(blocks: u64, block: u32, partitions: &[Range], out: &mut [Range]) -> usize {
    let b = block.max(1) as u64;
    let n = |bytes: u64| bytes.div_ceil(b).max(1);
    let mut k = 0;
    let mut push = |first: u64, end: u64, out: &mut [Range]| {
        let (first, end) = (first.min(blocks), end.min(blocks));
        if first < end && k < out.len() {
            out[k] = Range { first, end };
            k += 1;
        }
    };
    let d = n(DISK_END_BYTES);
    push(0, d, out);
    push(blocks.saturating_sub(d), blocks, out);
    for p in partitions {
        let (first, end) = (p.first.min(blocks), p.end.min(blocks));
        if first >= end {
            continue;
        }
        let e = n(PART_END_BYTES);
        push(first, (first + e).min(end), out);
        push(end.saturating_sub(e).max(first), end, out);
        let (at, len) = BTRFS_MIRROR;
        let m = first + at / b;
        push(m, (m + n(len)).min(end), out);
    }
    // Sort and merge.
    let r = &mut out[..k];
    r.sort_unstable_by_key(|r| r.first);
    let mut m = 0;
    for i in 0..r.len() {
        if m > 0 && r[i].first <= r[m - 1].end {
            r[m - 1].end = r[m - 1].end.max(r[i].end);
        } else {
            r[m] = r[i];
            m += 1;
        }
    }
    m
}

/// The blocks `plan`'s ranges cover.
pub fn planned_blocks(r: &[Range]) -> u64 {
    r.iter().map(Range::blocks).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn wipe_fresh_is_read_and_nothing_else_is() {
        assert_eq!(wipe_word("wipe: fresh\n"), Wipe::Fresh);
        assert_eq!(wipe_word("a: 1\nwipe: \"fresh\"  # the fresh install\n"), Wipe::Fresh);
        assert_eq!(wipe_word("wipe: FRESH"), Wipe::Fresh);
        assert_eq!(wipe_word("wipe: none\n"), Wipe::None);
        assert_eq!(wipe_word("storage:\n  wipe: fresh\n"), Wipe::None, "only the top level");
        assert_eq!(wipe_word(""), Wipe::None);
        assert_eq!(wipe_word("wiped: fresh"), Wipe::None);
    }

    #[test]
    fn no_storage_section_or_all_takes_every_drive() {
        assert!(selected("", Some("S1")));
        assert!(selected("", None));
        assert!(selected("storage:\n  drives: all\n", Some("S1")));
        assert!(selected("storage:\n  format:\n    method: fast\n", Some("S1")));
        // A `drives:` outside `storage:` is not the storage section's.
        assert!(selected("other:\n  drives: [X]\n", Some("S1")));
    }

    #[test]
    fn a_list_takes_only_the_serials_it_names() {
        let inline = "storage:\n  drives: [S1, \"S2\"]  # two\n  system_mirror: all\n";
        assert!(selected(inline, Some("S1")));
        assert!(selected(inline, Some(" s2 ")));
        assert!(!selected(inline, Some("S3")));
        assert!(!selected(inline, None), "no serial, not taken by a list");
        let block = "storage:\n  drives:\n    - S1\n    - 'S2'   # bay 2\n  system_mirror: 1\nwipe: fresh\n";
        assert!(selected(block, Some("S1")));
        assert!(selected(block, Some("S2")));
        assert!(!selected(block, Some("1")));
        assert!(!selected(block, Some("S3")));
        // The list at the end of the file, at the key's own indent.
        let tail = "storage:\n  drives:\n  - S9\n";
        assert!(selected(tail, Some("S9")));
        assert!(!selected(tail, Some("S1")));
    }

    #[test]
    fn an_unreadable_drives_takes_nothing() {
        assert!(!selected("storage:\n  drives: [S1\n", Some("S1")));
        assert!(!selected("storage:\n  drives: S1\n", Some("S1")));
    }

    #[test]
    fn the_boot_media_is_never_wiped_whatever_else_holds() {
        let all = Facts { boot_media: true, removable: true, usb: true, stormbootx: true, present: true, ..Facts::default() };
        assert_eq!(verdict(&all, ""), Err(Skip::BootMedia));
        let f = Facts { present: true, ..Facts::default() };
        assert_eq!(verdict(&f, ""), Ok(()));
        assert_eq!(verdict(&Facts { removable: true, ..f }, ""), Err(Skip::Removable));
        assert_eq!(verdict(&Facts { usb: true, ..f }, ""), Err(Skip::Usb));
        assert_eq!(verdict(&Facts { stormbootx: true, ..f }, ""), Err(Skip::Stormbootx));
        assert_eq!(verdict(&Facts { stormdist: true, ..f }, ""), Err(Skip::Stormdist));
        assert_eq!(verdict(&Facts { read_only: true, ..f }, ""), Err(Skip::ReadOnly));
        assert_eq!(verdict(&Facts { present: false, ..f }, ""), Err(Skip::NoMedium));
        let cfg = "storage:\n  drives: [A]\n";
        assert_eq!(verdict(&Facts { serial: Some("B"), ..f }, cfg), Err(Skip::NotSelected));
        assert_eq!(verdict(&Facts { serial: Some("A"), ..f }, cfg), Ok(()));
        // A USB stick that says it is fixed is still never wiped.
        assert_eq!(verdict(&Facts { usb: true, serial: Some("A"), ..f }, cfg), Err(Skip::Usb));
    }

    #[test]
    fn the_plan_covers_both_ends_and_every_partition() {
        let blocks = 10 * 1024 * MIB / 4096; // 10 GiB of 4K blocks
        let p1 = Range { first: 256, end: 256 + 128 * MIB / 4096 }; // 128 MiB ESP at 1 MiB
        let p2 = Range { first: 4 * 1024 * MIB / 4096, end: blocks - 1024 }; // to near the end
        let mut out = [Range::default(); plan_capacity(2)];
        let n = plan(blocks, 4096, &[p1, p2], &mut out);
        let r = &out[..n];
        // Sorted, disjoint.
        for w in r.windows(2) {
            assert!(w[0].end < w[1].first, "{r:?}");
        }
        let covered = |lba: u64| r.iter().any(|x| x.first <= lba && lba < x.end);
        let d = DISK_END_BYTES / 4096;
        assert!(covered(0) && covered(d - 1) && !covered(d + 1000));
        assert!(covered(blocks - 1) && covered(blocks - d));
        // p1 sits inside the first 64 MiB and past it; its end and mirror.
        assert!(covered(p1.end - 1));
        // p2's start, its btrfs mirror, its end (inside the disk's last 64 MiB).
        assert!(covered(p2.first) && covered(p2.first + PART_END_BYTES / 4096 - 1));
        assert!(!covered(p2.first + PART_END_BYTES / 4096));
        assert!(covered(p2.first + 64 * MIB / 4096));
        assert!(covered(p2.end - 1));
        assert!(r.iter().all(|x| x.end <= blocks));
    }

    #[test]
    fn a_small_disk_is_wiped_whole_and_nothing_runs_past_its_end() {
        let blocks = 100 * MIB / 512;
        let mut out = [Range::default(); plan_capacity(1)];
        let n = plan(blocks, 512, &[Range { first: 2048, end: blocks * 2 }], &mut out);
        assert_eq!(&out[..n], &[Range { first: 0, end: blocks }]);
        assert_eq!(planned_blocks(&out[..n]), blocks);
        let mut out = [Range::default(); plan_capacity(0)];
        assert_eq!(plan(0, 512, &[], &mut out), 0);
        let n = plan(3, 4096, &[], &mut out);
        assert_eq!(&out[..n], &[Range { first: 0, end: 3 }]);
    }
}
