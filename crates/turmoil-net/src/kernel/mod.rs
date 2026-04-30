//! Per-host network stack.
//!
//! The [`Kernel`] owns one host's socket table, packet queues, and
//! interface configuration. It does not route between hosts — that's a
//! concern for the transport layer above.
//!
//! Mutability and thread-safety aren't yet decided — the kernel is a
//! plain struct with `&mut self` on mutating methods. Sharing and
//! interior mutability (`RefCell`, `Arc<Mutex<_>>`, thread-local, etc.)
//! will be layered on later once the transport story is clearer.

use std::collections::VecDeque;
use std::io::{Error, ErrorKind};
use std::net::{IpAddr, SocketAddr};
use std::task::{Context, Poll};

use tokio::io::ReadBuf;

use crate::kernel::socket::{BindKey, SocketTable, DEFAULT_RECV_BUF_CAP, DEFAULT_SEND_BUF_CAP};

mod packet;
mod socket;
mod tcp;
mod udp;
mod uds;

// for shims
pub use socket::{Addr, Domain, Fd, SocketOption, SocketOptionKind, Type};
// for netstat
pub use socket::{ListenState, Socket, Tcb, TcpState};
// for rules
pub use packet::{Packet, TcpFlags, TcpSegment, Transport, UdpDatagram};

// TODO: cooperative yielding on loopback.
//
// `egress()` folds loopback packets straight back through `deliver`
// inline — there's no scheduler step between them. A chatty protocol
// running over `127.0.0.1` (RPC ping/pong, TCP handshake under load,
// client+server both on one host) can keep egress-then-deliver going
// indefinitely without ever handing control back to tokio, starving
// timers, other tasks, and the test's own `step()` loop.
//
// Two plausible fixes:
//   1. Cap `egress()` — deliver up to N loopback packets, park the
//      rest on `outbound` for the next pump. Cheap, but defines a
//      somewhat arbitrary boundary.
//   2. Route loopback through `outbound` → the fabric just like
//      non-local traffic, and make the fabric responsible for pacing.
//      More principled once the fabric exists.
//
// Not urgent while nothing loops tightly, but pick an answer before
// we write tests that rely on back-and-forth request/response.

// TODO: SYN retransmit.
//
// A SYN dropped by the listener's backlog cap (or, eventually, by a
// link filter) is lost forever — the connecting socket sits in
// SynSent with no retry. Real TCP retransmits SYN with exponential
// backoff (Linux: tcp_syn_retries, default 6). We should:
//   1. Stash a retransmit deadline on the TCB when SYN is emitted.
//   2. On each egress pass (or a dedicated timer tick), re-emit SYN
//      if the deadline has passed and we're still in SynSent.
//   3. Give up after N attempts, surface as ConnectionRefused /
//      TimedOut.
// Same machinery will eventually cover data retransmit.

/// Default MTU for non-loopback traffic (bytes). Matches standard Ethernet.
pub const DEFAULT_MTU: u32 = 1500;
/// Default MTU for loopback (bytes). Matches Linux `lo`.
pub const DEFAULT_LOOPBACK_MTU: u32 = 65536;
/// Default backlog for `TcpListener::bind` when the caller doesn't
/// specify one. Mirrors Linux's `SOMAXCONN`.
pub const DEFAULT_BACKLOG: usize = 1024;

/// Tunable limits for a [`Kernel`]. Constructed via [`Self::default`]
/// and adjusted with the builder-style setters. Pass to
/// `Net::with_config` to apply.
#[derive(Debug, Clone)]
pub struct KernelConfig {
    pub mtu: u32,
    pub loopback_mtu: u32,
    pub send_buf_cap: usize,
    pub recv_buf_cap: usize,
    pub default_backlog: usize,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            mtu: DEFAULT_MTU,
            loopback_mtu: DEFAULT_LOOPBACK_MTU,
            send_buf_cap: DEFAULT_SEND_BUF_CAP,
            recv_buf_cap: DEFAULT_RECV_BUF_CAP,
            default_backlog: DEFAULT_BACKLOG,
        }
    }
}

