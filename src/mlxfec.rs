//! Set the 25G FEC of a Mellanox ConnectX NIC from firmware, before any OS.
//!
//! The fabric switch (a Dell S5148F on OS10 10.4.3.6) has no `fec auto` and
//! cannot be upgraded, so every 25G port on it is pinned to one FEC mode. The
//! NIC must match, and the only moment this project owns on a machine before
//! the network is asked for anything is this one: a UEFI binary with no OS
//! behind it. There is no MFT and no `mlxconfig` anywhere in the fleet, and
//! there is no Linux on the box yet to run one in.
//!
//! So this is the `mlxconfig` path, reimplemented. **Not** the HCA command
//! queue the OS driver uses: that needs DMA pages, an initialised HCA and sole
//! ownership of the device, and the Mellanox UEFI driver already owns it.
//! `mlxconfig` sidesteps all of that through the PCIe vendor-specific
//! capability, which exposes a semaphore plus an address/data window into the
//! card's configuration space; an ICMD goes through that window, and an
//! access-register command inside the ICMD reads or writes the non-volatile
//! configuration TLV that decides the FEC at the next NIC reset. Nothing here
//! talks to the driver, and the driver never notices.
//!
//! The value written is a *next-boot* value. Firmware reads NV configuration
//! when the card resets, so a write is followed by MFRL (the firmware's own
//! "prepare for warm boot" request, exactly what `mlxconfig` sends) and one
//! warm reset of the machine. It reboots **at most once per change**: the
//! decision to reset is taken only when this boot actually wrote something and
//! the readback shows the write landed; a card whose next-boot value already
//! matches is reported and left alone, whatever its current value says. A
//! firmware that needs a cold reset therefore costs a console line per boot,
//! never a loop.
//!
//! Only a card this code recognises is touched. The physical-function ids of
//! ConnectX-4 through ConnectX-7 are listed, the hardware id read from the
//! card has to be one whose ICMD readiness probe is known, and the capability
//! has to be the functional one with every address space this needs. Anything
//! else on the bus — another vendor, a virtual function, a card in recovery —
//! is named on the console and skipped. **Nothing here is fatal**: every
//! failure is a line and the boot continues, on whatever FEC the card has.
//!
//! Every constant is from mstflint (BSD-licensed), cited by file and line so a
//! number can be checked rather than trusted:
//!   VSC window        mtcr_ul/mtcr_ul_com.c:762-797, 1399-1590, 1692-1755
//!   address spaces    include/mtcr_ul/mtcr_com_defs.h:376-393
//!   ICMD              mtcr_ul/mtcr_ul_icmd_cif.c:61-129, 494-623, 708-914, 1258-1378
//!   register framing  mtcr_ul/mtcr_ul_com.c:4949-5030, packets_layout.h:57-100
//!   MNVDA / NV header tools_layouts/tools_open_layouts.h:236-286, 394-401
//!   FEC TLV           mlxconfig/mlxconfig_dbs/mlxconfig_host.db (nv_link_phy_conf)
//!   MFRL              reg_access/reg_access_hca_layouts.h:2388-2447

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use uefi::Identify;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::proto::pci::PciIoAddress;
use uefi::proto::pci::root_bridge::PciRootBridgeIo;

const VENDOR_MELLANOX: u16 = 0x15b3;

/// Physical-function ids this code is willing to touch (mtcr_ul_com.c:3263).
/// Virtual functions are deliberately absent: they have no NV configuration.
const KNOWN: &[(u16, &str)] = &[
    (0x1013, "ConnectX-4"),
    (0x1015, "ConnectX-4 Lx"),
    (0x1017, "ConnectX-5"),
    (0x1019, "ConnectX-5 Ex"),
    (0x101b, "ConnectX-6"),
    (0x101d, "ConnectX-6 Dx"),
    (0x101f, "ConnectX-6 Lx"),
    (0x1021, "ConnectX-7"),
];

/// Where firmware says whether it is ready for an ICMD, by hardware id
/// (mtcr_ul_icmd_cif.c:63-68, 1258-1336). The id is CR space 0xf0014 [15:0].
fn probe_addr(hw_id: u16) -> Option<u32> {
    match hw_id {
        0x209 | 0x20b => Some(0xb0004),                 // ConnectX-4, -4 Lx
        0x20d => Some(0xb5e04),                         // ConnectX-5
        0x20f | 0x212 | 0x216 | 0x218 => Some(0xb5f04), // ConnectX-6, -6 Dx, -6 Lx, -7
        _ => None,
    }
}
const HW_ID_ADDR: u32 = 0xf0014;

