use crate::envelope::{Datagram, Protocol};
use crate::net::UdpSocket;
use crate::{trace, Envelope};

use indexmap::IndexMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

/// A host in the simulated network.
///
/// Hosts have UDP and TCP (coming soon...) software available for networking.
///
/// Both modes may be used by host software simultaneously.
pub(crate) struct Host {
    /// Host ip address.
    pub(crate) addr: IpAddr,

    pub(crate) udp: Udp,

    /// Current instant at the host.
    pub(crate) now: Instant,

    _epoch: Instant,
}

impl Host {
    pub(crate) fn new(addr: IpAddr, now: Instant) -> Host {
        Host {
            addr,
            udp: Udp::install(),
            now,
            _epoch: now,
        }
    }

    /// Returns how long the host has been executing for in virtual time
    pub(crate) fn _elapsed(&self) -> Duration {
        self.now - self._epoch
    }

    pub(crate) fn receive_from_network(&mut self, envelope: Envelope) {
        let Envelope { src, dst, message } = envelope;

        trace!("Delivered {} {} {}", dst, src, message);

        match message {
            Protocol::Udp(datagram) => self.udp.receive_from_network(src, dst, datagram),
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

    pub(crate) fn bind(&mut self, addr: SocketAddr) -> io::Result<UdpSocket> {
        let (tx, rx) = mpsc::unbounded_channel();

        if self.binds.insert(addr, tx).is_some() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, addr.to_string()));
        }

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
        assert!(self.binds.remove(&addr).is_some(), "unknown bind {}", addr)
    }
}
