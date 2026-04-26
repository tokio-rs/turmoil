//! Kernel socket API.
//!
//! One [`Socket`] type, syscall-shaped methods. Non-blocking ops are exposed
//! as `poll_*`; synchronous ops (`bind`, `listen`, `local_addr`, option
//! get/set) complete immediately. `close` is `Drop`.
//!
//! This module defines the surface only — all method bodies are
//! `unimplemented!()`. UDP is the first protocol slated for implementation.

#![allow(dead_code, unused_variables)]

use std::io::Result;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::ReadBuf;

/// Communication domain — POSIX `socket(2)`'s first argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    /// IPv4 (`AF_INET`).
    Inet,
    /// IPv6 (`AF_INET6`).
    Inet6,
    /// Unix domain sockets (`AF_UNIX`).
    Unix,
}

/// Socket type — POSIX `socket(2)`'s second argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Options passed to [`Socket::set_option`] / returned from
/// [`Socket::get_option`]. Behavior of each variant is tagged inline:
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
    IpAddMembership {
        group: Ipv4Addr,
        iface: Ipv4Addr,
    },
    /// `IP_DROP_MEMBERSHIP`. simulated.
    IpDropMembership {
        group: Ipv4Addr,
        iface: Ipv4Addr,
    },

    // ---- IPPROTO_IPV6 ----
    /// `IPV6_V6ONLY`. simulated.
    Ipv6Only(bool),
    /// `IPV6_MULTICAST_HOPS`. stored.
    Ipv6MulticastHops(u8),
    /// `IPV6_MULTICAST_LOOP`. simulated.
    Ipv6MulticastLoop(bool),
    /// `IPV6_JOIN_GROUP`. simulated.
    Ipv6JoinGroup {
        group: Ipv6Addr,
        iface: u32,
    },
    /// `IPV6_LEAVE_GROUP`. simulated.
    Ipv6LeaveGroup {
        group: Ipv6Addr,
        iface: u32,
    },
}

/// Discriminant used with [`Socket::get_option`] to name which option to
/// read back. Separate from [`SocketOption`] because getters don't carry a
/// value.
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

/// A kernel socket handle. Cheap to clone (internally `Arc`-backed).
///
/// Close is `Drop` — matches fd lifetime semantics.
#[derive(Debug)]
pub struct Socket {
    // TODO: Arc<SocketEntry> into the per-host socket table.
    _priv: (),
}

impl Socket {
    // ---- syscall-shaped constructors and blocking ops ----

    /// `socket(2)`.
    pub fn new(domain: Domain, ty: Type) -> Result<Self> {
        unimplemented!()
    }

    /// `bind(2)`.
    pub fn bind(&self, addr: &Addr) -> Result<()> {
        unimplemented!()
    }

    /// `listen(2)`.
    pub fn listen(&self, backlog: u32) -> Result<()> {
        unimplemented!()
    }

    /// `getsockname(2)`.
    pub fn local_addr(&self) -> Result<Addr> {
        unimplemented!()
    }

    /// `getpeername(2)`.
    pub fn peer_addr(&self) -> Result<Addr> {
        unimplemented!()
    }

    /// `shutdown(2)`.
    pub fn shutdown(&self, how: Shutdown) -> Result<()> {
        unimplemented!()
    }

    /// `setsockopt(2)`.
    pub fn set_option(&self, opt: SocketOption) -> Result<()> {
        unimplemented!()
    }

    /// `getsockopt(2)`. Returns the [`SocketOption`] variant matching `kind`
    /// so the value type is carried alongside the option identity.
    pub fn get_option(&self, kind: SocketOptionKind) -> Result<SocketOption> {
        unimplemented!()
    }

    // ---- async-shaped (poll) ops ----

    /// `connect(2)`. Completes when the connection is established (for
    /// stream sockets) or immediately after setting the default peer (for
    /// datagram sockets).
    pub fn poll_connect(&self, cx: &mut Context<'_>, addr: &Addr) -> Poll<Result<()>> {
        unimplemented!()
    }

    /// `accept(2)`.
    pub fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<Result<(Socket, Addr)>> {
        unimplemented!()
    }

    /// `send(2)` — for connected sockets.
    pub fn poll_send(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        unimplemented!()
    }

    /// `sendto(2)` — for unconnected datagram sockets.
    pub fn poll_send_to(
        &self,
        cx: &mut Context<'_>,
        buf: &[u8],
        addr: &Addr,
    ) -> Poll<Result<usize>> {
        unimplemented!()
    }

    /// `recv(2)`.
    pub fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<Result<()>> {
        unimplemented!()
    }

    /// `recvfrom(2)`. Returns the peer address alongside the filled buffer.
    pub fn poll_recv_from(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<Addr>> {
        unimplemented!()
    }

    /// `recv(2)` with `MSG_PEEK` — non-destructive read.
    pub fn poll_peek(&self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<Result<()>> {
        unimplemented!()
    }

    /// `recvfrom(2)` with `MSG_PEEK`.
    pub fn poll_peek_from(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<Addr>> {
        unimplemented!()
    }
}