impl KernelConfig {
    pub fn mtu(mut self, v: u32) -> Self {
        self.mtu = v;
        self
    }
    pub fn loopback_mtu(mut self, v: u32) -> Self {
        self.loopback_mtu = v;
        self
    }
    pub fn send_buf_cap(mut self, v: usize) -> Self {
        self.send_buf_cap = v;
        self
    }
    pub fn recv_buf_cap(mut self, v: usize) -> Self {
        self.recv_buf_cap = v;
        self
    }
    pub fn default_backlog(mut self, v: usize) -> Self {
        self.default_backlog = v;
        self
    }
}

/// Linux errno for `EAFNOSUPPORT`. Used where we want `kind() ==
/// Uncategorized` to match what `tokio::net` surfaces for
/// socket-family mismatches (the `Uncategorized` variant itself is
/// unstable). `from_raw_os_error` on any platform we care about maps
/// this to `Uncategorized`.
const EAFNOSUPPORT: i32 = 97;

/// Linux errno for `EMSGSIZE`. Surfaced by UDP `sendto` when the
/// payload exceeds the link MTU under `IP_PMTUDISC_DO` semantics
/// (real Linux; `ErrorKind::FileTooLarge` is unstable).
pub(crate) const EMSGSIZE: i32 = 90;

/// A per-host network stack.
///
/// Owns the socket table and the inbound/outbound packet queues. Does
/// not know how packets reach other hosts — that's the transport
/// layer's job.
#[derive(Debug)]
pub struct Kernel {
    sockets: SocketTable,
    addresses: Vec<IpAddr>,
    pub(crate) mtu: u32,
    pub(crate) loopback_mtu: u32,
    pub(crate) send_buf_cap: usize,
    pub(crate) recv_buf_cap: usize,
    pub(crate) default_backlog: usize,
    /// Packets queued by `poll_send_*` awaiting `egress()`.
    outbound: VecDeque<Packet>,
    /// Monotonic TCP initial-sequence-number source. Deterministic by
    /// design — real kernels randomize.
    tcp_isn: u32,
}

impl Kernel {
    /// Construct a fresh kernel with default settings and no
    /// configured addresses beyond implicit loopback.
    pub fn new() -> Self {
        Self::with_config(KernelConfig::default())
    }

    pub fn with_config(cfg: KernelConfig) -> Self {
        Self {
            sockets: SocketTable::new(),
            addresses: Vec::new(),
            mtu: cfg.mtu,
            loopback_mtu: cfg.loopback_mtu,
            send_buf_cap: cfg.send_buf_cap,
            recv_buf_cap: cfg.recv_buf_cap,
            default_backlog: cfg.default_backlog,
            outbound: VecDeque::new(),
            tcp_isn: 0x0100_0000,
        }
    }

    /// Create a new socket in this kernel's socket table.
    fn mk_socket(&mut self, domain: Domain, ty: Type) -> Fd {
        self.sockets.insert(Socket::new(domain, ty))
    }

    /// `socket(2)`. Creates an unbound socket; useful for
    /// `connect`-without-`bind` flows like `TcpStream::connect`.
    pub fn open(&mut self, domain: Domain, ty: Type) -> Fd {
        self.mk_socket(domain, ty)
    }

    /// `close(2)`. Removes the entry from the socket table along with
    /// any binding. For an active TCP connection, decides between a
    /// clean close (silent) and an abortive close (emits RST):
    ///
    /// - RST if the app drops with unread bytes still queued in
    ///   `recv_buf`, or with a live write side that never sent FIN.
    ///   Matches Linux behavior and the `tokio::net::TcpStream` Drop
    ///   semantics the upstream crate recently fixed.
    /// - Silent close otherwise — the connection already reached a
    ///   clean terminal state via FIN exchange.
    ///
    /// Idempotent on an already-closed fd.
    pub fn close(&mut self, fd: Fd) {
        if tcp::on_close(self, fd) {
            self.sockets.remove(fd);
        }
        // else: lingering — `reap_closed` at the end of each egress
        // pass will clean up once the TCP state reaches `Closed`.
    }

    /// Iterate every socket in the table, in insertion order.
    pub fn sockets(&self) -> impl Iterator<Item = (Fd, &Socket)> {
        self.sockets.iter()
    }

