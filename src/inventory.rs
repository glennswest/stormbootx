//! The firmware inventory sent with the claim (#4, #20, #167): what this
//! machine's firmware sees, with no OS and no BMC.
//!
//! #20 (2026-10-02, option 2) sent only what a BMC can't give. #167
//! (2026-10-21, the server8 experience: a BMC that answers nothing) widened
//! it to everything, as `"v":2`: each NIC's MAC, link, MTU and driver; the
//! storage controllers; every disk UEFI sees with its bus, model, serial,
//! firmware, WWN, size, sector sizes, media type and partitions (with the
//! directories under an ESP's `\EFI`: the installs on it); the firmware,
//! system and board from SMBIOS; CPU and memory always. "BMC" is still
//! reported: SMBIOS has a Type 38 (IPMI device) or 42 (host interface).
//!
//! It goes in the claim body as `"inventory"`, beside `"agent"` (#90). The
//! engine keeps it in the host's last-claim record as given (stormblock#177),
//! and drops anything that is not a JSON object or is over 16 KiB, without
//! failing the claim. So `render` keeps it an object well under that: NICs
//! and controllers first, then disks while they fit (all counted in
//! `disk_count`, the cut ones in `disks_omitted`), then SMBIOS's.
//!
//! ```json
//! {"v":2,"bmc":false,
//!  "nics":[{"mac":"52:54:00:12:34:56","link":true,"driver":"Virtio Network Driver",
//!           "media_driver":false,"pci":"0000:00:02.0","id":"1af4:1000","mtu":1500}],
//!  "storage":[{"pci":"0000:00:1f.2","id":"8086:2922","class":"01:06:01","driver":"…"}],
//!  "disk_count":2,
//!  "disks":[{"bus":"nvme","path":"PciRoot(0x0)/…/NVMe(0x1,…)","model":"…","serial":"…",
//!            "firmware":"…","wwn":"eui.…","media":"nvme","blocks":2097152,"block_size":512,
//!            "removable":false,"medium":false,"table":"gpt",
//!            "partitions":[{"n":1,"kind":"esp","bytes":67108864,"efi":["BOOT","ubuntu"]}]}],
//!  "system":{"vendor":"…","product":"…","serial":"…","uuid":"…"},
//!  "board":{"vendor":"…","product":"…"},
//!  "firmware":{"vendor":"…","version":"…","date":"…"},
//!  "cpu":{"model":"…","sockets":1,"cores":4,"threads":8,"max_mhz":3000},
//!  "memory":{"mb":1024,"dimms":1}}
//! ```
//!
//! The parsers of what drives answer (ATA IDENTIFY, NVMe Identify, SCSI
//! INQUIRY and VPD pages 0x80, 0x83, 0xB1) are here, host-tested; asking
//! the firmware for them is `hardware.rs`.
//!
//! Same constraints as `sha256.rs` and the other core-only modules: `core`
//! and `alloc` only and no `crate::` item, so the tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/inventory.rs -o t/inventory-test && ./t/inventory-test
//! ```
//!
//! The collecting (SNP, PCI I/O, BlockIO, DiskInfo, pass-thru, the SMBIOS
//! table) is `hardware.rs`.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// What the engine keeps at most (stormblock's `CLAIM_EXTRA_MAX`), less room
/// for nothing in particular: the inventory never comes near it on a server,
/// and a disk list that would is cut, not sent and dropped.
pub const BUDGET: usize = 15 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Nic {
    pub mac: String,
    pub link: bool,
    pub driver: Option<String>,
    /// A driver stormbootx loaded from `\stormboot\drivers`.
    pub media_driver: bool,
    pub pci: Option<String>,
    pub id: Option<(u16, u16)>,
    /// The MTU the NIC runs at: the lease's (#165), else what its SNP carries.
    pub mtu: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Controller {
    pub pci: String,
    pub id: (u16, u16),
    /// Class, subclass, programming interface.
    pub class: (u8, u8, u8),
    pub driver: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Disk {
    pub path: Option<String>,
    pub blocks: u64,
    pub block_size: u32,
    pub removable: bool,
    /// The disk stormbootx itself was loaded from.
    pub medium: bool,
    /// `nvme`, `sata`, `ata`, `scsi`, `usb`, `virtio`, `sd`, `ufs` (#167).
    pub bus: Option<&'static str>,
    pub id: DriveId,
    /// What is on it: `gpt`, `mbr`, `none` (blank at LBA 0 and 1), or
    /// nothing when it was not read (no medium).
    pub table: Option<&'static str>,
    pub partitions: Vec<Part>,
    /// More partitions than `partitions` holds.
    pub partitions_omitted: usize,
}

/// What a drive says about itself (#167): ATA IDENTIFY, NVMe Identify, SCSI
/// INQUIRY and its VPD pages. Every field is optional: a firmware that
/// answers none of them still lists the disk.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DriveId {
    pub model: Option<String>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    /// `naa.<hex>` or `eui.<hex>`, as Linux's `/dev/disk/by-id/wwn-*` and
    /// `nvme id-ns` print them.
    pub wwn: Option<String>,
    /// `ssd`, `hdd` or `nvme`; `None` when the drive does not say.
    pub media: Option<&'static str>,
    /// Bytes per physical sector, when the drive says and it differs from the
    /// logical one.
    pub physical_block_size: Option<u32>,
}

/// One partition: its number, what kind, its size, its name, and for an
/// ESP the directories under `\EFI` (the installs it boots).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Part {
    pub number: u32,
    pub kind: String,
    pub bytes: u64,
    pub name: Option<String>,
    pub efi: Vec<String>,
}

/// What a disk's partitions are kept to, and an ESP's `\EFI` listing.
pub const MAX_PARTS: usize = 16;
pub const MAX_EFI_DIRS: usize = 8;

