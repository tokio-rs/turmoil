use crate::envelope::{Datagram, Protocol};
use crate::net::UdpSocket;
use crate::world::World;
use crate::{kernel, Envelope, TRACING_TARGET};

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
    pub(crate) tcp: kernel::tcp::Tcp,

    /// Ports 49152..=65535 for client connections.
    /// https://www.rfc-editor.org/rfc/rfc6335#section-6
    next_ephemeral_port: u16,

    /// Host elapsed time.
    elapsed: Duration,

    /// Set each time the software is run.
    now: Option<Instant>,
}

impl Host {
    pub(crate) fn new(addr: IpAddr, tcp_capacity: usize, udp_capacity: usize) -> Host {
        Host {
            addr,
            udp: Udp::new(udp_capacity),
            tcp: kernel::tcp::Tcp::new(),
            next_ephemeral_port: 49152,
            elapsed: Duration::ZERO,
            now: None,
        }
    }

    /// Set a new `Instant` for each iteration of the simulation. `elapsed` is
    /// updated after each iteration via `tick()`, where as this value is
    /// necessary to accurately calculate elapsed time while the software is
    /// running.
    ///
    /// This is required to track logical time across host restarts as a single
    /// `Instant` resets when the tokio runtime is recreated.
    pub(crate) fn now(&mut self, now: Instant) {
        self.now.replace(now);
    }

    /// Returns how long the host has been executing for in virtual time.
    pub(crate) fn elapsed(&self) -> Duration {
        let run_duration = self.now.expect("host instant not set").elapsed();
        self.elapsed + run_duration
    }

    pub(crate) fn assign_ephemeral_port(&mut self) -> u16 {
        // Check for existing binds to avoid port conflicts
        loop {
            let ret = self.next_ephemeral_port;

            if self.next_ephemeral_port == 65535 {
                // re-load
                self.next_ephemeral_port = 49152;
            } else {
                // advance
                self.next_ephemeral_port += 1;
            }

            if self.udp.is_port_assigned(ret) {
                //} || self.tcp.is_port_assigned(ret) {
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

        tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %message, "Delivered");

        match message {
            Protocol::Tcp(segment) => self.tcp.receive_from_network(src, dst, segment),
            Protocol::Udp(datagram) => {
                self.udp.receive_from_network(src, dst, datagram);
                Ok(())
            }
        }
    }

    pub(crate) fn tick(&mut self, duration: Duration) {
        self.elapsed += duration
    }
}

/// Returns how long the currently executing host has been executing for in
/// virtual time.
///
/// Must be called from within a Turmoil simulation.
pub fn elapsed() -> Duration {
    World::current(|world| world.current_host_mut().elapsed())
}

/// Simulated UDP host software.
pub(crate) struct Udp {
    /// Bound udp sockets
    binds: IndexMap<u16, UdpBind>,

    /// UdpSocket channel capacity
    capacity: usize,
}

struct UdpBind {
    bind_addr: SocketAddr,
    queue: mpsc::Sender<(Datagram, SocketAddr)>,
}

impl Udp {
    fn new(capacity: usize) -> Self {
        Self {
            binds: IndexMap::new(),
            capacity,
        }
    }

    fn is_port_assigned(&self, port: u16) -> bool {
        self.binds.keys().any(|p| *p == port)
    }

    pub(crate) fn bind(&mut self, addr: SocketAddr) -> io::Result<UdpSocket> {
        let (tx, rx) = mpsc::channel(self.capacity);
        let bind = UdpBind {
            bind_addr: addr,
            queue: tx,
        };

        match self.binds.entry(addr.port()) {
            indexmap::map::Entry::Occupied(_) => {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, addr.to_string()));
            }
            indexmap::map::Entry::Vacant(entry) => entry.insert(bind),
        };

        tracing::info!(target: TRACING_TARGET, ?addr, protocol = %"UDP", "Bind");

        Ok(UdpSocket::new(addr, rx))
    }

    fn receive_from_network(&mut self, src: SocketAddr, dst: SocketAddr, datagram: Datagram) {
        if let Some(bind) = self.binds.get_mut(&dst.port()) {
            if !matches(bind.bind_addr, dst) {
                tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Protocol::Udp(datagram), "Dropped (Addr not bound)");
                return;
            }
            if let Err(err) = bind.queue.try_send((datagram, src)) {
                // drop any packets that exceed the capacity
                match err {
                    mpsc::error::TrySendError::Full((datagram, _)) => {
                        tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Protocol::Udp(datagram), "Dropped (Full buffer)");
                    }
                    mpsc::error::TrySendError::Closed((datagram, _)) => {
                        tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Protocol::Udp(datagram), "Dropped (Receiver closed)");
                    }
                }
            }
        }
    }

    pub(crate) fn unbind(&mut self, addr: SocketAddr) {
        let exists = self.binds.remove(&addr.port());

        assert!(exists.is_some(), "unknown bind {addr}");

        tracing::info!(target: TRACING_TARGET, ?addr, protocol = %"UDP", "Unbind");
    }
}

/// Returns whether the given bind addr can accept a packet routed to the given dst
pub fn matches(bind: SocketAddr, dst: SocketAddr) -> bool {
    if bind.ip().is_unspecified() && bind.port() == dst.port() {
        return true;
    }

    bind == dst
}

/// Returns true if loopback is supported between two addresses, or
/// if the IPs are the same (in which case turmoil treats it like loopback)
pub(crate) fn is_same(src: SocketAddr, dst: SocketAddr) -> bool {
    dst.ip().is_loopback() || src.ip() == dst.ip()
}

#[cfg(test)]
mod test {
    use crate::{Host, Result};

    #[test]
    fn recycle_ports() -> Result {
        let mut host = Host::new(std::net::Ipv4Addr::UNSPECIFIED.into(), 1, 1);

        host.udp.bind((host.addr, 65534).into())?;
        host.udp.bind((host.addr, 65535).into())?;

        for _ in 49152..65534 {
            host.assign_ephemeral_port();
        }

        assert_eq!(49152, host.assign_ephemeral_port());

        Ok(())
    }
}