    pub(crate) fn lookup(&self, fd: Fd) -> std::io::Result<&Socket> {
        self.sockets
            .get(fd)
            .ok_or_else(|| Error::from(ErrorKind::NotFound))
    }

    pub(crate) fn lookup_mut(&mut self, fd: Fd) -> std::io::Result<&mut Socket> {
        self.sockets
            .get_mut(fd)
            .ok_or_else(|| Error::from(ErrorKind::NotFound))
    }

    /// Configure an additional local address for this host. Loopback
    /// (`127.0.0.1`, `::1`) is always implicit.
    pub fn add_address(&mut self, addr: IpAddr) {
        if !self.addresses.contains(&addr) {
            self.addresses.push(addr);
        }
    }

    /// `true` if `addr` is one of this host's local addresses
    /// (including implicit loopback).
    pub fn is_local(&self, addr: IpAddr) -> bool {
        if addr.is_loopback() {
            return true;
        }
        self.addresses.contains(&addr)
    }

    /// `socket(2)` + `bind(2)`. Creates a socket of the given type,
    /// assigns it `addr`, and returns the handle. Domain is inferred
    /// from `addr`.
    pub fn bind(&mut self, addr: &Addr, ty: Type) -> std::io::Result<Fd> {
        let (domain, ip, port) = match addr {
            Addr::Inet(sa) if sa.is_ipv4() => (Domain::Inet, sa.ip(), sa.port()),
            Addr::Inet(sa) => (Domain::Inet6, sa.ip(), sa.port()),
            Addr::Unix(_) => unimplemented!("AF_UNIX bind"),
        };

        // Non-wildcard IPs must be configured on this host.
        if !ip.is_unspecified() && !self.is_local(ip) {
            return Err(Error::from(ErrorKind::AddrNotAvailable));
        }

        // Pick a port if the caller asked for one.
        let port = if port == 0 {
            self.sockets
                .allocate_port(domain, ty)
                .ok_or_else(|| Error::from(ErrorKind::AddrInUse))?
        } else {
            port
        };

        let key = BindKey {
            domain,
            ty,
            local_addr: ip,
            local_port: port,
        };

        // Walk every existing binding on the same (domain, ty, port) to
        // evaluate conflicts. Two scenarios:
        //
        // 1. Exact-tuple match. Allowed only if every socket in the
        //    existing group AND the new socket has SO_REUSEPORT set.
        // 2. Wildcard-vs-specific mismatch (one side bound to 0.0.0.0/::,
        //    the other to a concrete IP). Allowed only if both sides
        //    have SO_REUSEADDR set.
        //
        // Distinct concrete IPs on the same port never conflict on
        // Linux — those just coexist regardless of options.
        //
        // Socket defaults (reuse_addr, reuse_port) are both `false`, so
        // a freshly created socket never satisfies either reuse
        // condition — that means any overlap at this step is a
        // conflict and we can bail before creating the table entry.
        for (existing, _ids) in self
            .sockets
            .bindings_on_port(key.domain, key.ty, key.local_port)
        {
            if existing.local_addr == key.local_addr
                || existing.local_addr.is_unspecified()
                || key.local_addr.is_unspecified()
            {
                return Err(Error::from(ErrorKind::AddrInUse));
            }
        }

        let fd = self.mk_socket(domain, ty);
        self.sockets.insert_binding(key.clone(), fd);
        self.lookup_mut(fd).expect("socket entry present").bound = Some(key);
        Ok(fd)
    }

    /// `setsockopt(2)`. Most variants are still unimplemented.
    pub fn set_option(&mut self, fd: Fd, opt: SocketOption) -> std::io::Result<()> {
        let st = self.lookup_mut(fd)?;
        match opt {
            SocketOption::Broadcast(v) => st.broadcast = v,
            SocketOption::IpTtl(v) => st.ttl = v,
            SocketOption::TcpNoDelay(v) => st.tcp_nodelay = v,
            _ => unimplemented!("set_option {:?}", opt),
        }
        Ok(())
    }

