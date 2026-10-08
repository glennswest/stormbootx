//! The firmware inventory sent with the claim (#4, #20): what this machine's
//! firmware sees that its BMC can't tell anyone.
//!
//! The owner's decision on #20 (2026-10-02, option 2): report **only what the
//! BMC can't give**. That is each NIC's MAC as firmware sees it and which
//! driver bound it (the firmware's own or a stormnic driver from the media),
//! and the storage controllers and disks UEFI sees. A machine **with no BMC**
//! (pve VMs, homelab boxes) sends the full set: CPU and memory as well, which
//! stormipmi otherwise reads over Redfish. "No BMC" is SMBIOS having no
//! Type 38 (IPMI device) and no Type 42 (management controller host
//! interface) structure.
//!
//! It goes in the claim body as `"inventory"`, beside `"agent"` (#90). The
//! engine keeps it in the host's last-claim record as given (stormblock#177),
//! and drops anything that is not a JSON object or is over 16 KiB, without
//! failing the claim. So `render` keeps it an object well under that: NICs
//! and controllers first, then disks while they fit (the rest counted in
//! `disks_omitted`), then CPU and memory.
//!
//! ```json
//! {"v":1,"bmc":false,
//!  "nics":[{"mac":"52:54:00:12:34:56","link":true,"driver":"Virtio Network Driver",
//!           "media_driver":false,"pci":"0000:00:02.0","id":"1af4:1000"}],
//!  "storage":[{"pci":"0000:00:1f.2","id":"8086:2922","class":"01:06:01","driver":"…"}],
//!  "disks":[{"path":"PciRoot(0x0)/…","blocks":2048,"block_size":2048,
//!            "removable":true,"medium":true}],
//!  "cpu":{"model":"…","sockets":1,"cores":4,"threads":8,"max_mhz":3000},
//!  "memory":{"mb":1024,"dimms":1}}
//! ```
//!
//! Same constraints as `sha256.rs` and the other core-only modules: `core`
//! and `alloc` only and no `crate::` item, so the tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/inventory.rs -o t/inventory-test && ./t/inventory-test
//! ```
//!
//! The collecting (SNP, PCI I/O, BlockIO, the SMBIOS table) is `hardware.rs`.

extern crate alloc;

use alloc::format;
use alloc::string::String;
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
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cpu {
    pub model: Option<String>,
    pub sockets: u32,
    pub cores: u32,
    pub threads: u32,
    pub max_mhz: u32,
}

/// What SMBIOS says: whether there is a BMC, and the CPUs and memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Smbios {
    pub bmc: bool,
    pub cpu: Option<Cpu>,
    pub memory_mb: u64,
    pub dimms: u32,
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
        match ty {
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

fn disk_json(d: &Disk) -> String {
    let mut s = String::from("{");
    if let Some(p) = &d.path {
        s.push_str(&format!("\"path\":{},", js(p)));
    }
    s.push_str(&format!(
        "\"blocks\":{},\"block_size\":{},\"removable\":{},\"medium\":{}}}",
        d.blocks, d.block_size, d.removable, d.medium
    ));
    s
}

/// The inventory as the claim's `"inventory"` object, never over `BUDGET`
/// bytes. CPU and memory only without a BMC (#20).
pub fn render(nics: &[Nic], storage: &[Controller], disks: &[Disk], smbios: &Smbios) -> String {
    let mut tail = String::new();
    if !smbios.bmc {
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
    let head = format!("{{\"v\":1,\"bmc\":{}", smbios.bmc);
    // Fixed parts first, then what is left goes to NICs, controllers and disks
    // in that order. Each list stops at its room rather than being cut mid-way.
    let fixed = head.len() + tail.len() + 64;
    let mut room = BUDGET.saturating_sub(fixed);
    let (n, _) = list(nics.iter().map(nic_json).collect(), room);
    room = room.saturating_sub(n.len());
    let (c, _) = list(storage.iter().map(controller_json).collect(), room);
    room = room.saturating_sub(c.len());
    let (d, kept) = list(disks.iter().map(disk_json).collect(), room.saturating_sub(24));
    let mut out = format!("{head},\"nics\":{n},\"storage\":{c},\"disks\":{d}");
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
        }];
        let storage = vec![Controller {
            pci: "0000:00:1f.2".into(),
            id: (0x8086, 0x2922),
            class: (1, 6, 1),
            driver: None,
        }];
        let disks = vec![Disk { path: Some("PciRoot(0x0)/Pci(0x1F,0x2)/Sata(0x2,0xFFFF,0x0)".into()), blocks: 2048, block_size: 2048, removable: true, medium: true }];
        (nics, storage, disks)
    }

    #[test]
    fn rendered_as_the_engine_keeps_it() {
        let (n, c, d) = sample();
        let s = Smbios { bmc: false, cpu: Some(Cpu { model: Some("QEMU".into()), sockets: 1, cores: 2, threads: 2, max_mhz: 2000 }), memory_mb: 1024, dimms: 1 };
        assert_eq!(
            render(&n, &c, &d, &s),
            "{\"v\":1,\"bmc\":false,\
             \"nics\":[{\"mac\":\"52:54:00:12:34:56\",\"link\":true,\"driver\":\"Virtio \\\"Network\\\" Driver\",\
             \"media_driver\":false,\"pci\":\"0000:00:02.0\",\"id\":\"1af4:1000\"}],\
             \"storage\":[{\"pci\":\"0000:00:1f.2\",\"id\":\"8086:2922\",\"class\":\"01:06:01\"}],\
             \"disks\":[{\"path\":\"PciRoot(0x0)/Pci(0x1F,0x2)/Sata(0x2,0xFFFF,0x0)\",\"blocks\":2048,\
             \"block_size\":2048,\"removable\":true,\"medium\":true}],\
             \"cpu\":{\"model\":\"QEMU\",\"sockets\":1,\"cores\":2,\"threads\":2,\"max_mhz\":2000},\
             \"memory\":{\"mb\":1024,\"dimms\":1}}"
        );
    }

    #[test]
    fn with_a_bmc_no_cpu_or_memory() {
        let (n, c, d) = sample();
        let s = Smbios { bmc: true, cpu: Some(Cpu::default()), memory_mb: 1024, dimms: 1 };
        let r = render(&n, &c, &d, &s);
        assert!(r.starts_with("{\"v\":1,\"bmc\":true,"));
        assert!(!r.contains("\"cpu\"") && !r.contains("\"memory\""), "{r}");
    }

    #[test]
    fn a_long_disk_list_is_cut_to_the_budget_and_counted() {
        let (n, c, _) = sample();
        let disk = Disk { path: Some("x".repeat(300)), blocks: 1, block_size: 512, removable: false, medium: false };
        let disks = vec![disk; 200];
        let s = Smbios::default();
        let r = render(&n, &c, &disks, &s);
        assert!(r.len() <= BUDGET, "{}", r.len());
        assert!(r.ends_with('}') && r.contains("\"disks_omitted\":"), "{}", &r[r.len() - 40..]);
        let kept = r.matches("\"medium\":false").count();
        assert!(kept > 10 && kept < 200);
        assert!(r.contains(&format!("\"disks_omitted\":{}", 200 - kept)));
    }
}