/// The firmware and system strings SMBIOS carries (Types 0, 1 and 2).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Named {
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub version: Option<String>,
    pub serial: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cpu {
    pub model: Option<String>,
    pub sockets: u32,
    pub cores: u32,
    pub threads: u32,
    pub max_mhz: u32,
}

/// What SMBIOS says: whether there is a BMC, the CPUs and memory, and the
/// firmware, system and board (#167).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Smbios {
    pub bmc: bool,
    pub cpu: Option<Cpu>,
    pub memory_mb: u64,
    pub dimms: u32,
    /// Type 0: vendor and version; its release date beside it.
    pub firmware: Named,
    pub firmware_date: Option<String>,
    /// Type 1, with its UUID as text.
    pub system: Named,
    pub uuid: Option<String>,
    /// Type 2.
    pub board: Named,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// String `index` (1-based) of the strings after a structure's formatted
/// section; `None` for 0, a missing one, or an empty one.
fn smbios_string(strings: &[u8], index: u8) -> Option<String> {
    if index == 0 {
        return None;
    }
    let s = strings.split(|&b| b == 0).nth(index as usize - 1)?;
    let s: String = s.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '?' }).collect();
    let s = String::from(s.trim());
    (!s.is_empty()).then_some(s)
}

/// SMBIOS's UUID (2.6+: the first three fields little-endian, as GPT's), as
/// `dmidecode` prints it; none for all zeros or all ones ("not set").
fn uuid_text(b: &[u8]) -> Option<String> {
    if b.len() != 16 || b.iter().all(|&x| x == 0) || b.iter().all(|&x| x == 0xFF) {
        return None;
    }
    let mut g = [0u8; 16];
    g.copy_from_slice(b);
    Some(guid_text(&g))
}

/// A GUID stored mixed-endian (GPT, SMBIOS 2.6+) as upper-case text.
pub fn guid_text(g: &[u8; 16]) -> String {
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        le32(g, 0),
        le16(g, 4),
        le16(g, 6),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

/// A GPT partition type by name, for the ones an operator meets on a server;
/// any other as its GUID.
pub fn partition_kind(type_guid: &[u8; 16]) -> String {
    let g = guid_text(type_guid);
    let name = match g.as_str() {
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "esp",
        "21686148-6449-6E6F-744E-656564454649" => "bios-boot",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "linux",
        "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709" => "linux-root",
        "BC13C2FF-59E6-4262-A352-B275FD6F7172" => "linux-boot",
        "933AC7E1-2EB4-4F13-B844-0E14E2AEF915" => "linux-home",
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => "linux-swap",
        "E6D6D379-F507-44C2-A23C-238F2A3DF928" => "linux-lvm",
        "A19D880F-05FC-4D3B-A006-743F0F84911E" => "linux-raid",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "windows-reserved",
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => "windows-data",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "windows-recovery",
        "6A898CC3-1DD2-11B2-99A6-080020736631" => "zfs",
        "516E7CB6-6ECF-11D6-8FF8-00022D09712B" => "freebsd-ufs",
        "48465300-0000-11AA-AA11-00306543ECAC" => "apple-hfs",
        "7C3457EF-0000-11AA-AA11-00306543ECAC" => "apple-apfs",
        _ => return g,
    };
    String::from(name)
}

/// An MBR partition type by name, for a disk with no GPT.
pub fn mbr_kind(t: u8) -> String {
    let name = match t {
        0x01 | 0x04 | 0x06 | 0x0B | 0x0C | 0x0E => "fat",
        0x05 | 0x0F => "extended",
        0x07 => "ntfs",
        0x27 => "windows-recovery",
        0x82 => "linux-swap",
        0x83 => "linux",
        0x8E => "linux-lvm",
        0xEE => "gpt-protective",
        0xEF => "esp",
        0xFD => "linux-raid",
        _ => return format!("mbr-{t:02x}"),
    };
    String::from(name)
}

/// Bytes as a string with the trailing and leading spaces and NULs off,
/// anything not printable ASCII as `?`; `None` when nothing is left.
fn ascii(b: &[u8]) -> Option<String> {
    let s: String = b.iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else if c == 0 { ' ' } else { '?' }).collect();
    let s = String::from(s.trim());
    (!s.is_empty()).then_some(s)
}