    /// `getsockopt(2)`. Most variants are still unimplemented.
    pub fn get_option(&self, fd: Fd, kind: SocketOptionKind) -> std::io::Result<SocketOption> {
        let st = self.lookup(fd)?;
        Ok(match kind {
            SocketOptionKind::Broadcast => SocketOption::Broadcast(st.broadcast),
            SocketOptionKind::IpTtl => SocketOption::IpTtl(st.ttl),
            SocketOptionKind::TcpNoDelay => SocketOption::TcpNoDelay(st.tcp_nodelay),
            _ => unimplemented!("get_option {:?}", kind),
        })
    }

    /// `getsockname(2)`.
    pub fn local_addr(&self, fd: Fd) -> std::io::Result<Addr> {
        let key = self
            .lookup(fd)?
            .bound
            .as_ref()
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        Ok(Addr::Inet(SocketAddr::new(key.local_addr, key.local_port)))
    }

    /// `getpeername(2)`. Returns `NotConnected` if `connect` was never
    /// called.
    pub fn peer_addr(&self, fd: Fd) -> std::io::Result<Addr> {
        self.lookup(fd)?
            .peer
            .clone()
            .ok_or_else(|| Error::from(ErrorKind::NotConnected))
    }

    /// `sendto(2)` for UDP. Auto-binds an ephemeral local address if
    /// `socket` was never explicitly bound, matching Linux semantics.
    pub fn poll_send_to(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
        buf: &[u8],
        dst: &Addr,
    ) -> Poll<std::io::Result<usize>> {
        let Addr::Inet(dst_sa) = dst else {
            panic!("AF_UNIX not wired through poll_send_to");
        };
        let (ty, domain) = match self.lookup(fd) {
            Ok(st) => (st.ty, st.domain),
            Err(e) => return Poll::Ready(Err(e)),
        };
        assert_eq!(ty, Type::Dgram, "poll_send_to on non-Dgram fd");
        match (domain, dst_sa) {
            (Domain::Inet, SocketAddr::V4(_)) | (Domain::Inet6, SocketAddr::V6(_)) => {}
            _ => return Poll::Ready(Err(Error::from_raw_os_error(EAFNOSUPPORT))),
        }
        udp::send_to(self, fd, cx, buf, dst_sa)
    }

