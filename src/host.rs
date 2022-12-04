use crate::envelope::{hex, Datagram, Protocol, Segment, Syn};
use crate::net::{SocketPair, TcpListener, UdpSocket};
use crate::world::World;
use crate::{Envelope, TRACING_TARGET};

use bytes::{Buf, Bytes};
use indexmap::IndexMap;
use std::collections::VecDeque;
use std::fmt::Display;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use tokio::sync::{mpsc, Notify};
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

    /// Host elapsed time.
    elapsed: Duration,

    /// Set each time the software is run.
    now: Option<Instant>,
}

impl Host {
    pub(crate) fn new(addr: IpAddr) -> Host {
        Host {
            addr,
            udp: Udp::new(),
            tcp: Tcp::new(),
            next_ephemeral_port: 1024,
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

        tracing::trace!(target: TRACING_TARGET, ?dst, ?src, protocol = %message, "Delivered");

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

        tracing::info!(target: TRACING_TARGET, ?addr, protocol = %"UDP", "Bind");

        Ok(UdpSocket::new(addr, rx))
    }

    fn receive_from_network(&mut self, src: SocketAddr, dst: SocketAddr, datagram: Datagram) {
        if let Some(s) = self.binds.get_mut(&dst) {
            s.try_send((datagram, src))
                .unwrap_or_else(|_| panic!("unable to send to {}", dst))
        }
    }

    pub(crate) fn unbind(&mut self, addr: SocketAddr) {
        let exists = self.binds.remove(&addr);

        assert!(exists.is_some(), "unknown bind {}", addr);

        tracing::info!(target: TRACING_TARGET, ?addr, protocol = %"UDP", "Unbind");
    }
}

pub(crate) struct Tcp {
    /// Bound server sockets
    binds: IndexMap<SocketAddr, ServerSocket>,

    /// TcpListener channel capacity
    server_socket_capacity: usize,

    /// Active stream sockets
    sockets: IndexMap<SocketPair, StreamSocket>,
}

struct ServerSocket {
    /// Notify the TcpListener when SYNs are delivered
    notify: Arc<Notify>,

    /// Pending connections for the TcpListener to accept
    deque: VecDeque<(Syn, SocketAddr)>,
}

pub(crate) struct StreamSocket {
    buf: IndexMap<u64, SequencedSegment>,
    next_send_seq: u64,
    recv_seq: u64,
    rcv_buffer: VecDeque<SequencedSegment>,
    /// Waker from the last context that polled with read interest. This mirrors tokio's behaviour.
    reader: Option<Waker>,
    /// A simple reference counter for tracking read/write half drops. Once 0, the
    /// socket may be removed from the host.
    ref_ct: usize,
    closed: bool,
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
            SequencedSegment::Data(data) => hex("TCP", data, f),
            SequencedSegment::Fin => write!(f, "TCP FIN"),
        }
    }
}

impl StreamSocket {
    fn new() -> Self {
        let sock = Self {
            buf: IndexMap::new(),
            next_send_seq: 1,
            recv_seq: 0,
            ref_ct: 2,
            rcv_buffer: Default::default(),
            reader: None,
            closed: false,
        };

        sock
    }

    pub(crate) fn poll_read_ready(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.rcv_buffer.is_empty() && !self.closed {
            self.reader.replace(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }

    pub(crate) fn try_read(&mut self, buf: &mut [u8]) -> usize {
        let mut total_read = 0;
        while total_read < buf.len() {
            match self.rcv_buffer.pop_front() {
                Some(SequencedSegment::Data(mut data)) => {
                    let n = data.len().min(buf.len() - total_read);
                    buf[total_read..total_read + n].copy_from_slice(&data[..n]);
                    data.advance(n);
                    // buf is not large enough to contains all the data in the segment: re-enqueue it
                    // for later.
                    if data.has_remaining() {
                        self.rcv_buffer.push_front(SequencedSegment::Data(data));
                    }
                    total_read += n;
                }
                Some(SequencedSegment::Fin) => {
                    self.closed = true;
                    break;
                }
                None => break,
            }
        }
        total_read
    }

    fn assign_seq(&mut self) -> u64 {
        let seq = self.next_send_seq;
        self.next_send_seq += 1;
        seq
    }

    // Buffer and re-order received segments by `seq` as the network may deliver
    // them out of order.
    fn buffer(&mut self, seq: u64, segment: SequencedSegment) -> Result<(), Protocol> {
        let exists = self.buf.insert(seq, segment);

        assert!(exists.is_none(), "duplicate segment {}", seq);

        while self.buf.contains_key(&(self.recv_seq + 1)) {
            self.recv_seq += 1;

            let segment = self.buf.remove(&self.recv_seq).unwrap();
            self.rcv_buffer.push_back(segment);
        }

        if let Some(waker) = self.reader.take() {
            waker.wake();
        }

        Ok(())
    }
}

impl Tcp {
    fn new() -> Self {
        Self {
            binds: IndexMap::new(),
            sockets: IndexMap::new(),
            // TODO: Make capacity configurable
            server_socket_capacity: 64,
        }
    }

    fn is_port_assigned(&self, port: u16) -> bool {
        self.binds.keys().any(|a| a.port() == port)
            || self.sockets.keys().any(|a| a.local.port() == port)
    }

    pub(crate) fn bind(&mut self, addr: SocketAddr) -> io::Result<TcpListener> {
        let notify = Arc::new(Notify::new());
        let sock = ServerSocket {
            notify: notify.clone(),
            deque: VecDeque::new(),
        };

        if self.binds.insert(addr, sock).is_some() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, addr.to_string()));
        }