/// An ATA string field: two characters a word, the first in the high byte.
fn ata_string(id: &[u8], word: usize, words: usize) -> Option<String> {
    let mut b = Vec::with_capacity(words * 2);
    for w in word..word + words {
        b.push(id[w * 2 + 1]);
        b.push(id[w * 2]);
    }
    ascii(&b)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// ATA IDENTIFY DEVICE (512 bytes): model (words 27-46), serial (10-19),
/// firmware (23-26), the WWN (108-111, NAA 5), the physical sector (106,
/// 117-118) and the rotation rate (217: 1 is an SSD).
pub fn ata_identify(id: &[u8]) -> DriveId {
    let mut d = DriveId::default();
    if id.len() < 512 {
        return d;
    }
    let w = |n: usize| le16(id, n * 2);
    d.serial = ata_string(id, 10, 10);
    d.firmware = ata_string(id, 23, 4);
    d.model = ata_string(id, 27, 20);
    // Word 87 bit 8 says the WWN is there (14 set and 15 clear: the word is
    // valid at all).
    if w(87) & 0xC000 == 0x4000 && w(87) & 0x0100 != 0 {
        let words = [w(108), w(109), w(110), w(111)];
        if words.iter().any(|&x| x != 0) {
            let bytes: Vec<u8> = words.iter().flat_map(|x| x.to_be_bytes()).collect();
            d.wwn = Some(format!("naa.{}", hex(&bytes)));
        }
    }
    let w106 = w(106);
    if w106 & 0xC000 == 0x4000 {
        let mut logical = 512u32;
        if w106 & 0x1000 != 0 {
            let words = (w(117) as u32) | ((w(118) as u32) << 16);
            if words >= 256 {
                logical = words * 2;
            }
        }
        if w106 & 0x2000 != 0 {
            let physical = logical << (w106 & 0xF);
            if physical != logical {
                d.physical_block_size = Some(physical);
            }
        }
    }
    d.media = match w(217) {
        1 => Some("ssd"),
        0x0401..=0xFFFE => Some("hdd"),
        _ => None,
    };
    d
}

/// NVMe Identify Controller (CNS 1, 4096 bytes): serial (bytes 4-23), model
/// (24-63), firmware (64-71).
pub fn nvme_controller(id: &[u8]) -> DriveId {
    let mut d = DriveId { media: Some("nvme"), ..DriveId::default() };
    if id.len() >= 72 {
        d.serial = ascii(&id[4..24]);
        d.model = ascii(&id[24..64]);
        d.firmware = ascii(&id[64..72]);
    }
    d
}

/// NVMe Identify Namespace (CNS 0): the namespace's NGUID (bytes 104-119),
/// else its EUI-64 (120-127), as `eui.<hex>`; none when both are zero.
pub fn nvme_namespace_wwn(ns: &[u8]) -> Option<String> {
    if ns.len() < 128 {
        return None;
    }
    [&ns[104..120], &ns[120..128]].into_iter().find(|b| b.iter().any(|&x| x != 0)).map(|b| format!("eui.{}", hex(b)))
}

/// Standard SCSI INQUIRY data: vendor (bytes 8-15) and product (16-31) as
/// the model, revision (32-35) as the firmware.
pub fn scsi_inquiry(inq: &[u8]) -> DriveId {
    let mut d = DriveId::default();
    if inq.len() >= 32 {
        d.model = match (ascii(&inq[8..16]), ascii(&inq[16..32])) {
            (Some(v), Some(p)) => Some(format!("{v} {p}")),
            (v, p) => p.or(v),
        };
    }
    if inq.len() >= 36 {
        d.firmware = ascii(&inq[32..36]);
    }
    d
}

/// The page length of a VPD page and its body, bounds-checked.
fn vpd_body(page: &[u8], code: u8) -> Option<&[u8]> {
    if page.len() < 4 || page[1] != code {
        return None;
    }
    let n = u16::from_be_bytes([page[2], page[3]]) as usize;
    page.get(4..(4 + n).min(page.len()))
}

/// VPD page 0x80, Unit Serial Number.
pub fn scsi_vpd_serial(page: &[u8]) -> Option<String> {
    vpd_body(page, 0x80).and_then(ascii)
}

/// VPD page 0x83, Device Identification: the logical unit's NAA designator,
/// else its EUI-64.
pub fn scsi_vpd_wwn(page: &[u8]) -> Option<String> {
    let body = vpd_body(page, 0x83)?;
    let mut at = 0;
    let mut eui = None;
    while at + 4 <= body.len() {
        let code_set = body[at] & 0x0F;
        let assoc = (body[at + 1] >> 4) & 0x3;
        let ty = body[at + 1] & 0x0F;
        let n = body[at + 3] as usize;
        let Some(id) = body.get(at + 4..at + 4 + n) else { break };
        // Binary, about the logical unit.
        if code_set == 1 && assoc == 0 && !id.is_empty() {
            match ty {
                3 => return Some(format!("naa.{}", hex(id))),
                2 if eui.is_none() => eui = Some(format!("eui.{}", hex(id))),
                _ => {}
            }
        }
        at += 4 + n;
    }
    eui
}

/// VPD page 0xB1, Block Device Characteristics: the rotation rate (bytes
/// 4-5; 1 is non-rotating, 0 not reported).
pub fn scsi_vpd_media(page: &[u8]) -> Option<&'static str> {
    let body = vpd_body(page, 0xB1)?;
    match u16::from_be_bytes([*body.first()?, *body.get(1)?]) {
        1 => Some("ssd"),
        0x0401..=0xFFFE => Some("hdd"),
        _ => None,
    }
}

/// Walk an SMBIOS structure table: Type 38/42 (a BMC), Type 4 (processors)
/// and Type 17 (memory devices). Bounds-checked against the table and each
/// structure's own length; a short or broken table yields what it held.
pub fn parse_smbios(table: &[u8]) -> Smbios {
    let mut out = Smbios::default();
    let mut cpu = Cpu::default();
    let mut at = 0usize;
    while at + 4 <= table.len() {
        let ty = table[at];
        let len = table[at + 1] as usize;
        if len < 4 || at + len > table.len() {
            break;
        }
        let s = &table[at..at + len];
        // The string set runs to a double NUL.
        let mut end = at + len;
        while end + 1 < table.len() && !(table[end] == 0 && table[end + 1] == 0) {
            end += 1;
        }
        let strings = &table[at + len..end.min(table.len())];
        let text = |at: usize| (len > at).then(|| smbios_string(strings, s[at])).flatten();
        match ty {
            0 => {
                out.firmware.vendor = text(0x04);
                out.firmware.version = text(0x05);
                out.firmware_date = text(0x08);
            }
            1 => {
                out.system = Named { vendor: text(0x04), product: text(0x05), version: text(0x06), serial: text(0x07) };
                if len >= 0x18 {
                    out.uuid = uuid_text(&s[0x08..0x18]);
                }
            }
            // The first board is the system's.
            2 if out.board == Named::default() => {
                out.board = Named { vendor: text(0x04), product: text(0x05), version: text(0x06), serial: text(0x07) };
            }
            38 | 42 => out.bmc = true,
            // Socket populated: status bit 6.
            4 if len > 0x18 && s[0x18] & 0x40 != 0 => {
                cpu.sockets += 1;
                if cpu.model.is_none() && len > 0x10 {
                    cpu.model = smbios_string(strings, s[0x10]);
                }
                if len >= 0x16 {
                    cpu.max_mhz = cpu.max_mhz.max(le16(s, 0x14) as u32);
                }
                // Core and thread counts (2.5); the 16-bit fields (3.0)
                // when the byte says 0xFF.
                if len > 0x25 {
                    let mut cores = s[0x23] as u32;
                    let mut threads = s[0x25] as u32;
                    if cores == 0xFF && len >= 0x2C {
                        cores = le16(s, 0x2A) as u32;
                    }
                    if threads == 0xFF && len >= 0x30 {
                        threads = le16(s, 0x2E) as u32;
                    }
                    cpu.cores += cores;
                    cpu.threads += threads;
                }
            }
            17 if len >= 0x0E => {
                let size = le16(s, 0x0C);
                let mb = match size {
                    0 | 0xFFFF => 0,
                    // Extended size, in MB (2.7).
                    0x7FFF if len >= 0x20 => (le32(s, 0x1C) & 0x7FFF_FFFF) as u64,
                    0x7FFF => 0,
                    s if s & 0x8000 != 0 => (s & 0x7FFF) as u64 / 1024,
                    s => s as u64,
                };
                if mb > 0 {
                    out.memory_mb += mb;
                    out.dimms += 1;
                }
            }
            127 => break,
            _ => {}
        }
        at = end + 2;
    }
    if cpu.sockets > 0 {
        out.cpu = Some(cpu);
    }
    out
}

