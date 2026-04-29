//! Kernel socket types and the per-host socket table.

#![allow(dead_code, unused_variables)]

use std::collections::VecDeque;

use indexmap::IndexMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::task::Waker;
use std::time::Duration;

use bytes::{Bytes, BytesMut};

/// Linux default `ip_local_port_range`.
pub const DEFAULT_EPHEMERAL_PORTS: RangeInclusive<u16> = 49152..=65535;

/// Default per-socket TCP send buffer cap (bytes). Bounds the sum of
/// queued-but-unsent and sent-but-unACK'd data, matching Linux
/// `SO_SNDBUF` semantics. Overridable via [`KernelConfig`].
pub const DEFAULT_SEND_BUF_CAP: usize = 64 * 1024;

/// Default per-socket TCP receive buffer cap (bytes). Advertised as
/// the TCP window in outgoing segments. Overridable via
/// [`KernelConfig`].
pub const DEFAULT_RECV_BUF_CAP: usize = 64 * 1024;

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
    /// `TCP_NODELAY`. stored — we don't model Nagle at all (every
    /// `poll_write` copies to `send_buf`, `segment_all` emits
    /// immediately), so this round-trips without affecting traffic.
    /// TODO: if we ever model Nagle, gate segmentation on `!nodelay`
    /// and coalesce sub-MSS payloads the way real TCP does.
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

/// TCP connection state.
///
/// We intentionally skip `TimeWait` and go straight from
/// `FinWait2`/`LastAck` to `Closed`. Its two jobs in real TCP — late-
/// duplicate protection on 4-tuple reuse and re-ACKing a retransmitted
/// FIN — only matter on an unreliable transport. The simulated fabric
/// doesn't lose or duplicate packets, and we have no simulated clock
/// to run a 2×MSL timer against.
///
/// Once the fabric grows drop/duplicate faults or a virtual clock,
/// revisit this: `TimeWait` is additive (new state + timer) and won't
/// break existing transitions. The other user-visible effect — that
/// `bind()` to a recently-closed 4-tuple returns `EADDRINUSE` without
/// `SO_REUSEADDR` — isn't modeled either, and can be bolted on via a
/// "recently closed" bit on the bind table without a full state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    SynSent,
    SynReceived,
    Established,
    /// We sent FIN, waiting for its ACK and/or the peer's FIN. From
    /// here either `FinWait2` (peer ACK'd our FIN) or `Closing` (peer
    /// sent FIN before ACKing ours).
    FinWait1,
    /// Peer ACK'd our FIN; still waiting for the peer's FIN.
    FinWait2,
    /// Peer sent FIN; we've ACK'd it and may still write until the app
    /// calls `shutdown(Write)` or closes.
    CloseWait,
    /// Both sides have sent FIN; we're waiting for the peer's ACK of
    /// ours.
    LastAck,
    /// Simultaneous close — we sent FIN, then received theirs before
    /// ours was ACK'd. Waiting for our FIN's ACK.
    Closing,
    /// Terminal. Resources can be reaped.
    Closed,
}

/// TCP control block. Present on a socket once it's paired with a peer
/// (either via `connect` or `accept`). Mirrors the subset of fields a
/// real TCB needs for the pieces we model today; close-state
/// bookkeeping (`FIN`/`RST` states, `TIME-WAIT`) comes later.
///
/// `send_buf` layout — `[already-ACK'd drained | in-flight | queued]`:
/// - Bytes 0..(snd_nxt - snd_una) are on the wire, unACK'd. Kept so we
///   could retransmit, though v1's fabric is reliable.
/// - Bytes (snd_nxt - snd_una).. are queued for the next egress pass.
/// - ACK'd bytes are `advance()`d off the front.
#[derive(Debug)]
pub struct Tcb {
    pub state: TcpState,
    /// Remote endpoint. Mirrored in `Socket::peer` for UDP parity.
    pub peer: SocketAddr,
    /// Next sequence number we'll send (advances as egress chops and
    /// emits segments).
    pub snd_nxt: u32,
    /// Oldest unACK'd sequence number. `snd_nxt - snd_una` == in-flight.
    pub snd_una: u32,
    /// Peer's last-advertised receive window, in bytes. Bounds how far
    /// beyond `snd_una` we're allowed to push `snd_nxt` before pausing.
    pub snd_wnd: u16,
    /// Next sequence number we expect to receive.
    pub rcv_nxt: u32,
    /// Outgoing byte queue — see type-level doc for layout.
    pub send_buf: BytesMut,
    /// Incoming byte queue. App drains via `poll_read`; advertised
    /// window shrinks as this fills.
    pub recv_buf: BytesMut,
    /// App has closed the write side (FIN sent). Further `poll_write`
    /// calls return `BrokenPipe`.
    pub wr_closed: bool,
    /// Peer has sent FIN. Readers see `Ready(0)` after `recv_buf`
    /// drains.
    pub peer_fin: bool,
    /// Sequence number occupied by our FIN — ACK of `fin_seq + 1`
    /// confirms the peer received it. `None` until we emit FIN.
    pub fin_seq: Option<u32>,
    /// Connection was aborted by a RST (sent or received). Further ops
    /// return `ConnectionReset`; distinguishes the error from a clean
    /// `Closed` transition.
    pub reset: bool,
}

