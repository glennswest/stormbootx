//! NVMe/TCP initiator for boot loaders: synchronous, one command in flight,
//! over any byte stream (#10).
//!
//! Extracted from stormbootx's `src/nvme.rs`, itself ported from
//! sbregistry's host `src/nvme.rs`, which is validated against real hardware,
//! rather than rebuilt from the specification — the wire format is the part
//! most likely to be subtly wrong, and the host implementation has already
//! paid for those lessons. Two of them are load-bearing and both are
//! preserved here with their reasoning:
//!
//!   * **PSDT = 01b on every command.** There are no PRPs over a fabric.
//!     Leaving the FLAGS byte zero says "PRPs are used", and a controller that
//!     validates it rejects the command with Invalid Field before it looks at
//!     the SGL. stormblockmk does not check; the Linux target does.
//!   * **The controller must be enabled before admin commands.** Fabrics
//!     Connect only establishes a queue. Identify before CC.EN is answered
//!     with Command Sequence Error on a conforming target.
//!
//! Differences from the host version, both forced by where this runs: it is
//! synchronous (firmware has no executor and the caller is a block driver that
//! wants blocking reads), and there is no write pipelining — a boot path reads
//! far more than it writes, and one command in flight keeps this small.
//!
//! **Where the bytes go is the caller's.** A [`Transport`] is a connected byte
//! stream: stormbootx's is its smoltcp socket on a UEFI NIC's SNP, and
//! stormboot4bios's will be smoltcp over a NIC option ROM's UNDI. A
//! [`Platform`] opens one per queue (attach opens two: admin, then I/O) and
//! waits while the controller comes ready. Nothing here names a firmware, an
//! allocator beyond `alloc`, or a clock: one initiator for both loaders, so
//! there is no third copy to drift (the issue that made this a crate).
//!
//! `no_std` + `alloc`, no dependencies. `cargo test -p nvme-tcp-initiator`
//! runs it on the host against an in-memory controller that refuses a
//! command without PSDT=01b and an admin command before CC.EN.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// A connected byte stream to the portal: one per queue.
pub trait Transport {
    type Error: core::fmt::Display;
    /// Send every byte of `buf`, or fail.
    fn send_all(&mut self, buf: &[u8]) -> Result<(), Self::Error>;
    /// Fill `buf` exactly, or fail. Every NVMe/TCP header and payload length
    /// is known before it is read, so this is the primitive the layer wants.
    fn recv_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error>;
    /// The MTU of the path, if the stack will say. Printed, never used to
    /// size anything (see `transfer_limit`).
    fn link_mtu(&self) -> Option<u32> {
        None
    }
}

/// What attaching needs from where it runs, besides the bytes.
pub trait Platform {
    type Transport: Transport;
    /// A new connection to the portal.
    fn connect(&mut self, addr: [u8; 4], port: u16) -> Result<Self::Transport, String>;
    /// Wait `ms` milliseconds (the controller coming ready after CC.EN).
    fn stall_ms(&mut self, ms: u32);
}

// PDU types.
const PDU_ICREQ: u8 = 0x00;
const PDU_ICRESP: u8 = 0x01;
const PDU_H2C_TERM: u8 = 0x02;
const PDU_C2H_TERM: u8 = 0x03;
const PDU_CMD: u8 = 0x04;
const PDU_RESP: u8 = 0x05;
const PDU_H2C_DATA: u8 = 0x06;
const PDU_C2H_DATA: u8 = 0x07;
const PDU_R2T: u8 = 0x09;

// NVMe opcodes.
const OPC_WRITE: u8 = 0x01;
const OPC_READ: u8 = 0x02;
const OPC_IDENTIFY: u8 = 0x06;
const OPC_FABRICS: u8 = 0x7F;
const FCTYPE_CONNECT: u8 = 0x01;
const FCTYPE_PROPERTY_SET: u8 = 0x00;
const FCTYPE_PROPERTY_GET: u8 = 0x04;

// Controller registers, reached over a fabric as properties.
const REG_CAP: u32 = 0x00;
const REG_CC: u32 = 0x14;
const REG_CSTS: u32 = 0x1C;

/// CC with EN=1, NVM command set, 4 KiB pages, round-robin, and the queue
/// entry sizes every controller uses: 2^6 submission, 2^4 completion.
const CC_ENABLE: u64 = (6 << 16) | (4 << 20) | 1;

/// SGL descriptor byte for data the transport moves (R2T/H2CData, C2HData).
const SGL_TRANSPORT: u8 = 0x5A;
/// SGL descriptor byte for data carried inside the capsule itself.
const SGL_IN_CAPSULE: u8 = 0x01;
/// FLAGS byte with PSDT = 01b — SGLs, the only choice over a fabric.
const FLAGS_SGL: u8 = 0x40;