/// A string made safe between JSON quotes: quotes and backslashes escaped,
/// control characters dropped. Firmware-supplied text, all of it.
fn js(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars().filter(|c| !c.is_control()) {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn nic_json(n: &Nic) -> String {
    let mut f = Vec::new();
    f.push(format!("\"mac\":{}", js(&n.mac)));
    f.push(format!("\"link\":{}", n.link));
    if let Some(d) = &n.driver {
        f.push(format!("\"driver\":{}", js(d)));
    }
    f.push(format!("\"media_driver\":{}", n.media_driver));
    if let Some(p) = &n.pci {
        f.push(format!("\"pci\":{}", js(p)));
    }
    if let Some((v, d)) = n.id {
        f.push(format!("\"id\":\"{v:04x}:{d:04x}\""));
    }
    if n.mtu > 0 {
        f.push(format!("\"mtu\":{}", n.mtu));
    }
    format!("{{{}}}", f.join(","))
}

fn controller_json(c: &Controller) -> String {
    let (v, d) = c.id;
    let (cl, sub, pi) = c.class;
    let mut s = format!(
        "{{\"pci\":{},\"id\":\"{v:04x}:{d:04x}\",\"class\":\"{cl:02x}:{sub:02x}:{pi:02x}\"",
        js(&c.pci)
    );
    if let Some(dr) = &c.driver {
        s.push_str(&format!(",\"driver\":{}", js(dr)));
    }
    s.push('}');
    s
}

fn opt(f: &mut Vec<String>, key: &str, v: &Option<String>) {
    if let Some(v) = v {
        f.push(format!("\"{key}\":{}", js(v)));
    }
}

fn part_json(p: &Part) -> String {
    let mut f = vec![format!("\"n\":{}", p.number), format!("\"kind\":{}", js(&p.kind)), format!("\"bytes\":{}", p.bytes)];
    opt(&mut f, "name", &p.name);
    if !p.efi.is_empty() {
        let dirs: Vec<String> = p.efi.iter().map(|d| js(d)).collect();
        f.push(format!("\"efi\":[{}]", dirs.join(",")));
    }
    format!("{{{}}}", f.join(","))
}

fn disk_json(d: &Disk) -> String {
    let mut f = Vec::new();
    if let Some(b) = d.bus {
        f.push(format!("\"bus\":\"{b}\""));
    }
    opt(&mut f, "path", &d.path);
    opt(&mut f, "model", &d.id.model);
    opt(&mut f, "serial", &d.id.serial);
    opt(&mut f, "firmware", &d.id.firmware);
    opt(&mut f, "wwn", &d.id.wwn);
    if let Some(m) = d.id.media {
        f.push(format!("\"media\":\"{m}\""));
    }
    f.push(format!("\"blocks\":{},\"block_size\":{}", d.blocks, d.block_size));
    if let Some(p) = d.id.physical_block_size {
        f.push(format!("\"physical_block_size\":{p}"));
    }
    f.push(format!("\"removable\":{},\"medium\":{}", d.removable, d.medium));
    if let Some(t) = d.table {
        f.push(format!("\"table\":\"{t}\""));
    }
    if !d.partitions.is_empty() {
        let parts: Vec<String> = d.partitions.iter().map(part_json).collect();
        f.push(format!("\"partitions\":[{}]", parts.join(",")));
    }
    if d.partitions_omitted > 0 {
        f.push(format!("\"partitions_omitted\":{}", d.partitions_omitted));
    }
    format!("{{{}}}", f.join(","))
}

fn named_json(key: &str, n: &Named, extra: &[(&str, &Option<String>)]) -> String {
    let mut f = Vec::new();
    opt(&mut f, "vendor", &n.vendor);
    opt(&mut f, "product", &n.product);
    opt(&mut f, "version", &n.version);
    opt(&mut f, "serial", &n.serial);
    for (k, v) in extra {
        opt(&mut f, k, v);
    }
    if f.is_empty() {
        return String::new();
    }
    format!(",\"{key}\":{{{}}}", f.join(","))
}

/// A disk's one console line (#167), so the inventory reaches the log
/// collector too (#162): bus, model, serial, WWN, size, sectors, media.
pub fn disk_line(d: &Disk) -> String {
    let mut f: Vec<String> = Vec::new();
    f.push(String::from(d.bus.unwrap_or("disk")));
    if let Some(m) = &d.id.model {
        f.push(m.clone());
    }
    if let Some(x) = &d.id.serial {
        f.push(format!("serial {x}"));
    }
    if let Some(x) = &d.id.firmware {
        f.push(format!("fw {x}"));
    }
    if let Some(x) = &d.id.wwn {
        f.push(format!("wwn {x}"));
    }
    f.push(size_text(d.blocks * d.block_size as u64));
    match d.id.physical_block_size {
        Some(p) => f.push(format!("{} B sectors ({p} B physical)", d.block_size)),
        None => f.push(format!("{} B sectors", d.block_size)),
    }
    if let Some(m) = d.id.media {
        f.push(String::from(m));
    }
    if d.medium {
        f.push(String::from("the boot medium"));
    } else if d.removable {
        f.push(String::from("removable"));
    }
    f.join("  ")
}

/// A disk's partitions on one line: `gpt: 1 esp 64 MiB (EFI: BOOT, ubuntu),
/// 2 linux 960 MiB`, or what the table was when there are none.
pub fn partitions_line(d: &Disk) -> Option<String> {
    let t = d.table?;
    if d.partitions.is_empty() {
        return Some(format!("{t}: no partitions"));
    }
    let parts: Vec<String> = d
        .partitions
        .iter()
        .map(|p| {
            let mut s = format!("{} {} {}", p.number, p.kind, size_text(p.bytes));
            if let Some(n) = &p.name {
                s.push_str(&format!(" \"{n}\""));
            }
            if !p.efi.is_empty() {
                s.push_str(&format!(" (EFI: {})", p.efi.join(", ")));
            }
            s
        })
        .collect();
    let more = if d.partitions_omitted > 0 { format!(", and {} more", d.partitions_omitted) } else { String::new() };
    Some(format!("{t}: {}{more}", parts.join(", ")))
}

/// Bytes as the unit a person reads them in: B to TiB, one decimal below
/// 100 of a unit (integers only: the UEFI target has no hardware float).
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut u = 0;
    let mut div = 1u64;
    while bytes / div >= 1024 && u < UNITS.len() - 1 {
        div *= 1024;
        u += 1;
    }
    let tenths = (bytes as u128 * 10 / div as u128) as u64;
    if u == 0 || tenths >= 1000 || tenths % 10 == 0 {
        format!("{} {}", tenths / 10, UNITS[u])
    } else {
        format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[u])
    }
}