// --- PCI vendor-specific capability (mtcr_ul_com.c:762-797) -----------------
const PCI_CAP_PTR: u8 = 0x34;
const CAP_ID_VSC: u8 = 0x09;
const VSC_CTRL: u8 = 0x04; //  [15:0] address space, [31:29] status (0 = unsupported)
const VSC_COUNTER: u8 = 0x08;
const VSC_SEMAPHORE: u8 = 0x0c;
const VSC_ADDR: u8 = 0x10; //  [29:0] address, [31] flag
const VSC_DATA: u8 = 0x14;
const VSC_RETRIES: u32 = 2048;

/// Address spaces behind the window (mtcr_com_defs.h:376-393).
const AS_CR_SPACE: u16 = 0x2;
const AS_ICMD: u16 = 0x3;
const AS_SEMAPHORE: u16 = 0xa;

// --- ICMD over the VSC (mtcr_ul_icmd_cif.c:122-129) --------------------------
const ICMD_CTRL: u32 = 0x0; //        AS_ICMD: [0] go, [15:8] status, [31:16] opcode
const ICMD_MAILBOX: u32 = 0x10_0000; // AS_ICMD
const ICMD_MAX_SIZE: u32 = 0x1000; //  AS_ICMD: mailbox capacity in bytes
const ICMD_SYNDROME: u32 = 0x1008; //  AS_ICMD: [23:0]
const ICMD_SEM: u32 = 0x0; //          AS_SEMAPHORE
const ICMD_OP_ACCESS_REG: u16 = 0x9001;
const ICMD_TIMEOUT_MS: u32 = 40_000;

// --- Access-register framing (packets_layout.h, mtcr_ul_com.c:4949) ----------
const OP_TLV_DWORDS: usize = 4; // type 1, len 4, class 1, method, register id, tid
const REG_TLV_DWORDS: usize = 1; // type 3, len
const HDR_DWORDS: usize = OP_TLV_DWORDS + REG_TLV_DWORDS;
const METHOD_GET: u32 = 1;
const METHOD_SET: u32 = 2;

const REG_MNVDA: u16 = 0x9024;
const REG_MFRL: u16 = 0x9028;

/// NV header (tools_open_layouts.h:236-286): three dwords before the data.
const NV_HDR_DWORDS: usize = 3;
const NV_DATA_MAX: usize = 256;
const WRITER_ID_MLXCONFIG: u32 = 0x9;

/// The per-port TLVs that carry the FEC override (mlxconfig_host.db).
const TLV_CLASS_PHYSICAL_PORT: u32 = 1;
const TLV_LINK_PHY_CONF: u32 = 0x0201; // 8 bytes; dword0 [30:28] = phy_fec_override
const TLV_LINK_PHY_CAP: u32 = 0x0202; //  4 bytes; dword0 [31] = phy_fec_override_supported
const FEC_SHIFT: u32 = 28;
const FEC_MASK: u32 = 0x7 << FEC_SHIFT;

/// MFRL reset_trigger = "prepare for warm boot" | "PCIe link toggle", what
/// mlxconfig sends before printing "Please reboot" (mlxcfg_generic_commander.cpp:1085).
const MFRL_WARM_BOOT: u32 = (1 << 6) | (1 << 3);

/// `PHY_FEC_OVERRIDE` in `nv_link_phy_conf`. The names are mlxconfig's; the
/// numbers are the TLV's. `Rs` is what 25GBASE-SR is specified with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fec {
    /// 0 — firmware decides (on these cards, and with no auto-negotiation on
    /// an SR optic, that has meant no FEC).
    DeviceDefault,
    /// 1 — no FEC at 25G/50G/100G.
    Off,
    /// 2 — RS-FEC at 25G/50G/100G.
    Rs,
    /// 3 — FC (BASE-R) FEC at 25G/50G, RS at 100G.
    Fc,
    /// 4 — let auto-negotiation choose.
    AutoNeg,
    Unknown(u8),
}

impl Fec {
    fn from_bits(v: u32) -> Self {
        match v & 0x7 {
            0 => Fec::DeviceDefault,
            1 => Fec::Off,
            2 => Fec::Rs,
            3 => Fec::Fc,
            4 => Fec::AutoNeg,
            n => Fec::Unknown(n as u8),
        }
    }

    fn bits(self) -> u32 {
        match self {
            Fec::DeviceDefault => 0,
            Fec::Off => 1,
            Fec::Rs => 2,
            Fec::Fc => 3,
            Fec::AutoNeg => 4,
            Fec::Unknown(n) => n as u32 & 0x7,
        }
    }