    /// `recvfrom(2)`. Returns the peer address alongside the filled
    /// buffer. Returns `Pending` and stores the waker when the
    /// socket's recv queue is empty.
    pub fn poll_recv_from(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<Addr>> {
        let st = match self.lookup_mut(fd) {
            Ok(st) => st,
            Err(e) => return Poll::Ready(Err(e)),
        };
        assert_eq!(st.ty, Type::Dgram, "poll_recv_from on non-Dgram fd");
        udp::recv_from(st, cx, buf)
    }

    /// `connect(2)`. UDP is eager (no handshake, always `Ready` on
    /// first poll). TCP is real: first poll sends SYN and parks; later
    /// polls finish once the handshake completes.
    pub fn poll_connect(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
        addr: &Addr,
    ) -> Poll<std::io::Result<()>> {
        let Addr::Inet(peer) = addr else {
            panic!("AF_UNIX not wired through connect");
        };
        let (domain, ty, is_bound) = match self.lookup(fd) {
            Ok(st) => (st.domain, st.ty, st.bound.is_some()),
            Err(e) => return Poll::Ready(Err(e)),
        };
        match (domain, peer) {
            (Domain::Inet, SocketAddr::V4(_)) | (Domain::Inet6, SocketAddr::V6(_)) => {}
            _ => return Poll::Ready(Err(Error::from_raw_os_error(EAFNOSUPPORT))),
        }
        match ty {
            Type::Dgram => {
                if !is_bound {
                    if let Err(e) = udp::auto_bind(self, fd, domain, ty, peer.ip()) {
                        return Poll::Ready(Err(e));
                    }
                }
                self.lookup_mut(fd).expect("socket present").peer = Some(Addr::Inet(*peer));
                Poll::Ready(Ok(()))
            }
            Type::Stream => tcp::poll_connect(self, fd, cx, domain, *peer, is_bound),
            Type::SeqPacket => unimplemented!("SOCK_SEQPACKET connect"),
        }
    }

    /// `listen(2)`. Flips a bound stream socket into passive mode with
    /// the given backlog. Panics if called on a non-Stream or unbound
    /// fd — the shim only calls this after a successful `bind`, so
    /// either is an internal bug.
    pub fn listen(&mut self, fd: Fd, backlog: usize) -> std::io::Result<()> {
        let st = self.lookup_mut(fd)?;
        assert_eq!(st.ty, Type::Stream, "listen on non-Stream fd");
        assert!(st.bound.is_some(), "listen on unbound fd");
        st.listen = Some(ListenState::new(backlog));
        Ok(())
    }

    /// `accept(2)`. Pops a fully-established connection off the
    /// listener's ready queue; parks the caller if the queue is empty.
    /// Panics if called on a socket that isn't listening — structural
    /// guarantee from the `TcpListener` shim.
    pub fn poll_accept(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<(Fd, SocketAddr)>> {
        let st = match self.lookup_mut(fd) {
            Ok(st) => st,
            Err(e) => return Poll::Ready(Err(e)),
        };
        let listen = st.listen.as_mut().expect("poll_accept on non-listener fd");
        if let Some(child) = listen.ready.pop_front() {
            let peer = self
                .lookup(child)
                .expect("accepted fd present")
                .tcb
                .as_ref()
                .expect("accepted fd has TCB")
                .peer;
            return Poll::Ready(Ok((child, peer)));
        }
        if !listen.accept_wakers.iter().any(|w| w.will_wake(cx.waker())) {
            listen.accept_wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }

    /// `send(2)` for connected sockets. UDP: resolves the stored peer
    /// and delegates to `sendto`. TCP: copies into `send_buf` and
    /// lets `egress` segment and emit.
    pub fn poll_send(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let (ty, peer) = match self.lookup(fd) {
            Ok(st) => (st.ty, st.peer.clone()),
            Err(e) => return Poll::Ready(Err(e)),
        };
        match ty {
            Type::Dgram => {
                let Some(peer) = peer else {
                    return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
                };
                let Addr::Inet(peer_sa) = peer else {
                    panic!("UDP peer stored as Addr::Unix");
                };
                udp::send_to(self, fd, cx, buf, &peer_sa)
            }
            Type::Stream => tcp::poll_send(self, fd, cx, buf),
            Type::SeqPacket => unimplemented!("SOCK_SEQPACKET poll_send"),
        }
    }

    /// `recv(2)` for connected sockets. UDP: pops one datagram into
    /// `buf`. TCP: drains from `recv_buf`.
    pub fn poll_recv(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let ty = match self.lookup(fd) {
            Ok(st) => st.ty,
            Err(e) => return Poll::Ready(Err(e)),
        };
        match ty {
            Type::Dgram => {
                let st = self.lookup_mut(fd).expect("fd validated");
                let mut rb = ReadBuf::new(buf);
                match udp::recv(st, cx, &mut rb) {
                    Poll::Ready(Ok(())) => Poll::Ready(Ok(rb.filled().len())),
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                    Poll::Pending => Poll::Pending,
                }
            }
            Type::Stream => tcp::poll_recv(self, fd, cx, buf),
            Type::SeqPacket => unimplemented!("SOCK_SEQPACKET poll_recv"),
        }
    }

    /// `shutdown(SHUT_WR)`. TCP-only in practice; UDP sockets have no
    /// FIN to send. Kernel-level shutdown-read isn't exposed because
    /// tokio's API doesn't carry the idea (see `TcpStream` docs).
    pub fn poll_shutdown_write(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let ty = match self.lookup(fd) {
            Ok(st) => st.ty,
            Err(e) => return Poll::Ready(Err(e)),
        };
        match ty {
            Type::Stream => tcp::poll_shutdown_write(self, fd, cx),
            Type::Dgram | Type::SeqPacket => {
                unimplemented!("poll_shutdown_write on non-Stream fd")
            }
        }
    }

    /// `recvfrom(2)` with `MSG_PEEK`. Like [`Self::poll_recv_from`] but
    /// leaves the datagram in the recv queue.
    pub fn poll_peek_from(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<Addr>> {
        let st = match self.lookup_mut(fd) {
            Ok(st) => st,
            Err(e) => return Poll::Ready(Err(e)),
        };
        assert_eq!(st.ty, Type::Dgram, "poll_peek_from on non-Dgram fd");
        udp::peek_from(st, cx, buf)
    }

    /// `recv(2)` with `MSG_PEEK` — connected-socket peek. UDP returns
    /// the next datagram without consuming it; TCP returns buffered
    /// bytes without draining.
    pub fn poll_peek(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let ty = match self.lookup(fd) {
            Ok(st) => st.ty,
            Err(e) => return Poll::Ready(Err(e)),
        };
        match ty {
            Type::Dgram => {
                let st = self.lookup_mut(fd).expect("fd validated");
                let mut rb = ReadBuf::new(buf);
                match udp::peek_from(st, cx, &mut rb) {
                    Poll::Ready(Ok(_)) => Poll::Ready(Ok(rb.filled().len())),
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                    Poll::Pending => Poll::Pending,
                }
            }
            Type::Stream => tcp::poll_peek(self, fd, cx, buf),
            Type::SeqPacket => unimplemented!("SOCK_SEQPACKET poll_peek"),
        }
    }

    /// Hand an inbound packet to the stack — dispatches to the socket
    /// bound at the destination tuple, appending to its recv queue and
    /// waking any pending receiver. Drops the packet if no matching
    /// socket is bound.
    pub fn deliver(&mut self, pkt: Packet) {
        match pkt.payload.clone() {
            Transport::Udp(d) => udp::deliver(self, &pkt, &d),
            Transport::Tcp(s) => tcp::deliver(self, &pkt, &s),
        }
    }

    /// Take all packets the stack has produced since the last call.
    ///
    /// Runs TCP segmentation first (draining each established socket's
    /// `send_buf` into MSS-sized segments on `outbound`), then drains
    /// `outbound`. Loopback packets fold back through
    /// [`deliver`](Self::deliver) inline; the returned vec holds only
    /// packets that need to leave this host.
    ///
    /// Loops until a full pass produces no new outbound packets, so
    /// that an ACK folded through `deliver` can open a window and let
    /// the next segmentation pass pick up queued bytes.
    pub fn egress(&mut self) -> Vec<Packet> {
        let mut leaving = Vec::new();
        loop {
            tcp::segment_all(self);
            if self.outbound.is_empty() {
                break;
            }
            let drained: Vec<_> = std::mem::take(&mut self.outbound).into_iter().collect();
            for pkt in drained {
                if self.is_local(pkt.dst) {
                    self.deliver(pkt);
                } else {
                    leaving.push(pkt);
                }
            }
        }
        // Reap any `fd_closed` sockets that have finished closing.
        tcp::reap_closed(self);
        leaving
    }
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::net::SocketAddr;

    use super::*;

    fn inet(s: &str) -> Addr {
        Addr::Inet(s.parse().unwrap())
    }

    #[test]
    fn loopback_is_implicit_local() {
        let mut k = Kernel::new();
        assert!(k.is_local("127.0.0.1".parse().unwrap()));
        assert!(k.is_local("::1".parse().unwrap()));
        assert!(!k.is_local("10.0.0.1".parse().unwrap()));
        k.add_address("10.0.0.1".parse().unwrap());
        assert!(k.is_local("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn bind_records_local_addr() {
        let mut k = Kernel::new();
        let s = k.bind(&inet("127.0.0.1:5000"), Type::Dgram).unwrap();
        assert_eq!(k.local_addr(s).unwrap(), inet("127.0.0.1:5000"));
    }

    #[test]
    fn bind_port_zero_allocates_ephemeral() {
        let mut k = Kernel::new();
        let s = k.bind(&inet("127.0.0.1:0"), Type::Dgram).unwrap();
        let Addr::Inet(SocketAddr::V4(v4)) = k.local_addr(s).unwrap() else {
            panic!("expected v4")
        };
        assert!((49152..=65535).contains(&v4.port()));
    }

    #[test]
    fn bind_conflict_is_addr_in_use() {
        let mut k = Kernel::new();
        k.bind(&inet("127.0.0.1:5000"), Type::Dgram).unwrap();
        let err = k.bind(&inet("127.0.0.1:5000"), Type::Dgram).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AddrInUse);
    }

    #[test]
    fn bind_different_protocols_can_share_port() {
        let mut k = Kernel::new();
        // TCP and UDP live in separate port spaces.
        k.bind(&inet("127.0.0.1:5000"), Type::Dgram).unwrap();
        k.bind(&inet("127.0.0.1:5000"), Type::Stream).unwrap();
    }

    #[test]
    fn bind_rejects_non_local_addr() {
        let mut k = Kernel::new();
        let err = k.bind(&inet("10.0.0.1:5000"), Type::Dgram).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AddrNotAvailable);
    }

    #[test]
    fn bind_wildcard_addr_is_allowed() {
        let mut k = Kernel::new();
        k.bind(&inet("0.0.0.0:5000"), Type::Dgram).unwrap();
    }

    #[test]
    fn distinct_specific_ips_coexist() {
        // Linux: two sockets bound to different specific IPs on the
        // same port never conflict.
        let mut k = Kernel::new();
        k.add_address("10.0.0.1".parse().unwrap());
        k.add_address("10.0.0.2".parse().unwrap());
        k.bind(&inet("10.0.0.1:5000"), Type::Dgram).unwrap();
        k.bind(&inet("10.0.0.2:5000"), Type::Dgram).unwrap();
    }

    // Helpers for driving poll_* directly. Kernel-level tests don't
    // have a tokio runtime — noop waker is enough because we never
    // expect `Pending` from these specific syscalls (send to a
    // configured local IP with a valid target succeeds immediately).
    fn noop_cx() -> Context<'static> {
        use std::task::Waker;
        Context::from_waker(Waker::noop())
    }

    #[test]
    fn udp_broadcast_send_requires_broadcast_option() {
        // Send to a broadcast destination fails with PermissionDenied
        // unless SO_BROADCAST is set — Linux behavior.
        let mut k = Kernel::new();
        k.add_address("10.0.0.1".parse().unwrap());
        let s = k.bind(&inet("10.0.0.1:0"), Type::Dgram).unwrap();

        let dst = Addr::Inet("255.255.255.255:9000".parse().unwrap());
        let Poll::Ready(Err(e)) = k.poll_send_to(s, &mut noop_cx(), b"x", &dst) else {
            panic!("expected broadcast rejection");
        };
        assert_eq!(e.kind(), ErrorKind::PermissionDenied);

        k.set_option(s, SocketOption::Broadcast(true)).unwrap();
        let Poll::Ready(Ok(_)) = k.poll_send_to(s, &mut noop_cx(), b"x", &dst) else {
            panic!("broadcast send should succeed with SO_BROADCAST");
        };
    }

    #[test]
    fn bind_zero_avoids_ports_taken_on_other_ips() {
        // Linux: `:0` picks a port not in use at any IP for the same
        // (domain, ty). The allocator cursor starts at 49152, so that
        // would be the first port handed out. Squatting it on 10.0.0.1
        // should force the next `:0` bind to pick a different port
        // rather than colliding.
        let mut k = Kernel::new();
        k.add_address("10.0.0.1".parse().unwrap());

        k.bind(&inet("10.0.0.1:49152"), Type::Dgram).unwrap();
        let s = k.bind(&inet("127.0.0.1:0"), Type::Dgram).unwrap();
        let Addr::Inet(sa) = k.local_addr(s).unwrap() else {
            panic!("v4 expected")
        };
        assert_ne!(sa.port(), 49152);
    }

    #[test]
    fn broadcast_option_roundtrips() {
        let mut k = Kernel::new();
        let s = k.bind(&inet("127.0.0.1:0"), Type::Dgram).unwrap();
        assert_eq!(
            k.get_option(s, SocketOptionKind::Broadcast).unwrap(),
            SocketOption::Broadcast(false)
        );
        k.set_option(s, SocketOption::Broadcast(true)).unwrap();
        assert_eq!(
            k.get_option(s, SocketOptionKind::Broadcast).unwrap(),
            SocketOption::Broadcast(true)
        );
    }
}