/// The inventory as the claim's `"inventory"` object, never over `BUDGET`
/// bytes. Since #167 (`"v":2`) CPU and memory go whether or not there is a
/// BMC (the owner: a BMC that answers nothing is the case this is for), with
/// the firmware, system and board from SMBIOS, and every disk's identity
/// and partitions.
pub fn render(nics: &[Nic], storage: &[Controller], disks: &[Disk], smbios: &Smbios) -> String {
    let mut tail = String::new();
    tail.push_str(&named_json("system", &smbios.system, &[("uuid", &smbios.uuid)]));
    tail.push_str(&named_json("board", &smbios.board, &[]));
    tail.push_str(&named_json("firmware", &smbios.firmware, &[("date", &smbios.firmware_date)]));
    if let Some(c) = &smbios.cpu {
        tail.push_str(",\"cpu\":{");
        if let Some(m) = &c.model {
            tail.push_str(&format!("\"model\":{},", js(m)));
        }
        tail.push_str(&format!(
            "\"sockets\":{},\"cores\":{},\"threads\":{},\"max_mhz\":{}}}",
            c.sockets, c.cores, c.threads, c.max_mhz
        ));
    }
    if smbios.dimms > 0 {
        tail.push_str(&format!(",\"memory\":{{\"mb\":{},\"dimms\":{}}}", smbios.memory_mb, smbios.dimms));
    }
    let list = |items: Vec<String>, room: usize| -> (String, usize) {
        let mut s = String::from("[");
        let mut kept = 0;
        for it in items {
            if s.len() + it.len() + 2 > room {
                break;
            }
            if kept > 0 {
                s.push(',');
            }
            s.push_str(&it);
            kept += 1;
        }
        s.push(']');
        (s, kept)
    };
    let head = format!("{{\"v\":2,\"bmc\":{}", smbios.bmc);
    // Fixed parts first, then what is left goes to NICs, controllers and disks
    // in that order. Each list stops at its room rather than being cut mid-way.
    let fixed = head.len() + tail.len() + 96;
    let mut room = BUDGET.saturating_sub(fixed);
    let (n, _) = list(nics.iter().map(nic_json).collect(), room);
    room = room.saturating_sub(n.len());
    let (c, _) = list(storage.iter().map(controller_json).collect(), room);
    room = room.saturating_sub(c.len());
    let (d, kept) = list(disks.iter().map(disk_json).collect(), room.saturating_sub(24));
    let mut out = format!("{head},\"nics\":{n},\"storage\":{c},\"disk_count\":{},\"disks\":{d}", disks.len());
    if kept < disks.len() {
        out.push_str(&format!(",\"disks_omitted\":{}", disks.len() - kept));
    }
    out.push_str(&tail);
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// One SMBIOS structure: the formatted section, then its strings.
    fn st(ty: u8, mut body: Vec<u8>, strings: &[&str]) -> Vec<u8> {
        let len = 4 + body.len();
        let mut s = vec![ty, len as u8, 0, 0];
        s.append(&mut body);
        if strings.is_empty() {
            s.extend_from_slice(&[0, 0]);
        } else {
            for x in strings {
                s.extend_from_slice(x.as_bytes());
                s.push(0);
            }
            s.push(0);
        }
        s
    }

    /// A Type 4 of `len` bytes: version string 1, max speed, populated,
    /// cores and threads.
    fn cpu4(mhz: u16, cores: u8, threads: u8, populated: bool, model: &str) -> Vec<u8> {
        let mut b = vec![0u8; 0x30 - 4];
        let put = |b: &mut Vec<u8>, at: usize, v: u8| b[at - 4] = v;
        put(&mut b, 0x10, 1);
        put(&mut b, 0x14, mhz as u8);
        put(&mut b, 0x15, (mhz >> 8) as u8);
        put(&mut b, 0x18, if populated { 0x41 } else { 0x00 });
        put(&mut b, 0x23, cores);
        put(&mut b, 0x25, threads);
        st(4, b, &[model])
    }

    fn mem17(size: u16, ext_mb: u32) -> Vec<u8> {
        let mut b = vec![0u8; 0x22 - 4];
        b[0x0C - 4..0x0E - 4].copy_from_slice(&size.to_le_bytes());
        b[0x1C - 4..0x20 - 4].copy_from_slice(&ext_mb.to_le_bytes());
        st(17, b, &[])
    }

    fn end() -> Vec<u8> {
        st(127, vec![], &[])
    }

    #[test]
    fn a_two_socket_server_with_a_bmc() {
        let mut t = Vec::new();
        t.extend(st(1, vec![0; 20], &["Dell Inc.", "PowerEdge"]));
        t.extend(cpu4(3600, 8, 16, true, "Intel(R) Xeon(R) E5-2650"));
        t.extend(cpu4(3600, 8, 16, true, "Intel(R) Xeon(R) E5-2650"));
        t.extend(cpu4(0, 0, 0, false, "")); // an empty socket
        t.extend(mem17(16384, 0));
        t.extend(mem17(16384, 0));
        t.extend(mem17(0, 0)); // an empty slot
        t.extend(mem17(0xFFFF, 0)); // unknown
        t.extend(st(38, vec![0; 14], &[]));
        t.extend(end());
        let s = parse_smbios(&t);
        assert!(s.bmc);
        assert_eq!(s.memory_mb, 32768);
        assert_eq!(s.dimms, 2);
        let c = s.cpu.unwrap();
        assert_eq!((c.sockets, c.cores, c.threads, c.max_mhz), (2, 16, 32, 3600));
        assert_eq!(c.model.as_deref(), Some("Intel(R) Xeon(R) E5-2650"));
    }

    #[test]
    fn sizes_in_kb_extended_and_a_host_interface() {
        let mut t = Vec::new();
        t.extend(mem17(0x8000 | 16384, 0)); // bit 15: KB units, so 16 MB
        t.extend(mem17(0x7FFF, 65536)); // the extended size: 64 GiB
        t.extend(st(42, vec![0; 2], &[]));
        t.extend(end());
        let s = parse_smbios(&t);
        assert!(s.bmc, "Type 42 is a BMC too");
        assert_eq!(s.dimms, 2);
        assert_eq!(s.memory_mb, 16 + 65536);
    }

    #[test]
    fn a_truncated_table_yields_what_it_held() {
        let mut t = cpu4(2000, 2, 4, true, "QEMU");
        t.extend(mem17(1024, 0));
        let mut cut = t.clone();
        cut.truncate(t.len() - 10);
        let s = parse_smbios(&cut);
        assert_eq!(s.cpu.unwrap().cores, 2);
        assert!(!s.bmc);
        assert_eq!(parse_smbios(&[]), Smbios::default());
        assert_eq!(parse_smbios(&[4, 2, 0]), Smbios::default());
    }

    fn sample() -> (Vec<Nic>, Vec<Controller>, Vec<Disk>) {
        let nics = vec![Nic {
            mac: "52:54:00:12:34:56".into(),
            link: true,
            driver: Some("Virtio \"Network\" Driver\n".into()),
            media_driver: false,
            pci: Some("0000:00:02.0".into()),
            id: Some((0x1af4, 0x1000)),
            mtu: 1500,
        }];
        let storage = vec![Controller {
            pci: "0000:00:1f.2".into(),
            id: (0x8086, 0x2922),
            class: (1, 6, 1),
            driver: None,
        }];
        let disks = vec![
            Disk {
                path: Some("PciRoot(0x0)/Pci(0x1F,0x2)/Sata(0x2,0xFFFF,0x0)".into()),
                blocks: 2048,
                block_size: 2048,
                removable: true,
                medium: true,
                ..Disk::default()
            },
            Disk {
                bus: Some("nvme"),
                path: Some("PciRoot(0x0)/Pci(0x4,0x0)/NVMe(0x1,…)".into()),
                id: DriveId {
                    model: Some("QEMU NVMe Ctrl".into()),
                    serial: Some("NVME0001".into()),
                    firmware: Some("9.1.0".into()),
                    wwn: Some("eui.0011223344556677".into()),
                    media: Some("nvme"),
                    physical_block_size: None,
                },
                blocks: 2097152,
                block_size: 512,
                table: Some("gpt"),
                partitions: vec![
                    Part { number: 1, kind: "esp".into(), bytes: 64 << 20, name: Some("EFI".into()), efi: vec!["BOOT".into(), "ubuntu".into()] },
                    Part { number: 2, kind: "linux".into(), bytes: 960 << 20, name: None, efi: vec![] },
                ],
                ..Disk::default()
            },
        ];
        (nics, storage, disks)
    }

    #[test]
    fn rendered_as_the_engine_keeps_it() {
        let (n, c, d) = sample();
        let s = Smbios {
            bmc: true,
            cpu: Some(Cpu { model: Some("QEMU".into()), sockets: 1, cores: 2, threads: 2, max_mhz: 2000 }),
            memory_mb: 1024,
            dimms: 1,
            firmware: Named { vendor: Some("EDK II".into()), version: Some("1.0".into()), ..Named::default() },
            firmware_date: Some("02/06/2015".into()),
            system: Named { vendor: Some("QEMU".into()), product: Some("Standard PC".into()), serial: Some("S1".into()), ..Named::default() },
            uuid: Some("00112233-4455-6677-8899-AABBCCDDEEFF".into()),
            board: Named::default(),
        };
        assert_eq!(
            render(&n, &c, &d, &s),
            "{\"v\":2,\"bmc\":true,\
             \"nics\":[{\"mac\":\"52:54:00:12:34:56\",\"link\":true,\"driver\":\"Virtio \\\"Network\\\" Driver\",\
             \"media_driver\":false,\"pci\":\"0000:00:02.0\",\"id\":\"1af4:1000\",\"mtu\":1500}],\
             \"storage\":[{\"pci\":\"0000:00:1f.2\",\"id\":\"8086:2922\",\"class\":\"01:06:01\"}],\
             \"disk_count\":2,\
             \"disks\":[{\"path\":\"PciRoot(0x0)/Pci(0x1F,0x2)/Sata(0x2,0xFFFF,0x0)\",\"blocks\":2048,\
             \"block_size\":2048,\"removable\":true,\"medium\":true},\
             {\"bus\":\"nvme\",\"path\":\"PciRoot(0x0)/Pci(0x4,0x0)/NVMe(0x1,…)\",\"model\":\"QEMU NVMe Ctrl\",\
             \"serial\":\"NVME0001\",\"firmware\":\"9.1.0\",\"wwn\":\"eui.0011223344556677\",\"media\":\"nvme\",\
             \"blocks\":2097152,\"block_size\":512,\"removable\":false,\"medium\":false,\"table\":\"gpt\",\
             \"partitions\":[{\"n\":1,\"kind\":\"esp\",\"bytes\":67108864,\"name\":\"EFI\",\"efi\":[\"BOOT\",\"ubuntu\"]},\
             {\"n\":2,\"kind\":\"linux\",\"bytes\":1006632960}]}],\
             \"system\":{\"vendor\":\"QEMU\",\"product\":\"Standard PC\",\"serial\":\"S1\",\
             \"uuid\":\"00112233-4455-6677-8899-AABBCCDDEEFF\"},\
             \"firmware\":{\"vendor\":\"EDK II\",\"version\":\"1.0\",\"date\":\"02/06/2015\"},\
             \"cpu\":{\"model\":\"QEMU\",\"sockets\":1,\"cores\":2,\"threads\":2,\"max_mhz\":2000},\
             \"memory\":{\"mb\":1024,\"dimms\":1}}"
        );
    }

    #[test]
    fn the_console_lines() {
        let (_, _, d) = sample();
        assert_eq!(disk_line(&d[0]), "disk  4 MiB  2048 B sectors  the boot medium");
        assert_eq!(
            disk_line(&d[1]),
            "nvme  QEMU NVMe Ctrl  serial NVME0001  fw 9.1.0  wwn eui.0011223344556677  1 GiB  512 B sectors  nvme"
        );
        assert_eq!(partitions_line(&d[0]), None);
        assert_eq!(
            partitions_line(&d[1]).unwrap(),
            "gpt: 1 esp 64 MiB \"EFI\" (EFI: BOOT, ubuntu), 2 linux 960 MiB"
        );
        assert_eq!(size_text(0), "0 B");
        assert_eq!(size_text(1536), "1.5 KiB");
        assert_eq!(size_text(500_107_862_016), "465 GiB");
        assert_eq!(size_text(4_000_787_030_016), "3.6 TiB");
    }

    #[test]
    fn a_long_disk_list_is_cut_to_the_budget_and_counted() {
        let (n, c, _) = sample();
        let disk = Disk { path: Some("x".repeat(300)), blocks: 1, block_size: 512, ..Disk::default() };
        let disks = vec![disk; 200];
        let s = Smbios::default();
        let r = render(&n, &c, &disks, &s);
        assert!(r.len() <= BUDGET, "{}", r.len());
        assert!(r.ends_with('}') && r.contains("\"disks_omitted\":"), "{}", &r[r.len() - 40..]);
        assert!(r.contains("\"disk_count\":200,"));
        let kept = r.matches("\"medium\":false").count();
        assert!(kept > 10 && kept < 200);
        assert!(r.contains(&format!("\"disks_omitted\":{}", 200 - kept)));
    }

    /// An ATA string field as IDENTIFY lays it: byte-swapped in each word.
    fn ata_put(id: &mut [u8], word: usize, words: usize, text: &str) {
        let mut b = vec![b' '; words * 2];
        b[..text.len()].copy_from_slice(text.as_bytes());
        for (i, pair) in b.chunks(2).enumerate() {
            id[(word + i) * 2] = pair[1];
            id[(word + i) * 2 + 1] = pair[0];
        }
    }

    fn ata_word(id: &mut [u8], word: usize, v: u16) {
        id[word * 2..word * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn ata_identify_as_qemu_answers_it() {
        let mut id = vec![0u8; 512];
        ata_put(&mut id, 10, 10, "SATA0001");
        ata_put(&mut id, 23, 4, "2.5+");
        ata_put(&mut id, 27, 20, "QEMU HARDDISK");
        ata_word(&mut id, 87, 0x4100);
        for (i, w) in [0x5000u16, 0xc500, 0xa1b2, 0xc3d4].into_iter().enumerate() {
            ata_word(&mut id, 108 + i, w);
        }
        // 4096-byte physical sectors under 512-byte logical ones (2^3).
        ata_word(&mut id, 106, 0x6003);
        ata_word(&mut id, 217, 1);
        let d = ata_identify(&id);
        assert_eq!(d.model.as_deref(), Some("QEMU HARDDISK"));
        assert_eq!(d.serial.as_deref(), Some("SATA0001"));
        assert_eq!(d.firmware.as_deref(), Some("2.5+"));
        assert_eq!(d.wwn.as_deref(), Some("naa.5000c500a1b2c3d4"));
        assert_eq!(d.physical_block_size, Some(4096));
        assert_eq!(d.media, Some("ssd"));
        // A spinning disk with no WWN word and no sector word.
        ata_word(&mut id, 87, 0x4000);
        ata_word(&mut id, 106, 0);
        ata_word(&mut id, 217, 7200);
        let d = ata_identify(&id);
        assert_eq!((d.wwn, d.physical_block_size, d.media), (None, None, Some("hdd")));
        assert_eq!(ata_identify(&[0u8; 100]), DriveId::default());
    }

    #[test]
    fn nvme_identify() {
        let mut c = vec![0u8; 4096];
        c[4..24].copy_from_slice(b"NVME0001            ");
        c[24..64].copy_from_slice(b"QEMU NVMe Ctrl                          ");
        c[64..72].copy_from_slice(b"9.1.0   ");
        let d = nvme_controller(&c);
        assert_eq!((d.model.as_deref(), d.serial.as_deref(), d.firmware.as_deref()), (Some("QEMU NVMe Ctrl"), Some("NVME0001"), Some("9.1.0")));
        assert_eq!(d.media, Some("nvme"));
        let mut ns = vec![0u8; 4096];
        assert_eq!(nvme_namespace_wwn(&ns), None);
        ns[120..128].copy_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77]);
        assert_eq!(nvme_namespace_wwn(&ns).as_deref(), Some("eui.0011223344556677"));
        ns[104] = 0xAB;
        assert_eq!(nvme_namespace_wwn(&ns).as_deref(), Some("eui.ab000000000000000000000000000000"));
    }

    #[test]
    fn scsi_inquiry_and_vpd() {
        let mut inq = vec![0u8; 36];
        inq[8..16].copy_from_slice(b"QEMU    ");
        inq[16..32].copy_from_slice(b"QEMU HARDDISK   ");
        inq[32..36].copy_from_slice(b"2.5+");
        let d = scsi_inquiry(&inq);
        assert_eq!((d.model.as_deref(), d.firmware.as_deref()), (Some("QEMU QEMU HARDDISK"), Some("2.5+")));
        assert_eq!(scsi_vpd_serial(&[0, 0x80, 0, 6, b'S', b'C', b'S', b'I', b'0', b' ']).as_deref(), Some("SCSI0"));
        assert_eq!(scsi_vpd_serial(&[0, 0x83, 0, 1, b'x']), None, "another page");
        // An ASCII T10 vendor id (code set 2), then the LU's NAA (binary, type 3).
        let mut p83 = vec![0, 0x83, 0, 0];
        p83.extend_from_slice(&[2, 1, 0, 4, b'Q', b'E', b'M', b'U']);
        p83.extend_from_slice(&[1, 3, 0, 8, 0x50, 0x00, 0xc5, 0x00, 0xa1, 0xb2, 0xc3, 0xd5]);
        p83[3] = (p83.len() - 4) as u8;
        assert_eq!(scsi_vpd_wwn(&p83).as_deref(), Some("naa.5000c500a1b2c3d5"));
        // A target port's NAA (association 1) is not the disk's.
        let p = [0, 0x83, 0, 12, 1, 0x13, 0, 8, 1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(scsi_vpd_wwn(&p), None);
        // A truncated designator is not read past the page.
        assert_eq!(scsi_vpd_wwn(&[0, 0x83, 0, 6, 1, 3, 0, 8, 0x50, 0]), None);
        assert_eq!(scsi_vpd_media(&[0, 0xB1, 0, 60, 0, 1]), Some("ssd"));
        assert_eq!(scsi_vpd_media(&[0, 0xB1, 0, 60, 0x1C, 0x20]), Some("hdd"));
        assert_eq!(scsi_vpd_media(&[0, 0xB1, 0, 60, 0, 0]), None);
    }

    #[test]
    fn partition_kinds() {
        // The ESP's type as GPT stores it.
        let esp = [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B];
        assert_eq!(guid_text(&esp), "C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
        assert_eq!(partition_kind(&esp), "esp");
        assert_eq!(partition_kind(&[0x11; 16]), "11111111-1111-1111-1111-111111111111");
        assert_eq!(mbr_kind(0x83), "linux");
        assert_eq!(mbr_kind(0x42), "mbr-42");
    }

    #[test]
    fn smbios_names_the_system_board_and_firmware() {
        let mut t = Vec::new();
        let mut b0 = vec![0u8; 0x12 - 4];
        b0[0x04 - 4] = 1;
        b0[0x05 - 4] = 2;
        b0[0x08 - 4] = 3;
        t.extend(st(0, b0, &["American Megatrends Inc.", "3.2", "06/12/2015"]));
        let mut b1 = vec![0u8; 0x1B - 4];
        b1[0x04 - 4] = 1;
        b1[0x05 - 4] = 2;
        b1[0x07 - 4] = 3;
        b1[0x08 - 4..0x18 - 4].copy_from_slice(&[0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        t.extend(st(1, b1, &["Supermicro", "X9SRD-F", "0123456789"]));
        let mut b2 = vec![0u8; 0x0F - 4];
        b2[0x04 - 4] = 1;
        b2[0x05 - 4] = 2;
        t.extend(st(2, b2.clone(), &["Supermicro", "X9SRD-F"]));
        b2[0x05 - 4] = 1;
        t.extend(st(2, b2, &["a second board"]));
        t.extend(end());
        let s = parse_smbios(&t);
        assert_eq!(s.firmware.vendor.as_deref(), Some("American Megatrends Inc."));
        assert_eq!(s.firmware.version.as_deref(), Some("3.2"));
        assert_eq!(s.firmware_date.as_deref(), Some("06/12/2015"));
        assert_eq!(s.system.product.as_deref(), Some("X9SRD-F"));
        assert_eq!(s.system.serial.as_deref(), Some("0123456789"));
        assert_eq!(s.system.version, None);
        assert_eq!(s.uuid.as_deref(), Some("00112233-4455-6677-8899-AABBCCDDEEFF"));
        assert_eq!(s.board.product.as_deref(), Some("X9SRD-F"), "the first board");
    }
}