    pub fn name(self) -> String {
        match self {
            Fec::DeviceDefault => String::from("device-default"),
            Fec::Off => String::from("off"),
            Fec::Rs => String::from("rs"),
            Fec::Fc => String::from("fc"),
            Fec::AutoNeg => String::from("autoneg"),
            Fec::Unknown(n) => format!("unknown({n})"),
        }
    }

    /// The config-file and console spelling.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rs" | "rs-fec" | "cl108" | "cl108-rs" | "cl91" => Some(Fec::Rs),
            "fc" | "fc-fec" | "baser" | "base-r" | "cl74" | "cl74-fc" => Some(Fec::Fc),
            "off" | "none" | "no-fec" | "nofec" => Some(Fec::Off),
            "default" | "device-default" => Some(Fec::DeviceDefault),
            "auto" | "autoneg" | "an" => Some(Fec::AutoNeg),
            _ => None,
        }
    }
}

/// What one port reported.
#[derive(Clone, Debug)]
pub struct Port {
    pub port: u8,
    /// `nv_link_phy_cap.phy_fec_override_supported`.
    pub supported: bool,
    /// The value firmware loaded at the last card reset.
    pub current: Fec,
    /// The value it will load at the next one.
    pub next: Fec,
}

/// What happened across the bus.
#[derive(Default, Debug)]
pub struct Summary {
    /// ConnectX cards that were queried.
    pub cards: usize,
    /// Ports whose next-boot value was written this boot and read back as
    /// intended.
    pub written: usize,
    /// Ports whose next-boot value already matched but whose current value
    /// did not — set earlier, not yet applied by a reset.
    pub pending: usize,
    /// Ports that could not be queried or written, with the reason.
    pub failed: usize,
}

impl Summary {
    /// Whether this boot changed something that only a reset will apply.
    pub fn reset_needed(&self) -> bool {
        self.written > 0
    }
}

/// One ConnectX function behind the VSC window.
struct Dev<'a> {
    br: &'a mut PciRootBridgeIo,
    addr: PciIoAddress,
    vsec: u8,
    probe: u32,
}