/// Bytes moved per NVMe command, taken from the controller's own limit.
///
/// This was derived from the path MTU, sized so one reply landed in one frame.
/// That inverted on exactly the networks worth having: with the 104 bytes of
/// IP, TCP and PDU header budgeted, a 9000 path rounded down to **8 KiB** a
/// command while a 1500 path took the 64 KiB fallback — eight times less data
/// per round trip on the faster network, which is the opposite of the point.
///
/// The frame argument does not survive contact with TCP. NVMe/TCP rides a byte
/// stream: the stack segments it to the MSS and IP never fragments it, so a
/// 64 KiB PDU on a 9000 path is eight segments, not a reassembly problem. What
/// the size actually buys is round trips. `read` issues one command at a time
/// and waits for it, so throughput is transfer ÷ RTT and nothing else — under
/// that rule bigger is strictly better, up to what the controller will accept.
///
/// So ask the controller. **MDTS** is the maximum data transfer size, in units
/// of the minimum page size the controller advertises in `CAP.MPSMIN`, and
/// zero means it imposes no limit of its own. That is a property of the device
/// on the other end rather than a number matched by hand to a network this
/// binary cannot see — which was the whole complaint that retired the original
/// constant.
///
/// The MTU is still read, and still printed, because "the path is 9000" is
/// worth knowing on a console when a boot is slow. It no longer decides
/// anything.
fn transfer_limit(mdts: u8, mpsmin: u32, block_size: u32) -> usize {
    /// What to use when the controller declines to say. Every controller
    /// accepts it, and it is what this client used before MDTS was read.
    const UNSTATED: usize = 64 * 1024;
    /// A ceiling of our own. One command is one stall with no other command
    /// outstanding, and a target that advertises a very large MDTS is offering
    /// more than a boot path has any use for.
    const CEILING: usize = 512 * 1024;

    let page = 1usize << (12 + mpsmin.min(12));
    let limit = if mdts == 0 {
        UNSTATED
    } else {
        page.saturating_mul(1usize << mdts.min(20)).min(CEILING)
    };

    // Down to a whole number of blocks. CDW12 carries NLB as a 0-based count,
    // so a transfer shorter than one block would compute `blocks - 1` on zero
    // and wrap — reachable on a namespace reporting a block size larger than
    // the limit, which the format permits up to 64 KiB.
    let bs = block_size.max(1) as usize;
    (limit / bs * bs).max(bs)
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn le64(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

/// One NVMe submission queue entry, built field by field: the layout is what
/// the wire cares about.
#[derive(Clone)]
struct Sqe([u8; 64]);

impl Sqe {
    fn new(opcode: u8, cid: u16, nsid: u32) -> Self {
        let mut s = [0u8; 64];
        s[0] = opcode;
        s[1] = FLAGS_SGL; // PSDT=01b — required on every fabrics command
        s[2..4].copy_from_slice(&cid.to_le_bytes());
        s[4..8].copy_from_slice(&nsid.to_le_bytes());
        Sqe(s)
    }
    /// Bytes 24..40 are the data pointer; as an SGL that is
    /// address(8) | length(4) | reserved(3) | type(1).
    fn sgl(&mut self, addr: u64, len: u32, kind: u8) -> &mut Self {
        self.0[24..32].copy_from_slice(&addr.to_le_bytes());
        self.0[32..36].copy_from_slice(&len.to_le_bytes());
        self.0[36..39].fill(0);
        self.0[39] = kind;
        self
    }
    fn dw(&mut self, n: usize, v: u32) -> &mut Self {
        let at = 40 + (n - 10) * 4;
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
        self
    }
    fn byte(&mut self, at: usize, v: u8) -> &mut Self {
        self.0[at] = v;
        self
    }
}

#[derive(Debug, Clone, Copy)]
struct Cqe {
    dw0: u32,
    dw1: u32,
    status: u16,
}

impl Cqe {
    fn parse(b: &[u8]) -> Self {
        Cqe {
            dw0: le32(b, 0),
            dw1: le32(b, 4),
            status: le16(b, 14),
        }
    }
    /// Bit 0 is the phase tag; status code and type sit above it.
    fn failed(&self) -> Option<String> {
        let sc = (self.status >> 1) & 0xFF;
        let sct = (self.status >> 9) & 0x7;
        (sc != 0 || sct != 0).then(|| format!("NVMe status sct={sct:#x} sc={sc:#04x}"))
    }
}

/// One NVMe/TCP queue: a connection that has completed ICReq and Connect.
pub struct Queue<T: Transport> {
    sock: T,
    cid: u16,
    maxh2cdata: u32,
}

impl<T: Transport> Queue<T> {
    fn open(sock: T) -> Result<Self, String> {
        let mut q = Queue {
            sock,
            cid: 0,
            maxh2cdata: 8192,
        };
        q.ic_handshake()?;
        Ok(q)
    }

    fn send(&mut self, buf: &[u8]) -> Result<(), String> {
        self.sock.send_all(buf).map_err(|e| e.to_string())
    }

    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut out = vec![0u8; n];
        self.sock.recv_exact(&mut out).map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// Negotiate the connection. Digests are declined, which keeps every later
    /// PDU free of trailing digest fields.
    fn ic_handshake(&mut self) -> Result<(), String> {
        let mut pdu = vec![0u8; 128];
        pdu[0] = PDU_ICREQ;
        pdu[2] = 128; // hlen
        pdu[4..8].copy_from_slice(&128u32.to_le_bytes()); // plen
        pdu[8..10].copy_from_slice(&0u16.to_le_bytes()); // pfv
        pdu[10] = 0; // hpda
        pdu[11] = 0; // no header or data digest
        pdu[12..16].copy_from_slice(&0u32.to_le_bytes()); // maxr2t = 1
        self.send(&pdu)?;

        let resp = self.read_exact(128)?;
        if resp[0] != PDU_ICRESP {
            return Err(format!(
                "expected ICResp, got PDU type {:#04x} (is this an NVMe/TCP port?)",
                resp[0]
            ));
        }
        if resp[11] & 0x3 != 0 {
            return Err("controller insists on PDU digests, which this client declined".into());
        }
        let maxh2cdata = le32(&resp, 12);
        if maxh2cdata >= 4096 {
            self.maxh2cdata = maxh2cdata;
        }
        Ok(())
    }

    /// The MTU of the path this queue runs over, if the stack will say.
    fn link_mtu(&self) -> Option<u32> {
        self.sock.link_mtu()
    }

    fn next_cid(&mut self) -> u16 {
        self.cid = self.cid.wrapping_add(1);
        self.cid
    }

    fn send_cmd(&mut self, sqe: &Sqe, in_capsule: Option<&[u8]>) -> Result<(), String> {
        let data = in_capsule.unwrap_or(&[]);
        let hlen = 72u8; // 8 common header + 64 SQE
        let plen = hlen as u32 + data.len() as u32;
        let mut pdu = Vec::with_capacity(plen as usize);
        pdu.push(PDU_CMD);
        pdu.push(0); // flags
        pdu.push(hlen);
        pdu.push(if data.is_empty() { 0 } else { hlen }); // pdo
        pdu.extend_from_slice(&plen.to_le_bytes());
        pdu.extend_from_slice(&sqe.0);
        pdu.extend_from_slice(data);
        self.send(&pdu)
    }

    fn send_h2c_data(
        &mut self,
        cid: u16,
        ttag: u16,
        offset: u32,
        data: &[u8],
        last: bool,
    ) -> Result<(), String> {
        let hlen: u8 = 24;
        let plen = hlen as u32 + data.len() as u32;
        let mut pdu = Vec::with_capacity(plen as usize);
        pdu.push(PDU_H2C_DATA);
        // Bit 2 is LAST_PDU in a data PDU; bits 0 and 1 are the digest flags.
        pdu.push(if last { 0x4 } else { 0 });
        pdu.push(hlen);
        pdu.push(hlen); // pdo
        pdu.extend_from_slice(&plen.to_le_bytes());
        pdu.extend_from_slice(&cid.to_le_bytes());
        pdu.extend_from_slice(&ttag.to_le_bytes());
        pdu.extend_from_slice(&offset.to_le_bytes());
        pdu.extend_from_slice(&(data.len() as u32).to_le_bytes());
        pdu.extend_from_slice(&0u32.to_le_bytes()); // reserved
        pdu.extend_from_slice(data);
        self.send(&pdu)
    }

    /// Wait for `cid`, servicing whatever PDUs arrive on the way.
    fn complete(
        &mut self,
        cid: u16,
        write_data: Option<&[u8]>,
        mut read_into: Option<&mut [u8]>,
    ) -> Result<Cqe, String> {
        // Bounded so a controller that goes silent cannot wedge the boot.
        for _ in 0..4096 {
            let ch = self.read_exact(8)?;
            let ptype = ch[0];
            let flags = ch[1];
            let hlen = ch[2] as usize;
            let pdo = ch[3] as usize;
            let plen = le32(&ch, 4) as usize;
            if plen < 8 || hlen < 8 {
                return Err(format!(
                    "malformed PDU (type {ptype:#04x}, hlen {hlen}, plen {plen})"
                ));
            }
            let rest = self.read_exact(plen - 8)?;

            match ptype {
                PDU_RESP => {
                    let cqe = Cqe::parse(&rest[..16]);
                    if le16(&rest, 12) != cid {
                        continue; // not ours; nothing else is in flight here
                    }
                    return match cqe.failed() {
                        Some(msg) => Err(msg),
                        None => Ok(cqe),
                    };
                }
                PDU_R2T => {
                    let cccid = le16(&rest, 0);
                    let ttag = le16(&rest, 2);
                    let offset = le32(&rest, 4) as usize;
                    let length = le32(&rest, 8) as usize;
                    let data = write_data
                        .ok_or("controller asked for data on a command carrying none")?;
                    if offset + length > data.len() {
                        return Err(format!(
                            "controller asked for bytes {offset}..{} of a {}-byte transfer",
                            offset + length,
                            data.len()
                        ));
                    }
                    let mut sent = 0usize;
                    while sent < length {
                        let chunk = (length - sent).min(self.maxh2cdata as usize);
                        let last = sent + chunk == length;
                        self.send_h2c_data(
                            cccid,
                            ttag,
                            (offset + sent) as u32,
                            &data[offset + sent..offset + sent + chunk],
                            last,
                        )?;
                        sent += chunk;
                    }
                }
                PDU_C2H_DATA => {
                    let offset = le32(&rest, 4) as usize;
                    let length = le32(&rest, 8) as usize;
                    let payload_at = pdo.saturating_sub(8);
                    if payload_at + length > rest.len() {
                        return Err("C2HData payload runs past the PDU".into());
                    }
                    let payload = &rest[payload_at..payload_at + length];
                    let buf = read_into
                        .as_deref_mut()
                        .ok_or("unexpected read data on a command that asked for none")?;
                    if offset + length > buf.len() {
                        return Err("controller returned more data than requested".into());
                    }
                    buf[offset..offset + length].copy_from_slice(payload);
                    // In a data PDU bits 0 and 1 are digest flags, so LAST_PDU
                    // and SUCCESS are bits 2 and 3.
                    const C2H_LAST_PDU: u8 = 0x4;
                    const C2H_SUCCESS: u8 = 0x8;
                    if flags & C2H_LAST_PDU != 0 && flags & C2H_SUCCESS != 0 {
                        return Ok(Cqe {
                            dw0: 0,
                            dw1: 0,
                            status: 0,
                        });
                    }
                }
                PDU_C2H_TERM | PDU_H2C_TERM => {
                    return Err(format!(
                        "controller terminated the connection (PDU {ptype:#04x})"
                    ));
                }
                other => return Err(format!("unexpected PDU type {other:#04x}")),
            }
        }
        Err("controller never completed the command".to_string())
    }

    fn property_get(&mut self, offset: u32, eight: bool) -> Result<u64, String> {
        let cid = self.next_cid();
        let mut sqe = Sqe::new(OPC_FABRICS, cid, 0);
        sqe.byte(4, FCTYPE_PROPERTY_GET)
            .byte(40, if eight { 1 } else { 0 })
            .dw(11, offset);
        self.send_cmd(&sqe, None)?;
        let cqe = self.complete(cid, None, None)?;
        Ok(if eight {
            (cqe.dw0 as u64) | ((cqe.dw1 as u64) << 32)
        } else {
            cqe.dw0 as u64
        })
    }

    fn property_set(&mut self, offset: u32, value: u64, eight: bool) -> Result<(), String> {
        let cid = self.next_cid();
        let mut sqe = Sqe::new(OPC_FABRICS, cid, 0);
        sqe.byte(4, FCTYPE_PROPERTY_SET)
            .byte(40, if eight { 1 } else { 0 })
            .dw(11, offset)
            .dw(12, value as u32)
            .dw(13, (value >> 32) as u32);
        self.send_cmd(&sqe, None)?;
        self.complete(cid, None, None).map(|_| ())
    }

    /// Fabrics Connect. `cntlid` is 0xFFFF on the admin queue (dynamic), and
    /// the id the admin Connect returned on an I/O queue.
    fn fabrics_connect(
        &mut self,
        qid: u16,
        sqsize: u16,
        cntlid: u16,
        subnqn: &str,
        hostnqn: &str,
        hostid: &[u8; 16],
    ) -> Result<u16, String> {
        let mut data = vec![0u8; 1024];
        data[0..16].copy_from_slice(hostid);
        data[16..18].copy_from_slice(&cntlid.to_le_bytes());
        let put = |buf: &mut [u8], s: &str| {
            let b = s.as_bytes();
            let n = b.len().min(255);
            buf[..n].copy_from_slice(&b[..n]);
        };
        put(&mut data[256..512], subnqn);
        put(&mut data[512..768], hostnqn);

        let cid = self.next_cid();
        let mut sqe = Sqe::new(OPC_FABRICS, cid, 0);
        sqe.byte(4, FCTYPE_CONNECT)
            .sgl(0, data.len() as u32, SGL_IN_CAPSULE)
            // CDW10: RECFMT(0) low half, QID high half.
            .dw(10, (qid as u32) << 16)
            // CDW11: SQSIZE, 0-based.
            .dw(11, sqsize.saturating_sub(1) as u32);
        self.send_cmd(&sqe, Some(&data))?;
        let cqe = self.complete(cid, None, None)?;
        Ok((cqe.dw0 & 0xFFFF) as u16)
    }

    /// Bring the controller out of reset.
    ///
    /// Connect only establishes a queue; a conforming target answers every
    /// admin command before this with Command Sequence Error.
    ///
    /// Returns the queue depth the controller allows and `CAP.MPSMIN`, which
    /// is the unit MDTS is counted in and so is needed to make sense of it.
    fn enable_controller(&mut self, stall_ms: &mut dyn FnMut(u32)) -> Result<(u16, u32), String> {
        let cap = self.property_get(REG_CAP, true)?;
        let mqes = ((cap & 0xFFFF) as u16).saturating_add(1);
        // CAP.MPSMIN, bits 51:48: the minimum page size as 2^(12 + MPSMIN).
        let mpsmin = ((cap >> 48) & 0xF) as u32;
        // CAP.TO is in 500ms units.
        let timeout_ms = ((cap >> 24) & 0xFF).max(1) * 500;

        self.property_set(REG_CC, CC_ENABLE, false)?;

        let mut waited = 0u64;
        loop {
            let csts = self.property_get(REG_CSTS, false)?;
            if csts & 0x2 != 0 {
                return Err("controller reports fatal status (CSTS.CFS)".into());
            }
            if csts & 0x1 != 0 {
                return Ok((mqes, mpsmin));
            }
            if waited > timeout_ms {
                return Err(format!("controller not ready {timeout_ms}ms after CC.EN"));
            }
            stall_ms(10);
            waited += 10;
        }
    }
}

/// Geometry of the namespace being booted from.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub blocks: u64,
    pub block_size: u32,
}

/// An attached namespace: admin queue enabled, I/O queue connected.
pub struct Namespace<T: Transport> {
    io: Queue<T>,
    pub nsid: u32,
    pub geometry: Geometry,
    /// Largest transfer this client will issue, in bytes. Derived from the
    /// controller's MDTS; see `transfer_limit`.
    pub max_transfer: usize,
    /// Raw MDTS as the controller reported it, for the console line. Zero
    /// means it stated no limit, or would not answer Identify Controller.
    pub mdts: u8,
    /// The MTU the firmware reported for the path, for the console line. It no
    /// longer sizes anything — see `transfer_limit` — but a slow boot on a
    /// path that turned out to be 1500 is worth being able to see.
    /// `None` means the stack would not say.
    pub mtu: Option<u32>,
}

impl<T: Transport> Namespace<T> {
    /// Connect, enable, identify, and open an I/O queue.
    pub fn attach<P: Platform<Transport = T>>(
        platform: &mut P,
        addr: [u8; 4],
        port: u16,
        subnqn: &str,
        nsid: u32,
        hostnqn: &str,
    ) -> Result<Self, String> {
        // A stable host id derived from the host NQN: the target uses it to
        // recognise this initiator across reconnects, and firmware has no
        // process id or randomness to fall back on.
        let mut hostid = [0u8; 16];
        for (i, b) in hostnqn.as_bytes().iter().take(16).enumerate() {
            hostid[i] = *b;
        }

        let mut admin = Queue::open(platform.connect(addr, port)?)?;
        let cntlid = admin.fabrics_connect(0, 32, 0xFFFF, subnqn, hostnqn, &hostid)?;
        let (mqes, mpsmin) = admin.enable_controller(&mut |ms| platform.stall_ms(ms))?;

        // Identify the namespace: CNS 0x00, nsid in the command.
        let cid = admin.next_cid();
        let mut sqe = Sqe::new(OPC_IDENTIFY, cid, nsid);
        sqe.sgl(0, 4096, SGL_TRANSPORT).dw(10, 0x00);
        let mut idns = vec![0u8; 4096];
        admin.send_cmd(&sqe, None)?;
        admin.complete(cid, None, Some(&mut idns))?;

        // NSZE at 0, FLBAS at 26, LBA format table at 128 (16 entries of 4
        // bytes; byte 2 of an entry is the LBA data size as a power of two).
        let blocks = le64(&idns, 0);
        let flbas = (idns[26] & 0x0F) as usize;
        let lbads = idns[128 + flbas * 4 + 2];
        let block_size = 1u32 << lbads;
        if block_size < 512 || block_size > 65536 {
            return Err(format!("implausible block size {block_size} (LBADS {lbads})"));
        }

        // Identify the controller: CNS 0x01, no namespace. MDTS is byte 77.
        // Not fatal if it fails — a controller that will not describe itself
        // still serves reads, and `transfer_limit` has an answer for silence.
        let mdts = {
            let cid = admin.next_cid();
            let mut sqe = Sqe::new(OPC_IDENTIFY, cid, 0);
            sqe.sgl(0, 4096, SGL_TRANSPORT).dw(10, 0x01);
            let mut idctrl = vec![0u8; 4096];
            match admin
                .send_cmd(&sqe, None)
                .and_then(|_| admin.complete(cid, None, Some(&mut idctrl)))
            {
                Ok(_) => idctrl[77],
                Err(_) => 0,
            }
        };

        // A second connection for I/O, carrying the controller id the admin
        // Connect handed back.
        let mut io = Queue::open(platform.connect(addr, port)?)?;
        let sqsize = mqes.min(128);
        io.fabrics_connect(1, sqsize, cntlid, subnqn, hostnqn, &hostid)?;

        // Ask the queue that will carry the reads, not the admin one: they are
        // separate connections and on a multi-homed portal the firmware could
        // route them over different interfaces.
        let mtu = io.link_mtu();

        Ok(Namespace {
            io,
            nsid,
            geometry: Geometry { blocks, block_size },
            max_transfer: transfer_limit(mdts, mpsmin, block_size),
            mdts,
            mtu,
        })
    }

    pub fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), String> {
        let bs = self.geometry.block_size as usize;
        if buf.len() % bs != 0 {
            return Err(format!("read of {} bytes is not a multiple of {bs}", buf.len()));
        }
        let mut done = 0usize;
        while done < buf.len() {
            let chunk = (buf.len() - done).min(self.max_transfer);
            let blocks = (chunk / bs) as u32;
            let cid = self.io.next_cid();
            let at = lba + (done / bs) as u64;
            let mut sqe = Sqe::new(OPC_READ, cid, self.nsid);
            sqe.sgl(0, chunk as u32, SGL_TRANSPORT)
                .dw(10, at as u32)
                .dw(11, (at >> 32) as u32)
                // CDW12 NLB is 0-based.
                .dw(12, blocks - 1);
            self.io.send_cmd(&sqe, None)?;
            self.io
                .complete(cid, None, Some(&mut buf[done..done + chunk]))?;
            done += chunk;
        }
        Ok(())
    }

    pub fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), String> {
        let bs = self.geometry.block_size as usize;
        if data.len() % bs != 0 {
            return Err(format!(
                "write of {} bytes is not a multiple of {bs}",
                data.len()
            ));
        }
        let mut done = 0usize;
        while done < data.len() {
            let chunk = (data.len() - done).min(self.max_transfer);
            let blocks = (chunk / bs) as u32;
            let cid = self.io.next_cid();
            let at = lba + (done / bs) as u64;
            let mut sqe = Sqe::new(OPC_WRITE, cid, self.nsid);
            sqe.sgl(0, chunk as u32, SGL_TRANSPORT)
                .dw(10, at as u32)
                .dw(11, (at >> 32) as u32)
                .dw(12, blocks - 1);
            self.io.send_cmd(&sqe, None)?;
            self.io
                .complete(cid, Some(&data[done..done + chunk]), None)?;
            done += chunk;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! The initiator against an in-memory controller, over the same traits a
    //! loader implements. The controller keeps the two rules a real target
    //! enforces and stormblockmk does not: a command without PSDT=01b is
    //! answered Invalid Field, and anything but Fabrics before CC.EN is
    //! answered Command Sequence Error. It answers in several C2HData PDUs,
    //! and asks for write data with an R2T, so both paths of `complete` run.

    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::string::String;
    use std::vec::Vec;

    const BLOCK: usize = 4096;
    const BLOCKS: u64 = 64;

    struct Ctrl {
        disk: Vec<u8>,
        enabled: bool,
        /// CSTS.RDY polls left after CC.EN before it reads ready.
        ready_after: u32,
        never_ready: bool,
        mdts: u8,
        digests: bool,
        /// Commands seen without PSDT=01b, or before CC.EN.
        violations: Vec<String>,
        /// Opcodes in order (fabrics as 0x7F00 | fctype).
        log: Vec<u32>,
    }

    struct Conn {
        ctrl: Rc<RefCell<Ctrl>>,
        inbuf: Vec<u8>,
        out: VecDeque<u8>,
        /// A write waiting for its H2CData: cid, lba, length, bytes so far.
        pending: Option<(u16, u64, usize, Vec<u8>)>,
        mtu: Option<u32>,
    }

    fn put16(v: &mut [u8], at: usize, x: u16) {
        v[at..at + 2].copy_from_slice(&x.to_le_bytes());
    }
    fn put32(v: &mut [u8], at: usize, x: u32) {
        v[at..at + 4].copy_from_slice(&x.to_le_bytes());
    }

    impl Conn {
        fn resp(&mut self, cid: u16, dw0: u32, dw1: u32, status: u16) {
            let mut p = vec![0u8; 24];
            p[0] = PDU_RESP;
            p[2] = 24;
            put32(&mut p, 4, 24);
            put32(&mut p, 8, dw0);
            put32(&mut p, 12, dw1);
            put16(&mut p, 20, cid);
            put16(&mut p, 22, status);
            self.out.extend(p);
        }

        /// `data` in C2HData PDUs of at most 3000 bytes; the last carries
        /// LAST_PDU|SUCCESS when `success_flag`, else a CapsuleResp follows.
        fn c2h(&mut self, cid: u16, data: &[u8], success_flag: bool) {
            let mut off = 0;
            while off < data.len() {
                let n = (data.len() - off).min(3000);
                let last = off + n == data.len();
                let mut p = vec![0u8; 24];
                p[0] = PDU_C2H_DATA;
                p[1] = if last && success_flag { 0x0C } else if last { 0x04 } else { 0 };
                p[2] = 24;
                p[3] = 24;
                put32(&mut p, 4, (24 + n) as u32);
                put16(&mut p, 8, cid);
                put32(&mut p, 12, off as u32);
                put32(&mut p, 16, n as u32);
                p.extend_from_slice(&data[off..off + n]);
                self.out.extend(p);
                off += n;
            }
            if !success_flag {
                self.resp(cid, 0, 0, 0);
            }
        }

        fn pdu(&mut self, p: &[u8]) {
            match p[0] {
                PDU_ICREQ => {
                    let mut r = vec![0u8; 128];
                    r[0] = PDU_ICRESP;
                    r[2] = 128;
                    put32(&mut r, 4, 128);
                    r[11] = if self.ctrl.borrow().digests { 0x1 } else { 0 };
                    put32(&mut r, 12, 8192);
                    self.out.extend(r);
                }
                PDU_CMD => {
                    let sqe = &p[8..72];
                    let data = &p[72..];
                    self.command(sqe, data);
                }
                PDU_H2C_DATA => {
                    let last = p[1] & 0x4 != 0;
                    let (cid, lba, len, mut got) = self.pending.take().expect("H2CData with no R2T");
                    assert_eq!(le16(p, 8), cid);
                    assert_eq!(le32(p, 12) as usize, got.len(), "H2CData out of order");
                    got.extend_from_slice(&p[24..]);
                    if last {
                        assert_eq!(got.len(), len);
                        let at = lba as usize * BLOCK;
                        self.ctrl.borrow_mut().disk[at..at + len].copy_from_slice(&got);
                        self.resp(cid, 0, 0, 0);
                    } else {
                        self.pending = Some((cid, lba, len, got));
                    }
                }
                t => panic!("the initiator sent PDU type {t:#04x}"),
            }
        }

        fn command(&mut self, sqe: &[u8], data: &[u8]) {
            let opc = sqe[0];
            let cid = le16(sqe, 2);
            let dw = |n: usize| le32(sqe, 40 + (n - 10) * 4);
            let fabrics = opc == OPC_FABRICS;
            self.ctrl.borrow_mut().log.push(if fabrics { 0x7F00 | sqe[4] as u32 } else { opc as u32 });
            // Invalid Field (sct 0, sc 0x02) / Command Sequence Error (sc 0x0C).
            if sqe[1] & 0xC0 != FLAGS_SGL {
                self.ctrl.borrow_mut().violations.push(format!("opcode {opc:#04x} without PSDT=01b"));
                return self.resp(cid, 0, 0, 0x02 << 1);
            }
            if !fabrics && !self.ctrl.borrow().enabled {
                self.ctrl.borrow_mut().violations.push(format!("opcode {opc:#04x} before CC.EN"));
                return self.resp(cid, 0, 0, 0x0C << 1);
            }
            match (opc, sqe[4]) {
                (OPC_FABRICS, FCTYPE_CONNECT) => {
                    assert_eq!(data.len(), 1024, "Connect carries its data in the capsule");
                    let qid = dw(10) >> 16;
                    let cntlid = le16(data, 16);
                    if qid == 0 {
                        assert_eq!(cntlid, 0xFFFF);
                    } else {
                        assert_eq!(cntlid, 7, "the I/O Connect names the admin's controller");
                    }
                    assert!(data[256..].starts_with(b"nqn.test:sub"));
                    assert!(data[512..].starts_with(b"nqn.test:host"));
                    self.resp(cid, 7, 0, 0);
                }
                (OPC_FABRICS, FCTYPE_PROPERTY_GET) => {
                    let mut c = self.ctrl.borrow_mut();
                    let v: u64 = match dw(11) {
                        // MQES 127 (0-based), CAP.TO 1 (500 ms), MPSMIN 0.
                        REG_CAP => 127 | (1 << 24),
                        REG_CSTS => {
                            if c.enabled && !c.never_ready && c.ready_after == 0 {
                                1
                            } else {
                                c.ready_after = c.ready_after.saturating_sub(1);
                                0
                            }
                        }
                        r => panic!("property get {r:#x}"),
                    };
                    drop(c);
                    self.resp(cid, v as u32, (v >> 32) as u32, 0);
                }
                (OPC_FABRICS, FCTYPE_PROPERTY_SET) => {
                    assert_eq!(dw(11), REG_CC);
                    assert_eq!(dw(12) as u64, CC_ENABLE);
                    self.ctrl.borrow_mut().enabled = true;
                    self.resp(cid, 0, 0, 0);
                }
                (OPC_IDENTIFY, _) => {
                    let mut id = vec![0u8; 4096];
                    if dw(10) == 0 {
                        id[0..8].copy_from_slice(&BLOCKS.to_le_bytes());
                        id[26] = 1; // FLBAS: format 1
                        id[128 + 4 + 2] = 12; // format 1 is 4096 bytes
                        id[128 + 2] = 9; // format 0 is 512: must not be used
                        self.c2h(cid, &id, true);
                    } else {
                        id[77] = self.ctrl.borrow().mdts;
                        self.c2h(cid, &id, false);
                    }
                }
                (OPC_READ, _) => {
                    let lba = dw(10) as u64 | (dw(11) as u64) << 32;
                    let n = (dw(12) as usize + 1) * BLOCK;
                    assert_eq!(le32(sqe, 32) as usize, n, "the SGL length is the transfer");
                    let at = lba as usize * BLOCK;
                    let bytes = self.ctrl.borrow().disk[at..at + n].to_vec();
                    self.c2h(cid, &bytes, true);
                }
                (OPC_WRITE, _) => {
                    let lba = dw(10) as u64 | (dw(11) as u64) << 32;
                    let n = (dw(12) as usize + 1) * BLOCK;
                    let mut r = vec![0u8; 24];
                    r[0] = PDU_R2T;
                    r[2] = 24;
                    put32(&mut r, 4, 24);
                    put16(&mut r, 8, cid);
                    put16(&mut r, 10, 0x55);
                    put32(&mut r, 12, 0);
                    put32(&mut r, 16, n as u32);
                    self.out.extend(r);
                    self.pending = Some((cid, lba, n, Vec::new()));
                }
                (o, f) => panic!("unexpected command {o:#04x}/{f:#04x}"),
            }
        }
    }

    impl Transport for Conn {
        type Error = String;
        fn send_all(&mut self, buf: &[u8]) -> Result<(), String> {
            self.inbuf.extend_from_slice(buf);
            loop {
                if self.inbuf.len() < 8 {
                    return Ok(());
                }
                let plen = le32(&self.inbuf, 4) as usize;
                if self.inbuf.len() < plen {
                    return Ok(());
                }
                let p: Vec<u8> = self.inbuf.drain(..plen).collect();
                self.pdu(&p);
            }
        }
        fn recv_exact(&mut self, buf: &mut [u8]) -> Result<(), String> {
            if self.out.len() < buf.len() {
                return Err(format!("receive timed out ({} of {} bytes)", self.out.len(), buf.len()));
            }
            for b in buf.iter_mut() {
                *b = self.out.pop_front().unwrap();
            }
            Ok(())
        }
        fn link_mtu(&self) -> Option<u32> {
            self.mtu
        }
    }

    struct Fake {
        ctrl: Rc<RefCell<Ctrl>>,
        connects: u32,
        stalled_ms: u32,
    }

    impl Platform for Fake {
        type Transport = Conn;
        fn connect(&mut self, addr: [u8; 4], port: u16) -> Result<Conn, String> {
            assert_eq!((addr, port), ([10, 0, 0, 1], 4420));
            self.connects += 1;
            Ok(Conn { ctrl: self.ctrl.clone(), inbuf: Vec::new(), out: VecDeque::new(), pending: None, mtu: Some(9000) })
        }
        fn stall_ms(&mut self, ms: u32) {
            self.stalled_ms += ms;
        }
    }

    fn fake(mdts: u8) -> Fake {
        let disk = (0..BLOCKS as usize * BLOCK).map(|i| (i * 7 + i / 4096) as u8).collect();
        let ctrl = Ctrl {
            disk,
            enabled: false,
            ready_after: 3,
            never_ready: false,
            mdts,
            digests: false,
            violations: Vec::new(),
            log: Vec::new(),
        };
        Fake { ctrl: Rc::new(RefCell::new(ctrl)), connects: 0, stalled_ms: 0 }
    }

    fn attach(f: &mut Fake) -> Result<Namespace<Conn>, String> {
        Namespace::attach(f, [10, 0, 0, 1], 4420, "nqn.test:sub", 3, "nqn.test:host")
    }

    #[test]
    fn attaches_enabling_before_identify_with_psdt_on_every_command() {
        let mut f = fake(1);
        let ns = attach(&mut f).unwrap();
        assert_eq!(ns.geometry.blocks, BLOCKS);
        assert_eq!(ns.geometry.block_size, 4096, "the block size comes from FLBAS, not format 0");
        assert_eq!(ns.mdts, 1);
        assert_eq!(ns.max_transfer, 8192, "MDTS 1 at MPSMIN 0 is two 4 KiB pages");
        assert_eq!(ns.mtu, Some(9000), "the I/O queue's path is asked");
        assert_eq!(f.connects, 2, "an admin queue and an I/O queue");
        assert_eq!(f.stalled_ms, 30, "three CSTS polls before RDY, 10 ms apart");
        let c = f.ctrl.borrow();
        assert!(c.violations.is_empty(), "{:?}", c.violations);
        // Connect, CAP, CC.EN, CSTS until ready, then Identify NS and
        // controller, then the I/O queue's Connect.
        let set = c.log.iter().position(|&o| o == 0x7F00).unwrap();
        let first_identify = c.log.iter().position(|&o| o == OPC_IDENTIFY as u32).unwrap();
        assert!(set < first_identify, "{:x?}", c.log);
        assert_eq!(c.log.first(), Some(&0x7F01));
        assert_eq!(c.log.last(), Some(&0x7F01));
    }

    #[test]
    fn reads_span_commands_and_land_in_place() {
        let mut f = fake(1);
        let mut ns = attach(&mut f).unwrap();
        // 5 blocks from LBA 9: three commands of at most 8 KiB.
        let mut buf = vec![0u8; 5 * BLOCK];
        ns.read(9, &mut buf).unwrap();
        assert!(buf == f.ctrl.borrow().disk[9 * BLOCK..14 * BLOCK]);
        let reads = f.ctrl.borrow().log.iter().filter(|&&o| o == OPC_READ as u32).count();
        assert_eq!(reads, 3);
        assert!(ns.read(0, &mut [0u8; 100]).is_err(), "a read that is not whole blocks");
    }

    #[test]
    fn writes_answer_r2t_and_read_back() {
        let mut f = fake(0);
        let mut ns = attach(&mut f).unwrap();
        assert_eq!(ns.max_transfer, 64 * 1024, "no MDTS: the 64 KiB this client always used");
        // 20 KiB: one command, sent as three H2CData PDUs of maxh2cdata 8 KiB.
        let data: Vec<u8> = (0..5 * BLOCK).map(|i| (i % 251) as u8).collect();
        ns.write(20, &data).unwrap();
        let mut back = vec![0u8; data.len()];
        ns.read(20, &mut back).unwrap();
        assert!(back == data);
        assert!(f.ctrl.borrow().violations.is_empty());
    }

    #[test]
    fn a_controller_that_never_comes_ready_fails_the_attach() {
        let mut f = fake(1);
        f.ctrl.borrow_mut().never_ready = true;
        let e = attach(&mut f).err().unwrap();
        assert!(e.contains("not ready 500ms after CC.EN"), "{e}");
        assert!(f.stalled_ms > 500);
    }

    #[test]
    fn a_controller_that_insists_on_digests_is_refused() {
        let mut f = fake(1);
        f.ctrl.borrow_mut().digests = true;
        let e = attach(&mut f).err().unwrap();
        assert!(e.contains("insists on PDU digests"), "{e}");
    }

    #[test]
    fn transport_errors_come_back_as_the_transport_said_them() {
        let mut q = Queue::open(Conn {
            ctrl: fake(1).ctrl,
            inbuf: Vec::new(),
            out: VecDeque::new(),
            pending: None,
            mtu: None,
        })
        .unwrap();
        let e = q.read_exact(8).err().unwrap();
        assert_eq!(e, "receive timed out (0 of 8 bytes)");
    }

    #[test]
    fn transfer_limits() {
        assert_eq!(transfer_limit(0, 0, 4096), 64 * 1024);
        assert_eq!(transfer_limit(5, 0, 4096), 128 * 1024, "the stormblock target's MDTS 5");
        assert_eq!(transfer_limit(20, 0, 512), 512 * 1024, "the ceiling");
        assert_eq!(transfer_limit(1, 0, 65536), 65536, "never less than one block");
        assert_eq!(transfer_limit(1, 1, 4096), 16 * 1024, "MPSMIN is the unit");
    }
}