/// Listener state. Attached to a socket by `listen(2)`.
#[derive(Debug)]
pub struct ListenState {
    /// Configured backlog. Real Linux clamps to `somaxconn`; we don't.
    pub backlog: usize,
    /// Fully-established connections waiting to be `accept`-ed. Each
    /// entry is the server-side Fd of a finished handshake.
    pub ready: VecDeque<Fd>,
    /// Tasks parked in `poll_accept`.
    pub accept_wakers: Vec<Waker>,
}

impl ListenState {
    pub fn new(backlog: usize) -> Self {
        Self {
            backlog,
            ready: VecDeque::new(),
            accept_wakers: Vec::new(),
        }
    }
}

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
    /// `IP_TTL` — stored only; not applied to simulated packets yet.
    pub ttl: u8,
    /// `TCP_NODELAY` — stored only. We don't model Nagle at all, so
    /// reading it back round-trips whatever the app set. See the
    /// `TcpNoDelay` variant on [`SocketOption`] for the TODO.
    pub tcp_nodelay: bool,
    /// Datagrams queued for this socket, in arrival order. Only UDP
    /// populates this today. Bounded by `SO_RCVBUF` later.
    pub recv_queue: VecDeque<(Addr, Bytes)>,
    /// Tasks waiting in `poll_recv*` when the queue was empty. Multiple
    /// because `UdpSocket::{recv, recv_from}` take `&self` and can be
    /// polled from concurrent tasks.
    pub recv_wakers: Vec<Waker>,
    /// TCP control block — populated on connect (client) or accept
    /// (server) once a socket is paired with a peer.
    pub tcb: Option<Tcb>,
    /// Listener state — populated by `listen(2)`.
    pub listen: Option<ListenState>,
    /// Waker for a `connect` parked in `SynSent` waiting for SYN-ACK.
    pub connect_waker: Option<Waker>,
    /// Waker for `poll_read` parked on an empty `recv_buf`. Fired when
    /// bytes arrive via `deliver`, or when FIN closes the read side.
    pub read_waker: Option<Waker>,
    /// Waker for `poll_write` parked on a full `send_buf`. Fired when
    /// ACK drains in-flight bytes and frees capacity.
    pub write_waker: Option<Waker>,
    /// The shim dropped its `Fd` handle, but the kernel is still
    /// draining queued bytes / FIN / waiting for a final ACK. The
    /// socket is reaped from the table once TCP reaches a terminal
    /// state. Behaves like Linux's `close(2)` — returns immediately
    /// but the kernel keeps running the state machine.
    pub fd_closed: bool,
}

impl Socket {
    pub fn new(domain: Domain, ty: Type) -> Self {
        Self {
            domain,
            ty,
            bound: None,
            peer: None,
            broadcast: false,
            ttl: 64,
            tcp_nodelay: false,
            recv_queue: VecDeque::new(),
            recv_wakers: Vec::new(),
            tcb: None,
            listen: None,
            connect_waker: None,
            read_waker: None,
            write_waker: None,
            fd_closed: false,
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
    sockets: IndexMap<Fd, Socket>,
    /// Reverse index for inbound demux: bound tuple → sockets. List
    /// (not single `Fd`) because `SO_REUSEPORT` permits multiple
    /// sockets to share the exact same tuple.
    bindings: IndexMap<BindKey, Vec<Fd>>,
    /// 4-tuple index for TCP inbound demux: (local, remote) → fd.
    /// Populated by `poll_connect` and `accept_syn`, cleared on
    /// socket removal. Never contains wildcard locals — client
    /// sockets auto-bind concretely, and listener children inherit
    /// the concrete accepted local address.
    connections: IndexMap<(SocketAddr, SocketAddr), Fd>,
    ports: PortAllocator,
}

impl SocketTable {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            sockets: IndexMap::new(),
            bindings: IndexMap::new(),
            connections: IndexMap::new(),
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

    pub fn iter(&self) -> impl Iterator<Item = (Fd, &Socket)> {
        self.sockets.iter().map(|(&fd, s)| (fd, s))
    }

    pub fn get_mut(&mut self, fd: Fd) -> Option<&mut Socket> {
        self.sockets.get_mut(&fd)
    }

    /// Remove a socket from the table, also clearing any binding or
    /// 4-tuple connection entry that points at it.
    pub fn remove(&mut self, fd: Fd) -> Option<Socket> {
        self.bindings.retain(|_, fds| {
            fds.retain(|&f| f != fd);
            !fds.is_empty()
        });
        self.connections.retain(|_, f| *f != fd);
        self.sockets.shift_remove(&fd)
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
                self.bindings.shift_remove(key);
            }
        }
    }

    /// Index a TCP connection by 4-tuple. Panics if `local` is
    /// unspecified — the index invariant requires concrete addrs.
    pub fn insert_connection(&mut self, local: SocketAddr, remote: SocketAddr, fd: Fd) {
        assert!(
            !local.ip().is_unspecified(),
            "connection index requires concrete local addr"
        );
        self.connections.insert((local, remote), fd);
    }

    /// Look up a connected socket by its 4-tuple.
    pub fn find_connection(&self, local: SocketAddr, remote: SocketAddr) -> Option<Fd> {
        self.connections.get(&(local, remote)).copied()
    }

    /// Iterate connections matching `local` — any remote. Used to
    /// count in-flight children of a listener.
    pub fn connections_on(
        &self,
        local: SocketAddr,
    ) -> impl Iterator<Item = (SocketAddr, Fd)> + '_ {
        self.connections
            .iter()
            .filter(move |((l, _), _)| *l == local)
            .map(|((_, r), fd)| (*r, *fd))
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
