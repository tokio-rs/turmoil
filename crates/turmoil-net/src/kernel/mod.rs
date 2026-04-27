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

#![allow(dead_code, unused_variables)]

use std::collections::VecDeque;
use std::io::{Error, ErrorKind};
use std::net::{IpAddr, SocketAddr};
use std::task::{Context, Poll};

use tokio::io::ReadBuf;

use crate::kernel::packet::{Packet, Transport};
use crate::kernel::socket::{BindKey, ListenState, Socket, SocketTable};

mod packet;
mod socket;
mod tcp;
mod udp;
mod uds;

// for shims
pub use socket::{Addr, Domain, Fd, SocketOption, SocketOptionKind, Type};

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

/// Default MTU for non-loopback traffic (bytes). Matches standard Ethernet.
const DEFAULT_MTU: u32 = 1500;
/// Default MTU for loopback (bytes). Matches Linux `lo`.
const DEFAULT_LOOPBACK_MTU: u32 = 65536;

/// Linux errno for `EAFNOSUPPORT`. Used where we want `kind() ==
/// Uncategorized` to match what `tokio::net` surfaces for
/// socket-family mismatches (the `Uncategorized` variant itself is
/// unstable). `from_raw_os_error` on any platform we care about maps
/// this to `Uncategorized`.
const EAFNOSUPPORT: i32 = 97;

/// A per-host network stack.
///
/// Owns the socket table and the inbound/outbound packet queues. Does
/// not know how packets reach other hosts — that's the transport
/// layer's job.
#[derive(Debug)]
pub struct Kernel {
    sockets: SocketTable,
    addresses: Vec<IpAddr>,
    mtu: u32,
    loopback_mtu: u32,
    /// Packets queued by `poll_send_*` awaiting `egress()`.
    outbound: VecDeque<Packet>,
    /// Monotonic TCP initial-sequence-number source. Deterministic by
    /// design — real kernels randomize.
    tcp_isn: u32,
}

impl Kernel {
    /// Construct a fresh kernel with default MTU settings and no
    /// configured addresses beyond implicit loopback.
    pub fn new() -> Self {
        Self {
            sockets: SocketTable::new(),
            addresses: Vec::new(),
            mtu: DEFAULT_MTU,
            loopback_mtu: DEFAULT_LOOPBACK_MTU,
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
    /// any binding. Idempotent — closing an already-closed socket is a
    /// no-op.
    pub fn close(&mut self, fd: Fd) {
        self.sockets.remove(fd);
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

    /// Set the non-loopback MTU.
    pub fn set_mtu(&mut self, mtu: u32) {
        self.mtu = mtu;
    }

    /// Current non-loopback MTU.
    pub fn mtu(&self) -> u32 {
        self.mtu
    }

    /// Current loopback MTU.
    pub fn loopback_mtu(&self) -> u32 {
        self.loopback_mtu
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

    /// `send(2)` — for connected sockets.
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
        assert_eq!(ty, Type::Dgram, "poll_send on non-Dgram fd");
        let Some(peer) = peer else {
            return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
        };
        let Addr::Inet(peer_sa) = peer else {
            panic!("UDP peer stored as Addr::Unix");
        };
        udp::send_to(self, fd, cx, buf, &peer_sa)
    }

    /// `recv(2)` — for connected sockets.
    pub fn poll_recv(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let st = match self.lookup_mut(fd) {
            Ok(st) => st,
            Err(e) => return Poll::Ready(Err(e)),
        };
        assert_eq!(st.ty, Type::Dgram, "poll_recv on non-Dgram fd");
        udp::recv(st, cx, buf)
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

    /// `recv(2)` with `MSG_PEEK` — connected-socket peek.
    pub fn poll_peek(
        &mut self,
        fd: Fd,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.poll_peek_from(fd, cx, buf) {
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
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
    /// Loopback (or any packet whose destination is one of this
    /// kernel's local addresses) is folded back through
    /// [`deliver`](Self::deliver) inline; the returned vec holds only
    /// packets that need to leave this host.
    pub fn egress(&mut self) -> Vec<Packet> {
        let drained: Vec<_> = std::mem::take(&mut self.outbound).into_iter().collect();
        let mut leaving = Vec::new();
        for pkt in drained {
            if self.is_local(pkt.dst) {
                self.deliver(pkt);
            } else {
                leaving.push(pkt);
            }
        }
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
    fn default_mtu_matches_ethernet() {
        let k = Kernel::new();
        assert_eq!(k.mtu(), 1500);
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
