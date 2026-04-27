//! Kernel socket types and the per-host socket table.

#![allow(dead_code, unused_variables)]

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::task::Waker;
use std::time::Duration;

use bytes::Bytes;

/// Linux default `ip_local_port_range`.
pub const DEFAULT_EPHEMERAL_PORTS: RangeInclusive<u16> = 49152..=65535;

/// Communication domain — POSIX `socket(2)`'s first argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Domain {
    /// IPv4 (`AF_INET`).
    Inet,
    /// IPv6 (`AF_INET6`).
    Inet6,
    /// Unix domain sockets (`AF_UNIX`).
    Unix,
}

/// Socket type — POSIX `socket(2)`'s second argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    /// Reliable, ordered, connection-oriented byte stream (`SOCK_STREAM`).
    Stream,
    /// Connectionless datagrams (`SOCK_DGRAM`).
    Dgram,
    /// Reliable, ordered, connection-oriented messages (`SOCK_SEQPACKET`).
    SeqPacket,
}

/// How to shut a connected socket down — `shutdown(2)`'s second argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown {
    Read,
    Write,
    Both,
}

/// Socket address. Unifies `AF_INET`/`AF_INET6` (via [`SocketAddr`]) and
/// `AF_UNIX` (via a path) so a single `bind`/`connect` surface works across
/// domains. Named [`Addr`] to avoid colliding with [`std::net::SocketAddr`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addr {
    Inet(SocketAddr),
    Unix(PathBuf),
}

/// Options. Each variant is tagged inline:
///
/// - **simulated** — observable in tests (affects send/recv/listen/close).
/// - **stored** — round-trips through get/set but doesn't change behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketOption {
    // ---- SOL_SOCKET ----
    /// `SO_BROADCAST`. simulated.
    Broadcast(bool),
    /// `SO_REUSEADDR`. simulated (affects bind conflict rules).
    ReuseAddr(bool),
    /// `SO_REUSEPORT`. simulated (load-balances accept across listeners).
    ReusePort(bool),
    /// `SO_LINGER`. simulated (controls close behavior for SOCK_STREAM).
    Linger(Option<Duration>),
    /// `SO_RCVBUF`. simulated (receive-side backpressure capacity).
    RecvBufferSize(usize),
    /// `SO_SNDBUF`. simulated (send-side backpressure capacity).
    SendBufferSize(usize),
    /// `SO_KEEPALIVE`. simulated (probe timer + dead-peer RST).
    KeepAlive(bool),

    // ---- IPPROTO_TCP ----
    /// `TCP_NODELAY`. stored (Nagle is not modeled).
    TcpNoDelay(bool),
    /// `TCP_KEEPIDLE`. simulated (idle time before first keepalive probe).
    TcpKeepIdle(Duration),
    /// `TCP_KEEPINTVL`. simulated (interval between keepalive probes).
    TcpKeepInterval(Duration),
    /// `TCP_KEEPCNT`. simulated (probe count before giving up).
    TcpKeepCount(u32),

    // ---- IPPROTO_IP ----
    /// `IP_TTL`. stored.
    IpTtl(u8),
    /// `IP_MULTICAST_TTL`. stored.
    IpMulticastTtl(u8),
    /// `IP_MULTICAST_LOOP`. simulated (whether sender sees own multicast).
    IpMulticastLoop(bool),
    /// `IP_ADD_MEMBERSHIP`. simulated.
    IpAddMembership { group: Ipv4Addr, iface: Ipv4Addr },
    /// `IP_DROP_MEMBERSHIP`. simulated.
    IpDropMembership { group: Ipv4Addr, iface: Ipv4Addr },

    // ---- IPPROTO_IPV6 ----
    /// `IPV6_V6ONLY`. simulated.
    Ipv6Only(bool),
    /// `IPV6_MULTICAST_HOPS`. stored.
    Ipv6MulticastHops(u8),
    /// `IPV6_MULTICAST_LOOP`. simulated.
    Ipv6MulticastLoop(bool),
    /// `IPV6_JOIN_GROUP`. simulated.
    Ipv6JoinGroup { group: Ipv6Addr, iface: u32 },
    /// `IPV6_LEAVE_GROUP`. simulated.
    Ipv6LeaveGroup { group: Ipv6Addr, iface: u32 },
}