        tracing::info!(target: TRACING_TARGET, ?addr, protocol = %"TCP", "Bind");

        Ok(TcpListener::new(addr, notify))
    }

    pub(crate) fn new_stream(&mut self, pair: SocketPair) {
        let sock = StreamSocket::new();

        let exists = self.sockets.insert(pair, sock);

        assert!(exists.is_none(), "{:?} is already connected", pair);
    }

    pub(crate) fn accept(&mut self, addr: SocketAddr) -> Option<(Syn, SocketAddr)> {
        self.binds[&addr].deque.pop_front()
    }

    // Ideally, we could "write through" the tcp software, but this is necessary
    // due to borrowing the world to access the mut host and for sending.
    pub(crate) fn assign_send_seq(&mut self, pair: SocketPair) -> Option<u64> {
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
                    if b.deque.len() == self.server_socket_capacity {
                        todo!("{} server socket buffer full", dst);
                    }

                    b.deque.push_back((syn, src));
                    b.notify.notify_one();
                }
            }
            Segment::Data(seq, data) => match self.sockets.get_mut(&SocketPair::new(dst, src)) {
                Some(sock) => sock.buffer(seq, SequencedSegment::Data(data))?,
                None => return Err(Protocol::Tcp(Segment::Rst)),
            },
            Segment::Fin(seq) => match self.sockets.get_mut(&SocketPair::new(dst, src)) {
                Some(sock) => sock.buffer(seq, SequencedSegment::Fin)?,
                None => return Err(Protocol::Tcp(Segment::Rst)),
            },
            Segment::Rst => {
                if self.sockets.get(&SocketPair::new(dst, src)).is_some() {
                    self.sockets.remove(&SocketPair::new(dst, src)).unwrap();
                }
            }
        };

        Ok(())
    }

    pub(crate) fn close_stream_half(&mut self, pair: SocketPair) {
        // Receiving a RST removes the socket, so it's possible that has occured
        // when halfs of the stream drop.
        if let Some(sock) = self.sockets.get_mut(&pair) {
            sock.ref_ct -= 1;

            if sock.ref_ct == 0 {
                self.sockets.remove(&pair).unwrap();
            }
        }
    }

    pub(crate) fn unbind(&mut self, addr: SocketAddr) {
        let exists = self.binds.remove(&addr);

        assert!(exists.is_some(), "unknown bind {}", addr);

        tracing::info!(target: TRACING_TARGET, ?addr, protocol = %"TCP", "Unbind");
    }

    pub(crate) fn socket_mut(&mut self, pair: &SocketPair) -> Option<&mut StreamSocket> {
        self.sockets.get_mut(pair)
    }
}

#[cfg(test)]
mod test {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake};

    use bytes::Bytes;

    use crate::{host::SequencedSegment, Host, Result};

    use super::StreamSocket;

    #[test]
    fn recycle_ports() -> Result {
        let mut host = Host::new(std::net::Ipv4Addr::UNSPECIFIED.into());

        host.udp.bind((host.addr, 65534).into())?;
        host.udp.bind((host.addr, 65535).into())?;

        for _ in 1024..65534 {
            host.assign_ephemeral_port();
        }

        assert_eq!(1024, host.assign_ephemeral_port());

        Ok(())
    }
}
