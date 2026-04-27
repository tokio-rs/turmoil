//! UDP (`AF_INET` + `AF_INET6`, `SOCK_DGRAM`).
//!
//! Free functions invoked by the generic syscall dispatchers in
//! [`kernel::mod`](crate::kernel). Kept out of `impl Kernel` because
//! sibling modules can't call private impl-block methods — free fns
//! with plain `pub(super)` visibility avoid that dance.
//!
//! # TODO
//! - **Broadcast fan-out.** `SO_BROADCAST` is gated in the send path
//!   but `deliver` doesn't fan out broadcast packets to every matching
//!   local socket, and the sender doesn't see its own broadcast.
//! - **Multicast.** Group membership (`IpAddMembership` /
//!   `Ipv6JoinGroup`) isn't wired through `set_option`; `deliver`
//!   doesn't multicast-fan-out; `IP_MULTICAST_LOOP` isn't honored.

use std::io::{Error, ErrorKind, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio::io::ReadBuf;

use crate::kernel::packet::{self, Packet, Transport, UdpDatagram};
use crate::kernel::socket::{Addr, BindKey, Domain, Fd, Type};
use crate::kernel::Kernel;

// TODO: fix weirdness that loops back into kernel for lookup. Just pass these udp extensions exactly what they need.

pub(super) fn connect(k: &mut Kernel, fd: Fd, addr: &Addr) -> Result<()> {
    let Addr::Inet(peer_sa) = addr else {
        return Err(Error::from(ErrorKind::InvalidInput));
    };
    let st = k.lookup(fd)?;
    match (st.domain, peer_sa) {
        (Domain::Inet, SocketAddr::V4(_)) | (Domain::Inet6, SocketAddr::V6(_)) => {}
        _ => return Err(Error::from(ErrorKind::InvalidInput)),
    }
    if st.bound.is_none() {
        auto_bind(k, fd, peer_sa.ip())?;
    }
    k.lookup_mut(fd)?.peer = Some(addr.clone());
    Ok(())
}

pub(super) fn send(
    k: &mut Kernel,
    fd: Fd,
    cx: &mut Context<'_>,
    buf: &[u8],
) -> Poll<Result<usize>> {
    let peer = match k.lookup(fd) {
        Ok(st) => st.peer.clone(),
        Err(e) => return Poll::Ready(Err(e)),
    };
    let Some(peer) = peer else {
        return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
    };
    send_to(k, fd, cx, buf, &peer)
}

pub(super) fn recv(
    k: &mut Kernel,
    fd: Fd,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
) -> Poll<Result<()>> {
    match recv_from(k, fd, cx, buf) {
        Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
        Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
        Poll::Pending => Poll::Pending,
    }
}

pub(super) fn send_to(
    k: &mut Kernel,
    fd: Fd,
    _cx: &mut Context<'_>,
    buf: &[u8],
    dst: &Addr,
) -> Poll<Result<usize>> {
    let Addr::Inet(dst_sa) = dst else {
        return Poll::Ready(Err(Error::from(ErrorKind::InvalidInput)));
    };
    let (domain, bound, broadcast_flag) = match k.lookup(fd) {
        Ok(st) => (st.domain, st.bound.clone(), st.broadcast),
        Err(e) => return Poll::Ready(Err(e)),
    };
    match (domain, dst_sa) {
        (Domain::Inet, SocketAddr::V4(_)) | (Domain::Inet6, SocketAddr::V6(_)) => {}
        _ => return Poll::Ready(Err(Error::from(ErrorKind::InvalidInput))),
    }

    // Broadcast destinations require SO_BROADCAST (IPv4 only — IPv6
    // has no broadcast concept).
    if let SocketAddr::V4(v4) = dst_sa {
        if is_ipv4_broadcast(v4.ip()) && !broadcast_flag {
            return Poll::Ready(Err(Error::from(ErrorKind::PermissionDenied)));
        }
    }

    if buf.len() as u32 > max_payload(k, dst_sa) {
        return Poll::Ready(Err(Error::new(
            ErrorKind::InvalidInput,
            "datagram exceeds MTU",
        )));
    }

    let src_bind = match bound {
        Some(bk) => bk,
        None => auto_bind(k, fd, dst_sa.ip())?,
    };

    let src_ip = if src_bind.local_addr.is_unspecified() {
        if dst_sa.ip().is_loopback() {
            match dst_sa {
                SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            }
        } else {
            k.addresses
                .iter()
                .copied()
                .find(|a| a.is_ipv4() == dst_sa.is_ipv4())
                .unwrap_or(src_bind.local_addr)
        }
    } else {
        src_bind.local_addr
    };

    k.outbound.push_back(Packet {
        src: src_ip,
        dst: dst_sa.ip(),
        ttl: 64,
        payload: Transport::Udp(UdpDatagram {
            src_port: src_bind.local_port,
            dst_port: dst_sa.port(),
            payload: Bytes::copy_from_slice(buf),
        }),
    });
    Poll::Ready(Ok(buf.len()))
}

pub(super) fn recv_from(
    k: &mut Kernel,
    fd: Fd,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
) -> Poll<Result<Addr>> {
    let st = k.lookup_mut(fd)?;
    if let Some((from, payload)) = st.recv_queue.pop_front() {
        let n = payload.len().min(buf.remaining());
        buf.put_slice(&payload[..n]);
        return Poll::Ready(Ok(from));
    }
    st.register_recv_waker(cx.waker());
    Poll::Pending
}

pub(super) fn deliver(k: &mut Kernel, pkt: &Packet, d: &UdpDatagram) {
    let domain = match pkt.dst {
        IpAddr::V4(_) => Domain::Inet,
        IpAddr::V6(_) => Domain::Inet6,
    };
    let exact = BindKey {
        domain,
        ty: Type::Dgram,
        local_addr: pkt.dst,
        local_port: d.dst_port,
    };
    let wildcard_ip = match pkt.dst {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    let target = k.sockets.find_by_bind(&exact).first().copied().or_else(|| {
        k.sockets
            .find_by_bind(&BindKey {
                domain,
                ty: Type::Dgram,
                local_addr: wildcard_ip,
                local_port: d.dst_port,
            })
            .first()
            .copied()
    });
    let Some(fd) = target else { return };
    let st = k.sockets.get_mut(fd).expect("socket entry present");
    let from = Addr::Inet(SocketAddr::new(pkt.src, d.src_port));
    if let Some(peer) = &st.peer {
        if peer != &from {
            return;
        }
    }
    st.recv_queue
        .push_back((from, Bytes::copy_from_slice(&d.payload)));
    st.wake_recv();
}

fn auto_bind(k: &mut Kernel, fd: Fd, dst: IpAddr) -> Result<BindKey> {
    let local_ip = if dst.is_loopback() {
        match dst {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        }
    } else {
        k.addresses
            .iter()
            .copied()
            .find(|a| a.is_ipv4() == dst.is_ipv4())
            .ok_or_else(|| Error::from(ErrorKind::AddrNotAvailable))?
    };
    let (domain, ty) = {
        let st = k.lookup(fd)?;
        (st.domain, st.ty)
    };
    let port = k
        .sockets
        .allocate_port(domain, ty)
        .ok_or_else(|| Error::from(ErrorKind::AddrInUse))?;
    let key = BindKey {
        domain,
        ty,
        local_addr: local_ip,
        local_port: port,
    };
    k.sockets.insert_binding(key.clone(), fd);
    k.lookup_mut(fd)?.bound = Some(key.clone());
    Ok(key)
}

fn max_payload(k: &Kernel, dst: &SocketAddr) -> u32 {
    let ip_hdr = match dst {
        SocketAddr::V4(_) => packet::IPV4_HEADER_SIZE as u32,
        SocketAddr::V6(_) => packet::IPV6_HEADER_SIZE as u32,
    };
    let mtu = if dst.ip().is_loopback() {
        k.loopback_mtu
    } else {
        k.mtu
    };
    mtu.saturating_sub(ip_hdr)
        .saturating_sub(packet::UDP_HEADER_SIZE as u32)
}

fn is_ipv4_broadcast(ip: &Ipv4Addr) -> bool {
    ip.is_broadcast() || ip.octets()[3] == 255
}