/// Discriminant used to name which option to read back. Separate from
/// [`SocketOption`] because getters don't carry a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketOptionKind {
    Broadcast,
    ReuseAddr,
    ReusePort,
    Linger,
    RecvBufferSize,
    SendBufferSize,
    KeepAlive,
    TcpNoDelay,
    TcpKeepIdle,
    TcpKeepInterval,
    TcpKeepCount,
    IpTtl,
    IpMulticastTtl,
    IpMulticastLoop,
    Ipv6Only,
    Ipv6MulticastHops,
    Ipv6MulticastLoop,
}

/// A kernel socket handle. Opaque, fd-like, `Copy` — carries no state
/// on its own; all lookups go through the owning kernel's table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fd(u64);

/// Kernel-internal per-socket state. Tracks creation parameters, the
/// bound local address (if any), reuse-option state, the default peer
/// (for connected sockets), and the inbound datagram queue for
/// SOCK_DGRAM.
#[derive(Debug)]
pub struct Socket {
    pub domain: Domain,
    pub ty: Type,
    pub bound: Option<BindKey>,
    /// Default peer set by `connect(2)`. For UDP, filters inbound to
    /// datagrams from this peer and lets `send`/`recv` be used without
    /// `_to`/`_from`. For TCP this is the connected remote endpoint.
    pub peer: Option<Addr>,
    pub broadcast: bool,
    /// Datagrams queued for this socket, in arrival order. Only UDP
    /// populates this today. Bounded by `SO_RCVBUF` later.
    pub recv_queue: VecDeque<(Addr, Bytes)>,
    /// Tasks waiting in `poll_recv*` when the queue was empty. Multiple
    /// because `UdpSocket::{recv, recv_from}` take `&self` and can be
    /// polled from concurrent tasks.
    pub recv_wakers: Vec<Waker>,
}

impl Socket {
    pub fn new(domain: Domain, ty: Type) -> Self {
        Self {
            domain,
            ty,
            bound: None,
            peer: None,
            broadcast: false,
            recv_queue: VecDeque::new(),
            recv_wakers: Vec::new(),
        }
    }

    /// Register `waker` as interested in `recv` readiness, dedup'd
    /// against existing entries via `will_wake`.
    pub fn register_recv_waker(&mut self, waker: &Waker) {
        if self.recv_wakers.iter().any(|w| w.will_wake(waker)) {
            return;
        }
        self.recv_wakers.push(waker.clone());
    }

    /// Wake every task currently parked on `recv`.
    pub fn wake_recv(&mut self) {
        for w in self.recv_wakers.drain(..) {
            w.wake();
        }
    }
}

/// Binding key. Matches the granularity real kernels index on:
/// transport protocol, local address, local port. With `SO_REUSEPORT`,
/// multiple sockets can share one key — the table value is a list.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindKey {
    pub domain: Domain,
    pub ty: Type,
    pub local_addr: IpAddr,
    pub local_port: u16,
}

/// Per-host socket table.
#[derive(Debug)]
pub struct SocketTable {
    next_id: u64,
    sockets: HashMap<Fd, Socket>,
    /// Reverse index for inbound demux: bound tuple → sockets. List
    /// (not single `Fd`) because `SO_REUSEPORT` permits multiple
    /// sockets to share the exact same tuple.
    bindings: HashMap<BindKey, Vec<Fd>>,
    ports: PortAllocator,
}

