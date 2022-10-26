use crate::envelope::{hex, Datagram, Protocol, Segment, Syn};
use crate::net::{SocketPair, TcpListener, UdpSocket};
use crate::{trace, Envelope};

use bytes::Bytes;
use indexmap::IndexMap;
use std::fmt::Display;
use std::io;
use std::net::{IpAddr, SocketAddr};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

/// A host in the simulated network.
///
/// Hosts have [`Udp`] and [`Tcp`] software available for networking.
///
/// Both modes may be used simultaneously.
pub(crate) struct Host {
    /// Host ip address.
    pub(crate) addr: IpAddr,

    /// L4 User Datagram Protocol (UDP).
    pub(crate) udp: Udp,

    /// L4 Transmission Control Protocol (TCP).
    pub(crate) tcp: Tcp,

    /// Ports 1024..=65535 for client connections.
    next_ephemeral_port: u16,

    /// Current instant at the host.
    pub(crate) now: Instant,

    _epoch: Instant,
}

impl Host {
    pub(crate) fn new(addr: IpAddr, now: Instant) -> Host {
        Host {
            addr,
            udp: Udp::new(),
            tcp: Tcp::new(),
            next_ephemeral_port: 1024,
            now,
            _epoch: now,
        }
    }

    /// Returns how long the host has been executing for in virtual time
    pub(crate) fn _elapsed(&self) -> Duration {
        self.now - self._epoch
    }

    pub(crate) fn assign_ephemeral_port(&mut self) -> u16 {
        // Check for existing binds to avoid port conflicts
        loop {
            let ret = self.next_ephemeral_port;

            if self.next_ephemeral_port == 65535 {
                // re-load
                self.next_ephemeral_port = 1024;
            } else {
                // advance
                self.next_ephemeral_port += 1;
            }

            if self.udp.is_port_assigned(ret) || self.tcp.is_port_assigned(ret) {
                continue;
            }

            return ret;
        }
    }

    /// Receive the `envelope` from the network.
    ///
    /// Returns an Err if a message needs to be sent in response to a failed
    /// delivery, e.g. TCP RST.
    // FIXME: This funkiness is necessary due to how message sending works. The
    // key problem is that the Host doesn't actually send messages, rather the
    // World is borrowed, and it sends.
    pub(crate) fn receive_from_network(&mut self, envelope: Envelope) -> Result<(), Protocol> {
        let Envelope { src, dst, message } = envelope;

        trace!(?dst, ?src, protocol = %message, "Delivered");

        match message {
            Protocol::Tcp(segment) => self.tcp.receive_from_network(src, dst, segment),
            Protocol::Udp(datagram) => {
                self.udp.receive_from_network(src, dst, datagram);
                Ok(())
            }
        }
    }

    pub(crate) fn tick(&mut self, now: Instant) {
        self.now = now;
    }
}

/// Simulated UDP host software.
pub(crate) struct Udp {
    /// Bound udp sockets
    binds: IndexMap<SocketAddr, mpsc::Sender<(Datagram, SocketAddr)>>,

    /// UdpSocket channel capacity
    capacity: usize,
}

impl Udp {
    fn new() -> Self {
        Self {
            binds: IndexMap::new(),
            // TODO: Make capacity configurable
            capacity: 64,
        }
    }

    fn is_port_assigned(&self, port: u16) -> bool {
        self.binds.keys().any(|a| a.port() == port)
    }

    pub(crate) fn bind(&mut self, addr: SocketAddr) -> io::Result<UdpSocket> {
        let (tx, rx) = mpsc::channel(self.capacity);

        if self.binds.insert(addr, tx).is_some() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, addr.to_string()));
        }

        trace!(?addr, protocol = %"UDP", "Bind");

        Ok(UdpSocket::new(addr, rx))
    }

    fn receive_from_network(&mut self, src: SocketAddr, dst: SocketAddr, datagram: Datagram) {
        match self.binds.get_mut(&dst) {
            Some(s) => s
                .try_send((datagram, src))
                .expect(&format!("unable to send to {}", dst)),
            _ => {}
        };
    }

    pub(crate) fn unbind(&mut self, addr: SocketAddr) {
        let exists = self.binds.remove(&addr);

        assert!(exists.is_some(), "unknown bind {}", addr);

        trace!(?addr, protocol = %"UDP", "Unbind");
    }
}

pub(crate) struct Tcp {
    /// Bound server sockets
    binds: IndexMap<SocketAddr, mpsc::Sender<(Syn, SocketAddr)>>,

    /// TcpListener channel capacity
    server_socket_capacity: usize,

    /// Active stream sockets
    sockets: IndexMap<SocketPair, StreamSocket>,

    /// TcpStream channel capacity
    socket_capacity: usize,
}

struct StreamSocket {
    local_addr: SocketAddr,
    buf: IndexMap<u64, SequencedSegment>,
    next_send_seq: u64,
    recv_seq: u64,
    sender: mpsc::Sender<SequencedSegment>,
}

/// Stripped down version of [`Segment`] for delivery out to the application
/// layer.
#[derive(Debug)]
pub(crate) enum SequencedSegment {
    Data(Bytes),
    Fin,
}

