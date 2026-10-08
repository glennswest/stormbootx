//! The NVMe/TCP initiator, on stormbootx's own TCP/IP.
//!
//! The initiator is the `nvme-tcp-initiator` crate (`crates/`, #10), shared
//! with stormboot4bios: the wire format, PSDT = 01b on every command, CC.EN
//! before admin commands, the MDTS-sized transfer, all live there and are
//! host-tested there. This is the UEFI end of it: the byte stream is a
//! `net::TcpSocket` (smoltcp on the NIC's SNP, #56), and the wait for the
//! controller to come ready is `boot::stall`.

use alloc::string::String;
use nvme_tcp_initiator::{Platform, Transport};

use crate::net::TcpSocket;

/// An attached namespace on a stormbootx socket.
pub type Namespace = nvme_tcp_initiator::Namespace<TcpSocket>;

impl Transport for TcpSocket {
    type Error = String;
    fn send_all(&mut self, buf: &[u8]) -> Result<(), String> {
        self.send(buf)
    }
    fn recv_exact(&mut self, buf: &mut [u8]) -> Result<(), String> {
        self.read_into(buf)
    }
    fn link_mtu(&self) -> Option<u32> {
        TcpSocket::link_mtu(self)
    }
}

/// Connections from smoltcp, waits from the firmware.
struct Firmware;

impl Platform for Firmware {
    type Transport = TcpSocket;
    fn connect(&mut self, addr: [u8; 4], port: u16) -> Result<TcpSocket, String> {
        TcpSocket::connect(addr, port)
    }
    fn stall_ms(&mut self, ms: u32) {
        uefi::boot::stall(core::time::Duration::from_millis(ms as u64));
    }
}

/// Connect, enable, identify, and open an I/O queue (see the crate).
pub fn attach(addr: [u8; 4], port: u16, subnqn: &str, nsid: u32, hostnqn: &str) -> Result<Namespace, String> {
    Namespace::attach(&mut Firmware, addr, port, subnqn, nsid, hostnqn)
}
