//! Structured packet types.
//!
//! Packets are not byte-encoded — the fields carry the same demux
//! information (src/dst addrs, ports, TCP flags/seq/ack) a real kernel
//! would read from IP + transport headers, but as typed data. This
//! preserves state-machine fidelity without a parser. Byte-level framing
//! can be added later if packet-corruption fault injection is needed.
//!
//! Header sizes below are used for MTU accounting only — no bytes
//! actually exist on the wire.

use std::net::IpAddr;

use bytes::Bytes;

/// IPv4 header size in bytes, no options.
pub const IPV4_HEADER_SIZE: u16 = 20;
/// IPv6 header size in bytes, no extension headers.
pub const IPV6_HEADER_SIZE: u16 = 40;
/// UDP header size in bytes.
pub const UDP_HEADER_SIZE: u16 = 8;
/// TCP header size in bytes, no options.
pub const TCP_HEADER_SIZE: u16 = 20;

/// An IP-layer packet. UDS lives outside this model — Unix sockets
/// bypass the network stack and deliver socket-to-socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub src: IpAddr,
    pub dst: IpAddr,
    pub ttl: u8,
    pub payload: Transport,
}

impl Packet {
    /// Total size in bytes if this packet were actually serialized —
    /// IP header + transport header + payload. Used to enforce MTU.
    pub fn size(&self) -> u32 {
        let ip_hdr = match self.dst {
            IpAddr::V4(_) => IPV4_HEADER_SIZE,
            IpAddr::V6(_) => IPV6_HEADER_SIZE,
        } as u32;
        ip_hdr + self.payload.size()
    }
}

/// Transport-layer payload of a [`Packet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Udp(UdpDatagram),
    Tcp(TcpSegment),
}

impl Transport {
    /// Transport header + payload size.
    pub fn size(&self) -> u32 {
        match self {
            Transport::Udp(d) => UDP_HEADER_SIZE as u32 + d.payload.len() as u32,
            Transport::Tcp(s) => TCP_HEADER_SIZE as u32 + s.payload.len() as u32,
        }
    }
}

/// UDP datagram — `(src_port, dst_port, payload)`. Addresses live on the
/// enclosing [`Packet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpDatagram {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: Bytes,
}

/// TCP segment. Fields mirror what a real kernel needs for state-machine
/// decisions; anything not listed here (urgent pointer, options beyond
/// MSS, checksum) is intentionally omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpSegment {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: TcpFlags,
    pub window: u16,
    pub payload: Bytes,
}

/// TCP control flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TcpFlags {
    pub syn: bool,
    pub ack: bool,
    pub fin: bool,
    pub rst: bool,
    pub psh: bool,
    pub urg: bool,
}