impl SocketTable {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            sockets: HashMap::new(),
            bindings: HashMap::new(),
            ports: PortAllocator::new(DEFAULT_EPHEMERAL_PORTS),
        }
    }

    pub fn insert(&mut self, socket: Socket) -> Fd {
        let fd = Fd(self.next_id);
        self.next_id += 1;
        self.sockets.insert(fd, socket);
        fd
    }

    pub fn get(&self, fd: Fd) -> Option<&Socket> {
        self.sockets.get(&fd)
    }

    pub fn get_mut(&mut self, fd: Fd) -> Option<&mut Socket> {
        self.sockets.get_mut(&fd)
    }

    /// Remove a socket from the table, also clearing any binding that
    /// points at it.
    pub fn remove(&mut self, fd: Fd) -> Option<Socket> {
        self.bindings.retain(|_, fds| {
            fds.retain(|&f| f != fd);
            !fds.is_empty()
        });
        self.sockets.remove(&fd)
    }

    /// All sockets bound to `key`, in bind order.
    pub fn find_by_bind(&self, key: &BindKey) -> &[Fd] {
        self.bindings
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn insert_binding(&mut self, key: BindKey, fd: Fd) {
        self.bindings.entry(key).or_default().push(fd);
    }

    pub fn remove_binding(&mut self, key: &BindKey, fd: Fd) {
        if let Some(fds) = self.bindings.get_mut(key) {
            fds.retain(|&f| f != fd);
            if fds.is_empty() {
                self.bindings.remove(key);
            }
        }
    }

    /// Iterate all bindings on `(domain, ty, port)` regardless of local
    /// address. Used to evaluate wildcard↔specific conflicts at bind
    /// time.
    pub fn bindings_on_port(
        &self,
        domain: Domain,
        ty: Type,
        port: u16,
    ) -> impl Iterator<Item = (&BindKey, &[Fd])> {
        self.bindings
            .iter()
            .filter(move |(k, _)| k.domain == domain && k.ty == ty && k.local_port == port)
            .map(|(k, v)| (k, v.as_slice()))
    }

    /// Allocate an ephemeral port with no existing binding on
    /// `(domain, ty, port)` at any local address. Matches Linux's
    /// `inet_csk_get_port` for `:0` — it searches globally and skips
    /// any port already in use, without considering `SO_REUSE*`.
    pub fn allocate_port(&mut self, domain: Domain, ty: Type) -> Option<u16> {
        let bindings = &self.bindings;
        self.ports.allocate(|p| {
            bindings
                .keys()
                .any(|k| k.domain == domain && k.ty == ty && k.local_port == p)
        })
    }
}

impl Default for SocketTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Ephemeral port allocator. Linear scan with a rotating cursor.
#[derive(Debug)]
pub struct PortAllocator {
    range: RangeInclusive<u16>,
    cursor: u16,
}

impl PortAllocator {
    pub fn new(range: RangeInclusive<u16>) -> Self {
        let cursor = *range.start();
        Self { range, cursor }
    }

    /// Allocate the next free port, given a predicate that returns
    /// `true` when a port is already in use. Returns `None` if the
    /// range is exhausted.
    pub fn allocate(&mut self, mut in_use: impl FnMut(u16) -> bool) -> Option<u16> {
        let start = self.cursor;
        loop {
            let p = self.cursor;
            self.cursor = if p == *self.range.end() {
                *self.range.start()
            } else {
                p + 1
            };
            if !in_use(p) {
                return Some(p);
            }
            if self.cursor == start {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_allocator_skips_in_use() {
        let mut a = PortAllocator::new(10..=12);
        let used = [10u16];
        assert_eq!(a.allocate(|p| used.contains(&p)), Some(11));
        assert_eq!(a.allocate(|p| used.contains(&p)), Some(12));
    }

    #[test]
    fn port_allocator_exhausts() {
        let mut a = PortAllocator::new(10..=11);
        let used = [10u16, 11];
        assert_eq!(a.allocate(|p| used.contains(&p)), None);
    }

    #[test]
    fn fds_are_unique() {
        let mut t = SocketTable::new();
        let a = t.insert(Socket::new(Domain::Inet, Type::Dgram));
        let b = t.insert(Socket::new(Domain::Inet, Type::Dgram));
        assert_ne!(a, b);
    }

    #[test]
    fn bindings_roundtrip() {
        let mut t = SocketTable::new();
        let fd = t.insert(Socket::new(Domain::Inet, Type::Dgram));
        let key = BindKey {
            domain: Domain::Inet,
            ty: Type::Dgram,
            local_addr: "10.0.0.1".parse().unwrap(),
            local_port: 5000,
        };
        t.insert_binding(key.clone(), fd);
        assert_eq!(t.find_by_bind(&key), &[fd]);
        t.remove_binding(&key, fd);
        assert!(t.find_by_bind(&key).is_empty());
    }

    #[test]
    fn multiple_bindings_on_same_key() {
        let mut t = SocketTable::new();
        let a = t.insert(Socket::new(Domain::Inet, Type::Dgram));
        let b = t.insert(Socket::new(Domain::Inet, Type::Dgram));
        let key = BindKey {
            domain: Domain::Inet,
            ty: Type::Dgram,
            local_addr: "10.0.0.1".parse().unwrap(),
            local_port: 5000,
        };
        t.insert_binding(key.clone(), a);
        t.insert_binding(key.clone(), b);
        assert_eq!(t.find_by_bind(&key), &[a, b]);
    }
}