impl Dev<'_> {
    fn cfg_read(&mut self, reg: u8) -> Result<u32, String> {
        let mut a = self.addr;
        a.reg = reg;
        self.br
            .pci()
            .read_one::<u32>(a)
            .map_err(|e| format!("config read {reg:#04x}: {e:?}"))
    }

    fn cfg_write(&mut self, reg: u8, v: u32) -> Result<(), String> {
        let mut a = self.addr;
        a.reg = reg;
        self.br
            .pci()
            .write_one::<u32>(a, v)
            .map_err(|e| format!("config write {reg:#04x}: {e:?}"))
    }

    // --- the window ---------------------------------------------------------

    /// Take the capability's semaphore: the ticket counter, written back and
    /// read back (mtcr_ul_com.c:1476-1512).
    /// One pass of the ticket handshake: read the semaphore, and if it is free
    /// take it with the next ticket. `Ok(true)` means taken.
    fn vsc_try_take(&mut self) -> Result<bool, String> {
        if self.cfg_read(self.vsec + VSC_SEMAPHORE)? != 0 {
            return Ok(false);
        }
        let ticket = self.cfg_read(self.vsec + VSC_COUNTER)?;
        self.cfg_write(self.vsec + VSC_SEMAPHORE, ticket)?;
        Ok(self.cfg_read(self.vsec + VSC_SEMAPHORE)? == ticket)
    }

    fn vsc_lock(&mut self) -> Result<(), String> {
        // First, the cooperative path: wait for whoever holds it to release.
        let mut last = 0u32;
        for _ in 0..VSC_RETRIES {
            if self.vsc_try_take()? {
                return Ok(());
            }
            last = self.cfg_read(self.vsec + VSC_SEMAPHORE)?;
            boot::stall(Duration::from_millis(1));
        }
        // It never released. On this platform that is the Mellanox UEFI driver,
        // which takes cap9 at init and *parks* it for its whole lifetime to keep
        // tools out — the held value is a small init-time ticket (0x3, 0x7), not
        // a value that moves. The driver does not use the address/data window
        // itself; its own path is the HCA command queue on the main BAR. So a
        // parked lock is safe to clear and retake for the window's duration.
        // 0xffffffff would instead mean the read never reached the card, and
        // clearing it would be pointless — so only force a plausible parked
        // ticket, never a dead bus.
        if last == 0 || last == 0xffff_ffff {
            return Err(format!(
                "VSC semaphore unreadable or never held (last {last:#010x}, vsec @ {:#04x})",
                self.vsec
            ));
        }
        uefi::println!(
            "    (cap9 parked at {last:#x} by the UEFI driver — clearing and taking the window)"
        );
        self.cfg_write(self.vsec + VSC_SEMAPHORE, 0)?;
        boot::stall(Duration::from_millis(1));
        for _ in 0..VSC_RETRIES {
            if self.vsc_try_take()? {
                return Ok(());
            }
            boot::stall(Duration::from_millis(1));
        }
        Err(format!(
            "VSC semaphore would not take even after clearing a parked lock (last {last:#010x}, vsec @ {:#04x})",
            self.vsec
        ))
    }

    fn vsc_unlock(&mut self) {
        let _ = self.cfg_write(self.vsec + VSC_SEMAPHORE, 0);
    }

    /// Select an address space and confirm the card supports it
    /// (mtcr_ul_com.c:1557-1590).
    fn vsc_space(&mut self, space: u16) -> Result<(), String> {
        let v = (self.cfg_read(self.vsec + VSC_CTRL)? & !0xffff) | space as u32;
        self.cfg_write(self.vsec + VSC_CTRL, v)?;
        let rd = self.cfg_read(self.vsec + VSC_CTRL)?;
        if rd & 0xffff != space as u32 || (rd >> 29) & 0x7 == 0 {
            return Err(format!("address space {space:#x} is not supported by this card"));
        }
        Ok(())
    }

    /// Poll the flag bit of the address register (mtcr_ul_com.c:1514-1535).
    fn vsc_wait_flag(&mut self, expect: u32) -> Result<(), String> {
        for i in 0..VSC_RETRIES {
            if (self.cfg_read(self.vsec + VSC_ADDR)? >> 31) == expect {
                return Ok(());
            }
            if i % 16 == 15 {
                boot::stall(Duration::from_millis(1));
            }
        }
        Err(String::from("VSC window did not complete the access"))
    }

    /// One dword through the window, holding the semaphore for its duration
    /// (mtcr_ul_com.c:1692-1755). Flag 1 = write, 0 = read.
    fn vsc_rw(&mut self, space: u16, offset: u32, write: Option<u32>) -> Result<u32, String> {
        if offset >> 30 != 0 {
            return Err(format!("address {offset:#x} is outside the 30-bit window"));
        }
        self.vsc_lock()?;
        let r = self.vsc_rw_locked(space, offset, write);
        self.vsc_unlock();
        r
    }

    fn vsc_rw_locked(&mut self, space: u16, offset: u32, write: Option<u32>) -> Result<u32, String> {
        self.vsc_space(space)?;
        match write {
            Some(d) => {
                self.cfg_write(self.vsec + VSC_DATA, d)?;
                self.cfg_write(self.vsec + VSC_ADDR, offset | (1 << 31))?;
                self.vsc_wait_flag(0)?;
                Ok(d)
            }
            None => {
                self.cfg_write(self.vsec + VSC_ADDR, offset)?;
                self.vsc_wait_flag(1)?;
                self.cfg_read(self.vsec + VSC_DATA)
            }
        }
    }

    fn read4(&mut self, space: u16, offset: u32) -> Result<u32, String> {
        self.vsc_rw(space, offset, None)
    }

    fn write4(&mut self, space: u16, offset: u32, v: u32) -> Result<(), String> {
        self.vsc_rw(space, offset, Some(v)).map(|_| ())
    }

    // --- ICMD ---------------------------------------------------------------

    /// The firmware's own ICMD semaphore, distinct from the window's
    /// (mtcr_ul_icmd_cif.c:708-798). Any nonzero token; mstflint uses its pid.
    fn icmd_take(&mut self) -> Result<(), String> {
        const TOKEN: u32 = 0x5b00_7fec;
        for _ in 0..256 {
            self.write4(AS_SEMAPHORE, ICMD_SEM, TOKEN)?;
            if self.read4(AS_SEMAPHORE, ICMD_SEM)? == TOKEN {
                return Ok(());
            }
            boot::stall(Duration::from_millis(10));
        }
        Err(String::from("firmware ICMD semaphore is held by someone else"))
    }

    fn icmd_release(&mut self) {
        let _ = self.write4(AS_SEMAPHORE, ICMD_SEM, 0);
    }

    /// Run one ICMD: `buf[..w]` goes into the mailbox, `buf[..r]` comes back
    /// (mtcr_ul_icmd_cif.c:814-914).
    fn icmd(&mut self, opcode: u16, buf: &mut [u32], w: usize, r: usize) -> Result<(), String> {
        let cap = self.read4(AS_ICMD, ICMD_MAX_SIZE)? as usize;
        if w * 4 > cap || r * 4 > cap {
            return Err(format!("mailbox holds {cap} bytes; command needs {}", w.max(r) * 4));
        }
        if self.read4(AS_CR_SPACE, self.probe)? >> 31 != 0 {
            return Err(String::from("firmware is not ready for commands"));
        }
        self.icmd_take()?;
        let res = self.icmd_locked(opcode, buf, w, r);
        self.icmd_release();
        res
    }

    fn icmd_locked(&mut self, opcode: u16, buf: &mut [u32], w: usize, r: usize) -> Result<(), String> {
        let mut ctrl = self.read4(AS_ICMD, ICMD_CTRL)?;
        ctrl = (ctrl & 0x0000_ffff) | ((opcode as u32) << 16);
        ctrl &= !(1 << 1); // never the extended mailbox
        self.write4(AS_ICMD, ICMD_CTRL, ctrl)?;
        for (i, d) in buf.iter().take(w).enumerate() {
            self.write4(AS_ICMD, ICMD_MAILBOX + 4 * i as u32, *d)?;
        }
        ctrl = self.read4(AS_ICMD, ICMD_CTRL)?;
        if ctrl & 1 != 0 {
            return Err(String::from("firmware reports a command already in flight"));
        }
        self.write4(AS_ICMD, ICMD_CTRL, ctrl | 1)?;

        // Poll the go bit: tight at first, then a millisecond at a time.
        let mut elapsed_ms = 0u32;
        let mut i = 0u32;
        loop {
            ctrl = self.read4(AS_ICMD, ICMD_CTRL)?;
            if ctrl & 1 == 0 {
                break;
            }
            if i < 64 {
                boot::stall(Duration::from_micros(100));
            } else {
                boot::stall(Duration::from_millis(1));
                elapsed_ms += 1;
            }
            i += 1;
            if elapsed_ms > ICMD_TIMEOUT_MS {
                return Err(String::from("command did not complete in 40 s"));
            }
        }
        let status = (ctrl >> 8) & 0xff;
        if status != 0 {
            let syndrome = self.read4(AS_ICMD, ICMD_SYNDROME).unwrap_or(0) & 0x00ff_ffff;
            return Err(format!(
                "ICMD status {status} ({}), syndrome {syndrome:#x}",
                icmd_status(status)
            ));
        }
        for i in 0..r {
            buf[i] = self.read4(AS_ICMD, ICMD_MAILBOX + 4 * i as u32)?;
        }
        Ok(())
    }

    // --- access register ----------------------------------------------------

    /// GET or SET one register. `reg` is the register image in dwords, sized
    /// to `reg_size` bytes; `w_reg`/`r_reg` are how many of those dwords go
    /// out and come back (mtcr_ul_com.c:4949-5030).
    fn access_reg(
        &mut self,
        id: u16,
        method: u32,
        reg: &mut [u32],
        reg_size: usize,
        w_reg: usize,
        r_reg: usize,
    ) -> Result<(), String> {
        let mut buf = vec![0u32; HDR_DWORDS + reg.len()];
        // OperationTlv dword 0 (packets_layout.h): status[8:15) dr[15]
        // len[16:27) Type[27:32). Type 1 (operation), length 4 dwords.
        buf[0] = (1 << 27) | (4 << 16);
        // OperationTlv dword 1: tlv_class[0:8) method[8:15) r[15]
        // register_id[16:32). class 1 = register access, r 0. The earlier form
        // mirrored all three fields (class at 24, method at 17, id at 0), which
        // the firmware rejected as ICMD status 4 (bad parameter).
        buf[1] = ((id as u32) << 16) | (method << 8) | 1;
        buf[2] = 0;
        buf[3] = 0;
        // reg_tlv: type 3, length in dwords including itself.
        buf[4] = (3 << 27) | ((((reg_size + 4) >> 2) as u32) << 16);
        buf[HDR_DWORDS..].copy_from_slice(reg);

        self.icmd(ICMD_OP_ACCESS_REG, &mut buf, HDR_DWORDS + w_reg, HDR_DWORDS + r_reg)?;

        let status = (buf[0] >> 8) & 0x7f;
        if status != 0 {
            return Err(format!("register {id:#06x}: {}", reg_status(status)));
        }
        reg.copy_from_slice(&buf[HDR_DWORDS..]);
        Ok(())
    }

    /// Read one NV TLV. `None` when the card says the TLV does not exist.
    fn nv_get(&mut self, tlv_type: u32, current: bool) -> Result<Option<Vec<u32>>, String> {
        let mut reg = vec![0u32; NV_HDR_DWORDS + NV_DATA_MAX / 4];
        reg[0] = NV_DATA_MAX as u32
            | (WRITER_ID_MLXCONFIG << 16)
            | ((current as u32) << 22)
            | (1 << 25); // over_en, as mlxconfig sets it
        reg[1] = tlv_type;
        let reg_size = NV_DATA_MAX + NV_HDR_DWORDS * 4;
        let reg_len = reg.len();
        match self.access_reg(REG_MNVDA, METHOD_GET, &mut reg, reg_size, NV_HDR_DWORDS, reg_len) {
            Ok(()) => {}
            Err(e) if e.contains("resource not available") => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = (reg[0] & 0x1ff) as usize;
        reg.truncate(NV_HDR_DWORDS + len.div_ceil(4));
        Ok(Some(reg))
    }

    /// Write one NV TLV's data as its next-boot value.
    fn nv_set(&mut self, tlv_type: u32, data: &[u32]) -> Result<(), String> {
        let len = data.len() * 4;
        let mut reg = vec![0u32; NV_HDR_DWORDS + data.len()];
        reg[0] = len as u32 | (WRITER_ID_MLXCONFIG << 16) | (1 << 25);
        reg[1] = tlv_type;
        reg[NV_HDR_DWORDS..].copy_from_slice(data);
        let reg_size = len + NV_HDR_DWORDS * 4;
        let reg_len = reg.len();
        self.access_reg(REG_MNVDA, METHOD_SET, &mut reg, reg_size, reg_len, NV_HDR_DWORDS)
    }

    /// Ask firmware to prepare for the warm boot that will apply NV config.
    fn mfrl_warm_boot(&mut self) -> Result<(), String> {
        let mut reg = [0u32, MFRL_WARM_BOOT];
        self.access_reg(REG_MFRL, METHOD_SET, &mut reg, 8, 2, 2)
    }

    // --- the FEC TLVs -------------------------------------------------------

    fn port_tlv(port: u8, idx: u32) -> u32 {
        (TLV_CLASS_PHYSICAL_PORT << 24) | ((port as u32) << 16) | idx
    }

    /// Query one port; `None` when the card has no such port.
    fn port(&mut self, port: u8) -> Result<Option<Port>, String> {
        let Some(cap) = self.nv_get(Self::port_tlv(port, TLV_LINK_PHY_CAP), false)? else {
            return Ok(None);
        };
        let supported = cap.get(NV_HDR_DWORDS).is_some_and(|d| d >> 31 == 1);
        let next = self.nv_get(Self::port_tlv(port, TLV_LINK_PHY_CONF), false)?;
        let current = self.nv_get(Self::port_tlv(port, TLV_LINK_PHY_CONF), true)?;
        let fec = |t: &Option<Vec<u32>>| {
            t.as_ref()
                .and_then(|r| r.get(NV_HDR_DWORDS))
                .map(|d| Fec::from_bits(d >> FEC_SHIFT))
                .unwrap_or(Fec::DeviceDefault)
        };
        Ok(Some(Port {
            port,
            supported,
            current: fec(&current),
            next: fec(&next),
        }))
    }

    /// Write `want` into the port's next-boot conf, preserving every other
    /// field, and read it back.
    fn set_port(&mut self, port: u8, want: Fec) -> Result<Fec, String> {
        let t = Self::port_tlv(port, TLV_LINK_PHY_CONF);
        // Firmware wants the whole TLV: read, patch, write — as mlxconfig does.
        let cur = self.nv_get(t, false)?;
        let mut data: Vec<u32> = match cur {
            Some(r) if r.len() > NV_HDR_DWORDS => r[NV_HDR_DWORDS..].to_vec(),
            _ => vec![0u32; 2],
        };
        if data.len() < 2 {
            data.resize(2, 0);
        }
        data[0] = (data[0] & !FEC_MASK) | (want.bits() << FEC_SHIFT);
        self.nv_set(t, &data)?;
        let back = self.nv_get(t, false)?;
        Ok(back
            .and_then(|r| r.get(NV_HDR_DWORDS).copied())
            .map(|d| Fec::from_bits(d >> FEC_SHIFT))
            .unwrap_or(Fec::Unknown(0xff)))
    }
}

