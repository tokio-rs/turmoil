use crate::envelope::{Datagram, Protocol, Segment, StreamData, Syn};
use crate::net::{SocketPair, TcpListener, UdpSocket};
use crate::{trace, Envelope};

use indexmap::IndexMap;
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

    /// Ports 1024 - 65535 for client connections.
    next_ephemeral_port: u16,

    /// Current instant at the host.
    pub(crate) now: Instant,

    _epoch: Instant,
}

impl Host {
    pub(crate) fn new(addr: IpAddr, now: Instant) -> Host {
        Host {
            addr,
            udp: Udp::install(),
            tcp: Tcp::install(),
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
        // re-load
        if self.next_ephemeral_port == 65535 {
            self.next_ephemeral_port = 1024;
        }

        // Check for existing binds to avoid port conflicts
        loop {
            let ret = self.next_ephemeral_port;
            self.next_ephemeral_port += 1;

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
    binds: IndexMap<SocketAddr, mpsc::UnboundedSender<(Datagram, SocketAddr)>>,
}

impl Udp {
    fn install() -> Self {
        Self {
            binds: IndexMap::new(),
        }
    }

    fn is_port_assigned(&self, port: u16) -> bool {
        self.binds.keys().any(|a| a.port() == port)
    }

    pub(crate) fn bind(&mut self, addr: SocketAddr) -> io::Result<UdpSocket> {
        let (tx, rx) = mpsc::unbounded_channel();

        if self.binds.insert(addr, tx).is_some() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, addr.to_string()));
        }

        trace!(?addr, protocol = %"UDP", "Bind");

        Ok(UdpSocket::new(addr, rx))
    }

    fn receive_from_network(&mut self, src: SocketAddr, dst: SocketAddr, datagram: Datagram) {
        match self.binds.get_mut(&dst) {
            Some(s) => {
                let _ = s.send((datagram, src));
            }
            _ => {}
        }
    }

    pub(crate) fn unbind(&mut self, addr: SocketAddr) {
        assert!(self.binds.remove(&addr).is_some(), "unknown bind {}", addr);

        trace!(?addr, protocol = %"UDP", "Unbind");
    }
}

pub(crate) struct Tcp {
    /// Bound server sockets
    binds: IndexMap<SocketAddr, mpsc::UnboundedSender<(Syn, SocketAddr)>>,

    /// Active stream sockets
    sockets: IndexMap<SocketPair, StreamSocket>,
}

struct StreamSocket {
    buf: IndexMap<u64, StreamData>,
    next_send_seq: u64,
    recv_seq: u64,
    sender: mpsc::UnboundedSender<StreamData>,
}

impl StreamSocket {
    fn new() -> (Self, mpsc::UnboundedReceiver<StreamData>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let sock = Self {
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
    fn buffer(&mut self, seq: u64, data: StreamData) {
        assert!(
            self.buf.insert(seq, data).is_none(),
            "duplicate segment {}",
            seq
        );

        while self.buf.contains_key(&(self.recv_seq + 1)) {
            self.recv_seq += 1;

            let data = self.buf.remove(&self.recv_seq).unwrap();
            let _ = self.sender.send(data);
        }
    }
}

impl Tcp {
    fn install() -> Self {
        Self {
            binds: IndexMap::new(),
            sockets: IndexMap::new(),
        }
    }

    fn is_port_assigned(&self, port: u16) -> bool {
        self.binds.keys().any(|a| a.port() == port)
            || self.sockets.keys().any(|a| a.local.port() == port)
    }

    pub(crate) fn bind(&mut self, addr: SocketAddr) -> io::Result<TcpListener> {
        let (tx, rx) = mpsc::unbounded_channel();

        if self.binds.insert(addr, tx).is_some() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, addr.to_string()));
        }

        trace!(?addr, protocol = %"TCP", "Bind");

        Ok(TcpListener::new(addr, rx))
    }

    pub(crate) fn new_stream(&mut self, pair: SocketPair) -> mpsc::UnboundedReceiver<StreamData> {
        let (sock, rx) = StreamSocket::new();

        assert!(
            self.sockets.insert(pair, sock).is_none(),
            "{:?} is already connected",
            pair
        );

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
                    let _ = b.send((syn, src));
                }
            }
            Segment::Data(seq, data) => match self.sockets.get_mut(&SocketPair::new(dst, src)) {
                Some(sock) => sock.buffer(seq, data),
                None => return Err(Protocol::Tcp(Segment::Rst)),
            },
            Segment::Fin(seq) => match self.sockets.get_mut(&SocketPair::new(dst, src)) {
                Some(sock) => sock.buffer(seq, StreamData::eof()),
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
        assert!(
            self.sockets.remove(&pair).is_some(),
            "{:?} is already disconnected",
            pair
        );
    }

    pub(crate) fn unbind(&mut self, addr: SocketAddr) {
        assert!(self.binds.remove(&addr).is_some(), "unknown bind {}", addr);

        trace!(?addr, protocol = %"TCP", "Unbind");
    }
}
