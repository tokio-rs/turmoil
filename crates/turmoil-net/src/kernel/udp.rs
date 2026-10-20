//! UDP (`AF_INET` + `AF_INET6`, `SOCK_DGRAM`).
//!
//! Free functions invoked by the generic syscall dispatchers in
//! [`kernel::mod`](crate::kernel). Kept out of `impl Kernel` because
//! sibling modules can't call private impl-block methods — free fns
//! with plain `pub(super)` visibility avoid that dance.
//!
//! # TODO
//! - **IP fragmentation.** Oversize datagrams return `EMSGSIZE`,
//!   matching Linux under `IP_PMTUDISC_DO`. Default Linux
//!   (`IP_PMTUDISC_WANT`) fragments at the IP layer and reassembles
//!   at the peer, with any lost fragment dropping the whole datagram.
//!   QUIC and other PMTU-sensitive workloads want that behavior.

use std::io::{Error, ErrorKind, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio::io::ReadBuf;

use crate::kernel::packet::{self, Packet, Transport, UdpDatagram};
use crate::kernel::socket::{Addr, BindKey, Domain, Fd, MulticastMembership, Socket, Type};
use crate::kernel::{DeliveryScope, Kernel, EMSGSIZE};

pub(super) fn recv(
    st: &mut Socket,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
) -> Poll<Result<()>> {
    match recv_from(st, cx, buf) {
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
    dst_sa: &SocketAddr,
) -> Poll<Result<usize>> {
    let st = k
        .sockets
        .get(fd)
        .expect("fd validated by Kernel::poll_send_to");
    let (domain, bound, broadcast_flag) = (st.domain, st.bound.clone(), st.broadcast);
    let multicast = dst_sa.ip().is_multicast();
    let multicast_loop = if dst_sa.is_ipv4() {
        st.multicast_loop_v4
    } else {
        st.multicast_loop_v6
    };
    let ttl = match dst_sa {
        SocketAddr::V4(_) if multicast => st.multicast_ttl_v4,
        SocketAddr::V6(_) if multicast => st.multicast_hops_v6,
        _ => st.ttl,
    };

    // Broadcast destinations require SO_BROADCAST (IPv4 only — IPv6
    // has no broadcast concept).
    let broadcast = packet::is_broadcast(dst_sa.ip());
    if broadcast && !broadcast_flag {
        return Poll::Ready(Err(Error::from(ErrorKind::PermissionDenied)));
    }

    let local_ip = bound
        .as_ref()
        .map_or_else(|| unspecified(dst_sa.ip()), |bind| bind.local_addr);
    let src_ip = source_ip(k, local_ip, dst_sa.ip());
    let scope = if dst_sa.ip().is_loopback() || ((broadcast || multicast) && src_ip.is_loopback()) {
        DeliveryScope::Loopback
    } else {
        DeliveryScope::Link
    };
    if buf.len() as u32 > max_payload(k, dst_sa, scope) {
        return Poll::Ready(Err(Error::from_raw_os_error(EMSGSIZE)));
    }

    let src_bind = match bound {
        Some(bk) => bk,
        None => auto_bind(k, fd, domain, Type::Dgram, dst_sa.ip())?,
    };

    let pkt = Packet {
        src: src_ip,
        dst: dst_sa.ip(),
        ttl,
        payload: Transport::Udp(UdpDatagram {
            src_port: src_bind.local_port,
            dst_port: dst_sa.port(),
            payload: Bytes::copy_from_slice(buf),
        }),
    };
    if broadcast || (multicast && multicast_loop) {
        k.outbound
            .push_back(super::Outbound::Local(pkt.clone(), scope));
    }
    // Local copies bypass network rules, just like unicast loopback.
    // The fabric excludes this host when routing the remote copy.
    if !(broadcast || multicast) || (scope == DeliveryScope::Link && ttl > 0) {
        k.outbound.push_back(super::Outbound::Network(pkt));
    }
    Poll::Ready(Ok(buf.len()))
}

pub(super) fn recv_from(
    st: &mut Socket,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
) -> Poll<Result<Addr>> {
    if let Some((from, payload)) = st.recv_queue.pop_front() {
        let n = payload.len().min(buf.remaining());
        buf.put_slice(&payload[..n]);
        return Poll::Ready(Ok(from));
    }
    st.register_read_waker(cx.waker());
    Poll::Pending
}

pub(super) fn peek_from(
    st: &mut Socket,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
) -> Poll<Result<Addr>> {
    if let Some((from, payload)) = st.recv_queue.front() {
        let n = payload.len().min(buf.remaining());
        buf.put_slice(&payload[..n]);
        return Poll::Ready(Ok(from.clone()));
    }
    st.register_read_waker(cx.waker());
    Poll::Pending
}

pub(super) fn deliver(k: &mut Kernel, pkt: &Packet, d: &UdpDatagram, scope: DeliveryScope) {
    let domain = match pkt.dst {
        IpAddr::V4(_) => Domain::Inet,
        IpAddr::V6(_) => Domain::Inet6,
    };
    if pkt.is_broadcast() || pkt.dst.is_multicast() {
        let targets: Vec<_> = k
            .sockets
            .bindings_on_port(domain, Type::Dgram, d.dst_port)
            .filter(|(bind, _)| bind.local_addr.is_unspecified() || scope.matches(bind.local_addr))
            .flat_map(|(_, fds)| fds.iter().copied())
            .collect();
        for fd in targets {
            let st = k.sockets.get_mut(fd).expect("socket entry present");
            if !pkt.dst.is_multicast()
                || st.multicast_memberships.iter().any(|membership| {
                    membership.group == pkt.dst && scope.matches(membership.interface)
                })
            {
                enqueue(st, pkt, d);
            }
        }
        return;
    }
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
    enqueue(st, pkt, d);
}

fn enqueue(st: &mut Socket, pkt: &Packet, d: &UdpDatagram) {
    let from = Addr::Inet(SocketAddr::new(pkt.src, d.src_port));
    if let Some(peer) = &st.peer {
        if peer != &from {
            return;
        }
    }
    st.recv_queue.push_back((from, d.payload.clone()));
    st.wake_read();
}

pub(super) fn auto_bind(
    k: &mut Kernel,
    fd: Fd,
    domain: Domain,
    ty: Type,
    dst: IpAddr,
) -> Result<BindKey> {
    let local_ip = source_ip(k, unspecified(dst), dst);
    if local_ip.is_unspecified() {
        return Err(Error::from(ErrorKind::AddrNotAvailable));
    }
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
    k.sockets.get_mut(fd).expect("socket present").bound = Some(key.clone());
    Ok(key)
}

fn max_payload(k: &Kernel, dst: &SocketAddr, scope: DeliveryScope) -> u32 {
    let ip_hdr = match dst {
        SocketAddr::V4(_) => packet::IPV4_HEADER_SIZE as u32,
        SocketAddr::V6(_) => packet::IPV6_HEADER_SIZE as u32,
    };
    let mtu = match scope {
        DeliveryScope::Loopback => k.loopback_mtu,
        DeliveryScope::Link => k.mtu,
    };
    mtu.saturating_sub(ip_hdr)
        .saturating_sub(packet::UDP_HEADER_SIZE as u32)
}

fn unspecified(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        IpAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    }
}

pub(super) fn validate_membership(st: &Socket, group: IpAddr) -> Result<()> {
    if !group.is_multicast()
        || st.ty != Type::Dgram
        || st.domain
            != if group.is_ipv4() {
                Domain::Inet
            } else {
                Domain::Inet6
            }
    {
        return Err(Error::from(ErrorKind::InvalidInput));
    }
    Ok(())
}

/// Select an address using the same rules for sends and default
/// memberships: concrete bind, first configured address of the right
/// family, then implicit loopback for multicast on a loopback-only host.
fn source_ip(k: &Kernel, local: IpAddr, dst: IpAddr) -> IpAddr {
    if !local.is_unspecified() {
        return local;
    }
    let loopback = match dst {
        IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
        IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
    };
    if dst.is_loopback() {
        return loopback;
    }
    k.addresses
        .iter()
        .copied()
        .find(|a| a.is_ipv4() == dst.is_ipv4())
        .unwrap_or_else(|| {
            if dst.is_multicast() || packet::is_broadcast(dst) {
                loopback
            } else {
                local
            }
        })
}

pub(super) fn membership(
    k: &mut Kernel,
    fd: Fd,
    group: IpAddr,
    interface: IpAddr,
    join: bool,
) -> Result<()> {
    let st = k.lookup(fd)?;
    validate_membership(st, group)?;
    let interface = if interface.is_unspecified() {
        let local = st.bound.as_ref().map_or(interface, |bind| bind.local_addr);
        source_ip(k, local, group)
    } else if k.is_local(interface) {
        interface
    } else {
        return Err(Error::from(ErrorKind::AddrNotAvailable));
    };
    let membership = MulticastMembership { group, interface };
    let st = k.lookup_mut(fd)?;
    if join {
        // Idempotent, matching turmoil's existing multicast API.
        st.multicast_memberships.insert(membership);
    } else if !st.multicast_memberships.shift_remove(&membership) {
        return Err(Error::from(ErrorKind::AddrNotAvailable));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::SocketOption;

    fn bind(k: &mut Kernel, ip: IpAddr, port: u16) -> Fd {
        k.bind(&Addr::Inet(SocketAddr::new(ip, port)), Type::Dgram)
            .unwrap()
    }

    fn send(k: &mut Kernel, fd: Fd, dst: SocketAddr, payload: &[u8]) {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            k.poll_send_to(fd, &mut cx, payload, &Addr::Inet(dst)),
            Poll::Ready(Ok(n)) if n == payload.len()
        ));
    }

    fn pending(k: &Kernel, fd: Fd) -> usize {
        k.lookup(fd).unwrap().recv_queue.len()
    }

    fn join(k: &mut Kernel, fd: Fd, group: IpAddr) {
        let opt = match group {
            IpAddr::V4(group) => SocketOption::IpAddMembership {
                group,
                iface: Ipv4Addr::UNSPECIFIED,
            },
            IpAddr::V6(group) => SocketOption::Ipv6JoinGroup { group, iface: 0 },
        };
        k.set_option(fd, opt).unwrap();
    }

    #[test]
    fn cast_fans_out_and_filters_port_family_group_and_peer() {
        for dst in ["255.255.255.255:9000", "239.0.0.255:9000", "[ff08::1]:9000"] {
            let dst: SocketAddr = dst.parse().unwrap();
            let mut k = Kernel::new();
            let ips: Vec<IpAddr> = if dst.is_ipv4() {
                ["10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4"]
            } else {
                ["fd00::1", "fd00::2", "fd00::3", "fd00::4"]
            }
            .map(|ip| ip.parse().unwrap())
            .to_vec();
            let fds: Vec<_> = ips
                .iter()
                .map(|&ip| {
                    k.add_address(ip);
                    bind(&mut k, ip, 9000)
                })
                .collect();
            let other_port = bind(&mut k, ips[0], 9001);
            let other_family = bind(
                &mut k,
                if dst.is_ipv4() {
                    Ipv6Addr::UNSPECIFIED.into()
                } else {
                    Ipv4Addr::UNSPECIFIED.into()
                },
                9000,
            );
            if dst.ip().is_multicast() {
                for fd in [fds[0], fds[1], fds[3], other_port] {
                    join(&mut k, fd, dst.ip());
                }
            }
            // The third socket is a member of a different group.
            let other_group = if dst.is_ipv4() {
                "239.0.0.2"
            } else {
                "ff08::2"
            }
            .parse()
            .unwrap();
            join(&mut k, fds[2], other_group);
            k.lookup_mut(fds[3]).unwrap().peer = Some(Addr::Inet(SocketAddr::new(ips[0], 9999)));
            k.set_option(fds[0], SocketOption::Broadcast(true)).unwrap();

            send(&mut k, fds[0], dst, b"cast");
            let mut out = Vec::new();
            k.egress(&mut out);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].dst, dst.ip());
            assert_eq!(pending(&k, fds[0]), 1);
            assert_eq!(pending(&k, fds[1]), 1);
            assert_eq!(pending(&k, fds[2]), usize::from(!dst.ip().is_multicast()));
            assert_eq!(pending(&k, fds[3]), 0);
            assert_eq!(pending(&k, other_port), 0);
            assert_eq!(pending(&k, other_family), 0);
        }
    }

    #[test]
    fn multicast_loop_uses_sender_flag_and_still_emits_remote_packet() {
        for (group, local, loop_option) in [
            (
                "239.0.0.1",
                "10.0.0.1",
                SocketOption::IpMulticastLoop as fn(bool) -> SocketOption,
            ),
            ("ff08::1", "fd00::1", SocketOption::Ipv6MulticastLoop),
        ] {
            let group: IpAddr = group.parse().unwrap();
            let local = local.parse().unwrap();
            let mut k = Kernel::new();
            k.add_address(local);
            let sender = bind(&mut k, local, 8000);
            let receiver = bind(&mut k, local, 9000);
            join(&mut k, receiver, group);
            k.set_option(receiver, loop_option(false)).unwrap();
            let dst = SocketAddr::new(group, 9000);

            send(&mut k, sender, dst, b"enabled");
            let mut out = Vec::new();
            k.egress(&mut out);
            assert_eq!(out.len(), 1);
            assert_eq!(pending(&k, receiver), 1);

            k.set_option(sender, loop_option(false)).unwrap();
            k.set_option(receiver, loop_option(true)).unwrap();
            send(&mut k, sender, dst, b"disabled");
            k.egress(&mut out);
            assert_eq!(out.len(), 2);
            assert_eq!(pending(&k, receiver), 1);
        }
    }

    #[test]
    fn multicast_ttl_zero_is_local_only() {
        let mut k = Kernel::new();
        let ip = "10.0.0.1".parse().unwrap();
        k.add_address(ip);
        let fd = bind(&mut k, ip, 9000);
        let dst: SocketAddr = "239.0.0.1:9000".parse().unwrap();
        join(&mut k, fd, dst.ip());
        k.set_option(fd, SocketOption::IpMulticastTtl(0)).unwrap();
        send(&mut k, fd, dst, b"local");
        let mut out = Vec::new();
        k.egress(&mut out);
        assert!(out.is_empty());
        assert_eq!(pending(&k, fd), 1);

        k.set_option(fd, SocketOption::IpMulticastTtl(7)).unwrap();
        send(&mut k, fd, dst, b"remote");
        k.egress(&mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ttl, 7);
        assert_eq!(pending(&k, fd), 2);
    }

    #[test]
    fn local_unicast_and_cast_preserve_send_order() {
        let mut k = Kernel::new();
        let ip = "10.0.0.1".parse().unwrap();
        k.add_address(ip);
        let fd = bind(&mut k, ip, 9000);
        k.set_option(fd, SocketOption::Broadcast(true)).unwrap();
        let group = "239.0.0.1".parse().unwrap();
        join(&mut k, fd, group);
        for (i, dst) in [ip, Ipv4Addr::BROADCAST.into(), ip, group]
            .into_iter()
            .enumerate()
        {
            send(&mut k, fd, SocketAddr::new(dst, 9000), &[i as u8]);
        }
        k.egress(&mut Vec::new());
        let payloads: Vec<_> = k
            .lookup(fd)
            .unwrap()
            .recv_queue
            .iter()
            .map(|(_, bytes)| bytes[0])
            .collect();
        assert_eq!(payloads, [0, 1, 2, 3]);
    }

    #[test]
    fn default_memberships_follow_the_bound_address_scope() {
        for (local, loopback, group, remote) in [
            ("10.0.0.1", "127.0.0.1", "239.0.0.1", "10.0.0.2"),
            ("fd00::1", "::1", "ff08::1", "fd00::2"),
        ] {
            let local: IpAddr = local.parse().unwrap();
            let loopback = loopback.parse().unwrap();
            let group = group.parse().unwrap();
            let mut k = Kernel::new();
            k.add_address(local);
            let link = bind(&mut k, local, 9000);
            let lo = bind(&mut k, loopback, 9000);
            join(&mut k, link, group);
            join(&mut k, lo, group);
            let dst = SocketAddr::new(group, 9000);

            send(&mut k, lo, dst, b"loopback");
            let mut out = Vec::new();
            k.egress(&mut out);
            assert!(out.is_empty(), "loopback must not reach the fabric");
            assert_eq!(pending(&k, lo), 1);
            assert_eq!(pending(&k, link), 0);

            send(&mut k, link, dst, b"shared link");
            k.egress(&mut out);
            assert_eq!(out.len(), 1);
            assert_eq!(pending(&k, lo), 1);
            assert_eq!(pending(&k, link), 1);

            let mut pkt = out.pop().unwrap();
            pkt.src = remote.parse().unwrap();
            k.deliver(pkt);
            assert_eq!(pending(&k, lo), 1);
            assert_eq!(pending(&k, link), 2);
        }
    }

    #[test]
    fn default_membership_selects_an_address_of_the_right_family() {
        let mut k = Kernel::new();
        k.add_address("fd00::1".parse().unwrap());
        let v4 = "10.0.0.1".parse().unwrap();
        k.add_address(IpAddr::V4(v4));
        let socket = bind(&mut k, Ipv4Addr::UNSPECIFIED.into(), 9000);
        let group = "239.0.0.1".parse().unwrap();
        join(&mut k, socket, IpAddr::V4(group));
        // Explicitly leaving the selected IPv4 address must find the join.
        k.set_option(socket, SocketOption::IpDropMembership { group, iface: v4 })
            .unwrap();
    }

    #[test]
    fn broadcast_respects_loopback_and_link_bindings() {
        let mut k = Kernel::new();
        let local = "10.0.0.1".parse().unwrap();
        k.add_address(local);
        let link = bind(&mut k, local, 9000);
        let lo = bind(&mut k, Ipv4Addr::LOCALHOST.into(), 9000);
        for fd in [link, lo] {
            k.set_option(fd, SocketOption::Broadcast(true)).unwrap();
        }
        let dst = SocketAddr::new(Ipv4Addr::BROADCAST.into(), 9000);
        send(&mut k, link, dst, b"link");
        let mut out = Vec::new();
        k.egress(&mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(pending(&k, link), 1);
        assert_eq!(pending(&k, lo), 0, "broadcast escaped into loopback");

        out.clear();
        send(&mut k, lo, dst, b"loopback");
        k.egress(&mut out);
        assert!(out.is_empty());
        assert_eq!(pending(&k, link), 1);
        assert_eq!(pending(&k, lo), 1);
    }

    #[test]
    fn loopback_cast_uses_loopback_mtu() {
        for dst in ["255.255.255.255:9000", "239.0.0.1:9000", "[ff08::1]:9000"] {
            let dst: SocketAddr = dst.parse().unwrap();
            let mut k = Kernel::new();
            let local = if dst.is_ipv4() {
                Ipv4Addr::LOCALHOST.into()
            } else {
                Ipv6Addr::LOCALHOST.into()
            };
            let fd = bind(&mut k, local, 9000);
            k.set_option(fd, SocketOption::Broadcast(true)).unwrap();
            if dst.ip().is_multicast() {
                join(&mut k, fd, dst.ip());
            }
            // Larger than the link MTU, but small enough for loopback.
            let payload = vec![42; 4096];
            send(&mut k, fd, dst, &payload);
            let mut out = Vec::new();
            k.egress(&mut out);
            assert!(out.is_empty());
            assert_eq!(k.lookup(fd).unwrap().recv_queue[0].1.as_ref(), payload);

            // The same payload must still fail over the shared link.
            let link_ip = if dst.is_ipv4() { "10.0.0.1" } else { "fd00::1" };
            let link_ip = link_ip.parse().unwrap();
            k.add_address(link_ip);
            let link = bind(&mut k, link_ip, 9000);
            k.set_option(link, SocketOption::Broadcast(true)).unwrap();
            let mut cx = Context::from_waker(std::task::Waker::noop());
            let Poll::Ready(Err(err)) = k.poll_send_to(link, &mut cx, &payload, &Addr::Inet(dst))
            else {
                panic!("expected shared-link MTU rejection");
            };
            assert_eq!(err.raw_os_error(), Some(EMSGSIZE));
            k.egress(&mut out);
            assert!(out.is_empty());
            assert_eq!(pending(&k, link), 0);
        }
    }

    #[test]
    fn unbound_multicast_uses_the_same_default_as_bound_multicast() {
        for (domain, group, local) in [
            (Domain::Inet, "239.0.0.1", "127.0.0.1"),
            (Domain::Inet6, "ff08::1", "::1"),
        ] {
            let group = group.parse().unwrap();
            let local = local.parse().unwrap();
            let mut k = Kernel::new();
            let receiver = bind(&mut k, local, 9000);
            join(&mut k, receiver, group);
            let sender = k.open(domain, Type::Dgram);
            send(&mut k, sender, SocketAddr::new(group, 9000), b"auto-bind");
            let mut out = Vec::new();
            k.egress(&mut out);
            assert!(out.is_empty());
            assert_eq!(pending(&k, receiver), 1);
        }
    }
}