fn icmd_status(s: u32) -> &'static str {
    match s {
        1 => "invalid opcode",
        2 => "invalid command",
        3 => "operational error",
        4 => "bad parameter",
        5 => "busy",
        6 => "ICM not available",
        7 => "write protected",
        _ => "unknown",
    }
}

fn reg_status(s: u32) -> &'static str {
    match s {
        1 => "device busy",
        2 => "version not supported",
        3 => "unknown TLV",
        4 => "register not supported",
        5 => "class not supported",
        6 => "method not supported",
        7 => "bad parameter",
        8 => "resource not available",
        9 => "message receipt ack",
        0x20 => "bad configuration",
        0x22 => "configuration corrupt",
        0x24 => "length too small",
        0x70 => "internal error",
        _ => "unknown status",
    }
}

/// Find the capability with id 9 and functional type 0 (mtcr_ul_com.c:1399-1474, 2344).
fn find_vsc(br: &mut PciRootBridgeIo, addr: PciIoAddress) -> Result<u8, String> {
    let rd = |br: &mut PciRootBridgeIo, reg: u8| -> Result<u32, String> {
        let mut a = addr;
        a.reg = reg;
        br.pci()
            .read_one::<u32>(a)
            .map_err(|e| format!("config read {reg:#04x}: {e:?}"))
    };
    // Status register bit 4: a capability list exists at all.
    if (rd(br, 0x04)? >> 16) & (1 << 4) == 0 {
        return Err(String::from("no PCI capability list"));
    }
    let mut off = (rd(br, PCI_CAP_PTR)? & 0xfc) as u8;
    let mut hops = 0;
    while off >= 0x40 && hops < 48 {
        let hdr = rd(br, off)?;
        let id = (hdr & 0xff) as u8;
        let next = ((hdr >> 8) & 0xfc) as u8;
        if id == CAP_ID_VSC {
            let kind = (hdr >> 24) & 0xff;
            if kind != 0 {
                return Err(format!("VSC is type {kind}, not the functional one (card in recovery?)"));
            }
            return Ok(off);
        }
        off = next;
        hops += 1;
    }
    Err(String::from("no vendor-specific capability"))
}