impl Display for SequencedSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SequencedSegment::Data(data) => hex("TCP", &data, f),
            SequencedSegment::Fin => write!(f, "TCP FIN"),
        }
    }
}

impl StreamSocket {
    fn new(local_addr: SocketAddr, capacity: usize) -> (Self, mpsc::Receiver<SequencedSegment>) {
        let (tx, rx) = mpsc::channel(capacity);
        let sock = Self {
            local_addr,
            buf: IndexMap::new(),
            next_send_seq: 1,
            recv_seq: 0,
            sender: tx,
        };

        (sock, rx)
    }

    fn assign_seq(&mut self) -> u64 {
        let seq = self.next_send_seq;
        self.next_send_seq += 1;
        seq
    }

    // Buffer and re-order received segments by `seq` as the network may deliver
    // them out of order.
    fn buffer(&mut self, seq: u64, segment: SequencedSegment) {
        let exists = self.buf.insert(seq, segment);

        assert!(exists.is_none(), "duplicate segment {}", seq);

        while self.buf.contains_key(&(self.recv_seq + 1)) {
            self.recv_seq += 1;

            let segment = self.buf.remove(&self.recv_seq).unwrap();
            self.sender
                .try_send(segment)
                .expect(&format!("unable to send to {}", self.local_addr));
        }
    }
}

impl Tcp {
    fn new() -> Self {
        Self {
            binds: IndexMap::new(),
            sockets: IndexMap::new(),
            // TODO: Make capacity configurable
            server_socket_capacity: 64,
            socket_capacity: 64,
        }
    }

    fn is_port_assigned(&self, port: u16) -> bool {
        self.binds.keys().any(|a| a.port() == port)
            || self.sockets.keys().any(|a| a.local.port() == port)
    }

    pub(crate) fn bind(&mut self, addr: SocketAddr) -> io::Result<TcpListener> {
        let (tx, rx) = mpsc::channel(self.server_socket_capacity);

        if self.binds.insert(addr, tx).is_some() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, addr.to_string()));
        }

        trace!(?addr, protocol = %"TCP", "Bind");

        Ok(TcpListener::new(addr, rx))
    }

    pub(crate) fn new_stream(&mut self, pair: SocketPair) -> mpsc::Receiver<SequencedSegment> {
        let (sock, rx) = StreamSocket::new(pair.local, self.socket_capacity);

        let exists = self.sockets.insert(pair, sock);

        assert!(exists.is_none(), "{:?} is already connected", pair);

        rx
    }

    // Ideally, we could "write through" the tcp software, but this is necessary
    // due to borrowing the world to access the mut host and for sending.
    pub(crate) fn assing_send_seq(&mut self, pair: SocketPair) -> Option<u64> {
        let sock = self.sockets.get_mut(&pair)?;
        Some(sock.assign_seq())
    }

    fn receive_from_network(
        &mut self,
        src: SocketAddr,
        dst: SocketAddr,
        segment: Segment,
    ) -> Result<(), Protocol> {
        match segment {
            Segment::Syn(syn) => {
                // If bound, queue the syn; else we drop the syn triggering
                // connection refused on the client.
                if let Some(b) = self.binds.get_mut(&dst) {
                    b.try_send((syn, src))
                        .expect(&format!("unable to send to {}", dst));
                }
            }
            Segment::Data(seq, data) => match self.sockets.get_mut(&SocketPair::new(dst, src)) {
                Some(sock) => sock.buffer(seq, SequencedSegment::Data(data)),
                None => return Err(Protocol::Tcp(Segment::Rst)),
            },
            Segment::Fin(seq) => match self.sockets.get_mut(&SocketPair::new(dst, src)) {
                Some(sock) => sock.buffer(seq, SequencedSegment::Fin),
                None => return Err(Protocol::Tcp(Segment::Rst)),
            },
            Segment::Rst => {
                if let Some(_) = self.sockets.get(&SocketPair::new(dst, src)) {
                    self.sockets.remove(&SocketPair::new(dst, src)).unwrap();
                }
            }
        };

        Ok(())
    }

    pub(crate) fn remove_stream(&mut self, pair: SocketPair) {
        let exists = self.sockets.remove(&pair);

        assert!(exists.is_some(), "unknown socket {:?}", pair);
    }

    pub(crate) fn unbind(&mut self, addr: SocketAddr) {
        let exists = self.binds.remove(&addr);

        assert!(exists.is_some(), "unknown bind {}", addr);

        trace!(?addr, protocol = %"TCP", "Unbind");
    }
}

#[cfg(test)]
mod test {
    use crate::{Host, Result};

    #[test]
    fn recycle_ports() -> Result {
        let mut host = Host::new(
            std::net::Ipv4Addr::UNSPECIFIED.into(),
            tokio::time::Instant::now(),
        );

        host.udp.bind((host.addr, 65534).into())?;
        host.udp.bind((host.addr, 65535).into())?;

        for _ in 1024..65534 {
            host.assign_ephemeral_port();
        }

        assert_eq!(1024, host.assign_ephemeral_port());

        Ok(())
    }
}