/// Walk the bus. Report every ConnectX port's FEC; when `want` is given, set
/// any port whose next-boot value differs. Prints as it goes, since a console
/// line per card is the only record a firmware boot leaves.
pub fn apply(want: Option<Fec>) -> Summary {
    let mut sum = Summary::default();
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&PciRootBridgeIo::GUID))
    else {
        uefi::println!("  no PCI root bridge — nothing to look at");
        return sum;
    };

    for h in handles.iter() {
        // GetProtocol, never exclusive. An exclusive open of the PCI root
        // bridge makes UEFI DisconnectController every BY_DRIVER agent under it
        // — PciBusDxe and, beneath it, the NIC drivers — so an exclusive open
        // here tears down the very network stack the next step needs, and often
        // fails outright when PciBusDxe will not detach mid-boot (leaving the
        // scan empty and the NICs dead). Borrow the interface; disturb nothing.
        let params = OpenProtocolParams {
            handle: *h,
            agent: boot::image_handle(),
            controller: None,
        };
        let Ok(mut bridge) = (unsafe {
            boot::open_protocol::<PciRootBridgeIo>(params, OpenProtocolAttributes::GetProtocol)
        }) else {
            continue;
        };
        for (addr, device) in mellanox_functions(&mut bridge) {
            let loc = format!("{:02x}:{:02x}.{}", addr.bus, addr.dev, addr.fun);
            let Some((_, model)) = KNOWN.iter().find(|(id, _)| *id == device) else {
                uefi::println!("  {loc}  15b3:{device:04x}  not a ConnectX physical function this code knows — skipped");
                continue;
            };
            sum.cards += 1;
            match one_card(&mut bridge, addr, model, &loc, want, &mut sum) {
                Ok(()) => {}
                Err(e) => {
                    sum.failed += 1;
                    uefi::println!("  {loc}  {model}: {e} — left as is");
                }
            }
        }
    }
    if sum.cards == 0 {
        uefi::println!("  no ConnectX on the bus");
    }
    sum
}

fn one_card(
    bridge: &mut PciRootBridgeIo,
    addr: PciIoAddress,
    model: &str,
    loc: &str,
    want: Option<Fec>,
    sum: &mut Summary,
) -> Result<(), String> {
    let vsec = find_vsc(bridge, addr)?;
    let mut dev = Dev { br: bridge, addr, vsec, probe: 0 };
    // Confirm the spaces before anything else touches the card.
    dev.vsc_lock()?;
    let spaces = [AS_CR_SPACE, AS_ICMD, AS_SEMAPHORE]
        .iter()
        .try_for_each(|s| dev.vsc_space(*s));
    dev.vsc_unlock();
    spaces?;

    let hw = dev.read4(AS_CR_SPACE, HW_ID_ADDR)?;
    let hw_id = (hw & 0xffff) as u16;
    let Some(probe) = probe_addr(hw_id) else {
        return Err(format!("hardware id {hw_id:#x} is not a generation this code knows"));
    };
    if hw_id == addr_device(dev.br, addr) {
        // Livefish: the card is running its recovery firmware and identifies
        // as its bare hardware id. Nothing here should be written to it.
        return Err(String::from("card is in livefish/recovery mode"));
    }
    dev.probe = probe;

    let mut port_no = 1u8;
    while port_no <= 2 {
        let Some(p) = dev.port(port_no)? else { break };
        let mut line = format!(
            "  {loc}  {model} port {}: current {}, next boot {}",
            p.port,
            p.current.name(),
            p.next.name()
        );
        match want {
            None => {}
            Some(_) if !p.supported => line.push_str(" — FEC override not supported on this port"),
            Some(w) if p.next == w => {
                if p.current != w {
                    sum.pending += 1;
                    line.push_str(" — set earlier, waiting for a card reset to apply");
                }
            }
            Some(w) => match dev.set_port(p.port, w) {
                Ok(back) if back == w => {
                    sum.written += 1;
                    line.push_str(&format!(" — wrote {}", w.name()));
                }
                Ok(back) => {
                    sum.failed += 1;
                    line.push_str(&format!(" — wrote {} but read back {}", w.name(), back.name()));
                }
                Err(e) => {
                    sum.failed += 1;
                    line.push_str(&format!(" — write failed: {e}"));
                }
            },
        }
        uefi::println!("{line}");
        port_no += 1;
    }

    if sum.written > 0 {
        // mlxconfig sends this before asking for the reboot; a firmware that
        // does not take it still applies NV config on the reset that follows.
        if let Err(e) = dev.mfrl_warm_boot() {
            uefi::println!("  {loc}  MFRL warm-boot request refused: {e} (the reset still applies the change)");
        }
    }
    Ok(())
}

fn addr_device(br: &mut PciRootBridgeIo, addr: PciIoAddress) -> u16 {
    let mut a = addr;
    a.reg = 0;
    br.pci().read_one::<u32>(a).map(|v| (v >> 16) as u16).unwrap_or(0)
}

/// Every Mellanox function on this root bridge, as (address, device id).
///
/// The same discovered walk as the shell's `pci`: start at bus 0, follow
/// bridges, never renumber anything.
fn mellanox_functions(br: &mut PciRootBridgeIo) -> Vec<(PciIoAddress, u16)> {
    let mut out = Vec::new();
    let mut buses: Vec<u8> = vec![0];
    let mut seen: Vec<u8> = Vec::new();
    while let Some(bus) = buses.pop() {
        if seen.contains(&bus) {
            continue;
        }
        seen.push(bus);
        for dev in 0..32u8 {
            for fun in 0..8u8 {
                let mut addr = PciIoAddress::new(bus, dev, fun);
                addr.reg = 0;
                let Ok(id) = br.pci().read_one::<u32>(addr) else { continue };
                let vendor = (id & 0xffff) as u16;
                if vendor == 0xffff || vendor == 0 {
                    if fun == 0 {
                        break;
                    }
                    continue;
                }
                addr.reg = 0x0c;
                let header = br
                    .pci()
                    .read_one::<u32>(addr)
                    .map(|v| ((v >> 16) & 0xff) as u8)
                    .unwrap_or(0);
                if (header & 0x7f) == 0x01 {
                    addr.reg = 0x18;
                    if let Ok(v) = br.pci().read_one::<u32>(addr) {
                        let secondary = ((v >> 8) & 0xff) as u8;
                        if secondary != 0 {
                            buses.push(secondary);
                        }
                    }
                }
                if vendor == VENDOR_MELLANOX {
                    addr.reg = 0;
                    out.push((addr, (id >> 16) as u16));
                }
                if fun == 0 && (header & 0x80) == 0 {
                    break;
                }
            }
        }
    }
    out
}
