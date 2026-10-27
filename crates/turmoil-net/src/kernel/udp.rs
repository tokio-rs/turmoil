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
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio::io::ReadBuf;

use crate::kernel::packet::{self, Packet, Transport, UdpDatagram};
use crate::kernel::socket::{Addr, BindKey, Domain, Fd, MulticastMembership, Socket, Type};
use crate::kernel::{InterfaceId, Kernel, EMSGSIZE};

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
    let (bound, broadcast_flag) = (st.bound.clone(), st.broadcast);
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
    // Membership, bind address, and multicast output interface are separate
    // socket state. In particular a loopback source does not select lo.
    let scope = if multicast {
        let explicit = match dst_sa {
            SocketAddr::V4(_) => st.multicast_if_v4,
            SocketAddr::V6(sa) if sa.scope_id() != 0 => Some(
                k.interfaces
                    .by_index(sa.scope_id())
                    .ok_or_else(|| Error::from_raw_os_error(super::ENODEV))?,
            ),
            SocketAddr::V6(_) => st.multicast_if_v6,
        };
        explicit.unwrap_or_else(|| k.interfaces.multicast_route(dst_sa.ip()))
    } else if dst_sa.ip().is_loopback() {
        InterfaceId::Loopback
    } else if broadcast {
        k.interfaces.multicast_route(dst_sa.ip())
    } else {
        InterfaceId::Link
    };
    let src_ip =
        if local_ip.is_unspecified() || local_ip.is_multicast() || packet::is_broadcast(local_ip) {
            let selected = if multicast && dst_sa.is_ipv4() {
                st.multicast_source_v4.map(IpAddr::V4)
            } else {
                None
            };
            selected
                .or_else(|| k.interfaces.source(scope, dst_sa.ip()))
                .ok_or(ErrorKind::AddrNotAvailable)?
        } else {
            local_ip
        };
    if buf.len() as u32 > max_payload(k, dst_sa, scope) {
        return Poll::Ready(Err(Error::from_raw_os_error(EMSGSIZE)));
    }

    let src_bind = match bound {
        Some(bk) => bk,
        None => {
            // sendto autobinds only the port. Keep source/route selection
            // available for subsequent datagrams to other interfaces.
            k.bind_socket(
                fd,
                &Addr::Inet(SocketAddr::new(unspecified(dst_sa.ip()), 0)),
            )?;
            k.lookup(fd)?.bound.clone().expect("socket just bound")
        }
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
    if broadcast
        || (multicast && (multicast_loop || (dst_sa.is_ipv4() && scope == InterfaceId::Loopback)))
    {
        k.outbound
            .push_back(super::Outbound::Local(pkt.clone(), scope));
    }
    // Local copies bypass network rules, just like unicast loopback.
    // The fabric excludes this host when routing the remote copy.
    if !(broadcast || multicast) || (scope == InterfaceId::Link && ttl > 0) {
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

pub(super) fn deliver(k: &mut Kernel, pkt: &Packet, d: &UdpDatagram, scope: InterfaceId) {
    // Linux rejects loopback-source packets arriving on a non-loopback device
    // under the default route_localnet policy. Source binding is not routing.
    if (pkt.is_broadcast() || pkt.dst.is_multicast())
        && scope == InterfaceId::Link
        && pkt.src.is_loopback()
    {
        return;
    }
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
    let wildcard = BindKey {
        local_addr: unspecified(pkt.dst),
        ..exact.clone()
    };
    if pkt.is_broadcast() || pkt.dst.is_multicast() {
        let multicast = pkt.dst.is_multicast();
        let membership = MulticastMembership {
            group: pkt.dst,
            interface: scope,
        };
        if multicast && !k.sockets.has_membership(&membership) {
            return;
        }
        // Only the destination bind and wildcard bind can receive this cast.
        // Both lookups include the port; unrelated sockets are never visited.
        for key in [&exact, &wildcard] {
            k.sockets.for_each_bound_mut(key, |st| {
                let all = if pkt.dst.is_ipv4() {
                    st.multicast_all_v4
                } else {
                    st.multicast_all_v6
                };
                if !multicast || all || st.multicast_memberships().contains(&membership) {
                    enqueue(st, pkt, d, scope);
                }
            });
        }
        return;
    }
    let exact_ids = k.sockets.find_by_bind(&exact);
    let wildcard_ids = k.sockets.find_by_bind(&wildcard);
    // Ordinary unicast needs no candidate scan, allocation, or extra socket
    // lookup. enqueue applies the connected-peer filter to the sole target.
    match (exact_ids, wildcard_ids) {
        ([], []) => return,
        ([fd], []) | ([], [fd]) => {
            let fd = *fd;
            enqueue(
                k.sockets.get_mut(fd).expect("socket entry present"),
                pkt,
                d,
                scope,
            );
            return;
        }
        _ => {}
    }

    // Only shared/overlapping binds need scoring. Keep the best score and
    // count in registers; a reuse-port group takes a second pass over that
    // group's sockets to select a flow without allocating a candidate Vec.
    let peer = source_endpoint(pkt.src, d.src_port, scope);
    let mut best_score = 0;
    let mut count = 0;
    let mut all_reuse_port = true;
    let mut last = None;
    let mut best_group = &[][..];
    for (ids, specificity) in [(exact_ids, 1), (wildcard_ids, 0)] {
        for &fd in ids {
            let st = k.lookup(fd).expect("socket entry present");
            if st.peer.as_ref().is_some_and(|p| p != &peer) {
                continue;
            }
            let score = specificity + 2 * u8::from(st.peer.is_some());
            if last.is_none() || score > best_score {
                best_score = score;
                count = 0;
                all_reuse_port = true;
                best_group = ids;
            }
            if score == best_score {
                count += 1;
                all_reuse_port &= st.reuse_port;
                last = Some(fd);
            }
        }
    }
    let Some(mut fd) = last else { return };
    if count > 1 && all_reuse_port {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hash = DefaultHasher::new();
        (pkt.src, pkt.dst, d.src_port, d.dst_port).hash(&mut hash);
        let selected = hash.finish() as usize % count;
        let connected = best_score >= 2;
        fd = best_group
            .iter()
            .copied()
            .filter(|&fd| {
                let st = k.lookup(fd).expect("socket entry present");
                st.peer.is_some() == connected && st.peer.as_ref().is_none_or(|p| p == &peer)
            })
            .nth(selected)
            .expect("selected member of reuse-port group");
    }
    enqueue(
        k.sockets.get_mut(fd).expect("socket entry present"),
        pkt,
        d,
        scope,
    );
}

fn source_endpoint(ip: IpAddr, port: u16, interface: InterfaceId) -> Addr {
    let addr = match ip {
        IpAddr::V6(ip) if ip.is_unicast_link_local() => {
            SocketAddrV6::new(ip, port, 0, interface.index()).into()
        }
        _ => SocketAddr::new(ip, port),
    };
    Addr::Inet(addr)
}

fn enqueue(st: &mut Socket, pkt: &Packet, d: &UdpDatagram, interface: InterfaceId) {
    let from = source_endpoint(pkt.src, d.src_port, interface);
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

fn max_payload(k: &Kernel, dst: &SocketAddr, scope: InterfaceId) -> u32 {
    let ip_hdr = match dst {
        SocketAddr::V4(_) => packet::IPV4_HEADER_SIZE as u32,
        SocketAddr::V6(_) => packet::IPV6_HEADER_SIZE as u32,
    };
    let mtu = k.interfaces.get(scope).mtu;
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

/// Source selection for sockets autobound by connect/sendto.
fn source_ip(k: &Kernel, local: IpAddr, dst: IpAddr) -> IpAddr {
    if !local.is_unspecified() && !local.is_multicast() && !packet::is_broadcast(local) {
        return local;
    }
    let iface = if dst.is_loopback() {
        InterfaceId::Loopback
    } else if dst.is_multicast() || packet::is_broadcast(dst) {
        k.interfaces.multicast_route(dst)
    } else {
        InterfaceId::Link
    };
    k.interfaces.source(iface, dst).unwrap_or(local)
}

pub(super) fn membership(
    k: &mut Kernel,
    fd: Fd,
    group: IpAddr,
    interface: InterfaceId,
    join: bool,
) -> Result<()> {
    let st = k.lookup(fd)?;
    validate_membership(st, group)?;
    let membership = MulticastMembership { group, interface };
    if join {
        if !k.sockets.join_multicast(fd, membership) {
            return Err(Error::from(ErrorKind::AddrInUse));
        }
    } else if !k.sockets.leave_multicast(fd, &membership) {
        return Err(Error::from(ErrorKind::AddrNotAvailable));
    }
    Ok(())
}

#[cfg(test)]
mod bench;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::SocketOption;

    fn bind(k: &mut Kernel, ip: IpAddr, port: u16) -> Fd {
        k.bind(&Addr::Inet(SocketAddr::new(ip, port)), Type::Dgram)
            .unwrap()
    }

    fn reuse_bind(k: &mut Kernel, ip: IpAddr, port: u16) -> Fd {
        let domain = if ip.is_ipv4() {
            Domain::Inet
        } else {
            Domain::Inet6
        };
        let fd = k.open(domain, Type::Dgram);
        k.set_option(fd, SocketOption::ReuseAddr(true)).unwrap();
        k.bind_socket(fd, &Addr::Inet(SocketAddr::new(ip, port)))
            .unwrap();
        fd
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
                    let fd = reuse_bind(&mut k, unspecified(ip), 9000);
                    k.set_option(fd, SocketOption::IpMulticastAll(false))
                        .unwrap();
                    k.set_option(fd, SocketOption::Ipv6MulticastAll(false))
                        .unwrap();
                    fd
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
            let receiver = bind(&mut k, unspecified(local), 9000);
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
        let fd = bind(&mut k, unspecified(ip), 9000);
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
        let fd = bind(&mut k, unspecified(ip), 9000);
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
    fn memberships_and_output_use_interface_identity() {
        for (local, loopback, group, remote) in [
            ("10.0.0.1", "127.0.0.1", "239.0.0.1", "10.0.0.2"),
            ("fd00::1", "::1", "ff08::1", "fd00::2"),
        ] {
            let local: IpAddr = local.parse().unwrap();
            let group: IpAddr = group.parse().unwrap();
            let mut k = Kernel::new();
            k.add_address(local);
            let link = reuse_bind(&mut k, unspecified(local), 9000);
            let lo = reuse_bind(&mut k, unspecified(local), 9000);
            join(&mut k, link, group);
            let (join_lo, send_lo, all) = match group {
                IpAddr::V4(group) => (
                    SocketOption::IpAddMembership {
                        group,
                        iface: loopback.parse().unwrap(),
                    },
                    SocketOption::IpMulticastIf(loopback.parse().unwrap()),
                    SocketOption::IpMulticastAll(false),
                ),
                IpAddr::V6(group) => (
                    SocketOption::Ipv6JoinGroup { group, iface: 1 },
                    SocketOption::Ipv6MulticastIf(1),
                    SocketOption::Ipv6MulticastAll(false),
                ),
            };
            k.set_option(lo, join_lo).unwrap();
            k.set_option(lo, send_lo).unwrap();
            k.set_option(lo, all.clone()).unwrap();
            k.set_option(link, all).unwrap();
            let dst = SocketAddr::new(group, 9000);
            send(&mut k, lo, dst, b"loopback");
            let mut out = Vec::new();
            k.egress(&mut out);
            assert!(out.is_empty());
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
    fn broadcast_filters_destination_instead_of_bind_interface() {
        let mut k = Kernel::new();
        let local = "10.0.0.1".parse().unwrap();
        k.add_address(local);
        let link = reuse_bind(&mut k, local, 9000);
        let lo = reuse_bind(&mut k, Ipv4Addr::LOCALHOST.into(), 9000);
        let wildcard = reuse_bind(&mut k, Ipv4Addr::UNSPECIFIED.into(), 9000);
        k.set_option(link, SocketOption::Broadcast(true)).unwrap();
        send(
            &mut k,
            link,
            "255.255.255.255:9000".parse().unwrap(),
            b"link",
        );
        let mut out = Vec::new();
        k.egress(&mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(pending(&k, link), 0);
        assert_eq!(pending(&k, lo), 0);
        assert_eq!(pending(&k, wildcard), 1);
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
            let fd = bind(&mut k, unspecified(local), 9000);
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
            let link = bind(&mut k, link_ip, 0);
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
            let receiver = bind(&mut k, unspecified(local), 9000);
            join(&mut k, receiver, group);
            let sender = k.open(domain, Type::Dgram);
            send(&mut k, sender, SocketAddr::new(group, 9000), b"auto-bind");
            let mut out = Vec::new();
            k.egress(&mut out);
            assert!(out.is_empty());
            assert_eq!(pending(&k, receiver), 1);
        }
    }

    #[test]
    fn shared_port_membership_all_and_last_member_close() {
        for group in ["239.10.10.10", "ff05::1234"] {
            let group: IpAddr = group.parse().unwrap();
            let mut k = Kernel::new();
            let local = if group.is_ipv4() {
                "10.0.0.1"
            } else {
                "fd00::1"
            };
            k.add_address(local.parse().unwrap());
            let first = reuse_bind(&mut k, unspecified(group), 9000);
            let second = reuse_bind(&mut k, group, 9000);
            let nonmember = reuse_bind(&mut k, unspecified(group), 9000);
            let sender = bind(&mut k, unspecified(group), 0);
            join(&mut k, first, group);
            join(&mut k, second, group);
            let dst = SocketAddr::new(group, 9000);

            send(&mut k, sender, dst, b"default all");
            k.egress(&mut Vec::new());
            for fd in [first, second, nonmember] {
                assert_eq!(pending(&k, fd), 1);
            }
            let all = if group.is_ipv4() {
                SocketOption::IpMulticastAll
            } else {
                SocketOption::Ipv6MulticastAll
            };
            k.set_option(nonmember, all(false)).unwrap();
            send(&mut k, sender, dst, b"per socket");
            k.egress(&mut Vec::new());
            assert_eq!(pending(&k, first), 2);
            assert_eq!(pending(&k, second), 2);
            assert_eq!(pending(&k, nonmember), 1);

            k.close(first);
            k.set_option(nonmember, all(true)).unwrap();
            send(&mut k, sender, dst, b"one member");
            k.egress(&mut Vec::new());
            assert_eq!(pending(&k, second), 3);
            assert_eq!(pending(&k, nonmember), 2);
            k.close(second);
            send(&mut k, sender, dst, b"no members");
            k.egress(&mut Vec::new());
            assert_eq!(pending(&k, nonmember), 2);
        }
    }

    #[test]
    fn ipv4_loopback_device_delivery_survives_disabled_loop_flag() {
        let mut k = Kernel::new();
        k.add_address("10.0.0.1".parse().unwrap());
        let receiver = bind(&mut k, Ipv4Addr::UNSPECIFIED.into(), 9000);
        let group = "239.10.10.10".parse().unwrap();
        k.set_option(
            receiver,
            SocketOption::IpAddMembership {
                group,
                iface: Ipv4Addr::LOCALHOST,
            },
        )
        .unwrap();
        let sender = bind(&mut k, Ipv4Addr::UNSPECIFIED.into(), 0);
        k.set_option(sender, SocketOption::IpMulticastIf(Ipv4Addr::LOCALHOST))
            .unwrap();
        k.set_option(sender, SocketOption::IpMulticastLoop(false))
            .unwrap();
        for ttl in [0, 1] {
            k.set_option(sender, SocketOption::IpMulticastTtl(ttl))
                .unwrap();
            send(
                &mut k,
                sender,
                SocketAddr::new(group.into(), 9000),
                b"loopback device",
            );
        }
        let mut out = Vec::new();
        k.egress(&mut out);
        assert!(out.is_empty());
        assert_eq!(pending(&k, receiver), 2);
    }

    #[test]
    fn bind_options_are_applied_before_udp_port_conflicts() {
        for reuse in [SocketOption::ReuseAddr(true), SocketOption::ReusePort(true)] {
            let mut k = Kernel::new();
            let first = k.open(Domain::Inet, Type::Dgram);
            let second = k.open(Domain::Inet, Type::Dgram);
            let addr = Addr::Inet("0.0.0.0:9000".parse().unwrap());
            k.set_option(first, reuse.clone()).unwrap();
            k.bind_socket(first, &addr).unwrap();
            assert_eq!(
                k.bind_socket(second, &addr).unwrap_err().kind(),
                ErrorKind::AddrInUse
            );
            // A failed bind leaves the descriptor usable.
            k.set_option(second, reuse).unwrap();
            k.bind_socket(second, &addr).unwrap();
            assert_eq!(k.local_addr(first).unwrap(), k.local_addr(second).unwrap());
            assert_eq!(
                k.bind_socket(second, &addr).unwrap_err().kind(),
                ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn multicast_output_alias_and_interface_can_change_after_autobind() {
        let mut k = Kernel::new();
        k.add_address("10.0.0.1".parse().unwrap());
        let alias = "10.0.0.2".parse().unwrap();
        k.add_address(IpAddr::V4(alias));
        let sender = k.open(Domain::Inet, Type::Dgram);
        let dst = "239.10.10.10:9000".parse().unwrap();
        k.set_option(sender, SocketOption::IpMulticastIf(alias))
            .unwrap();
        send(&mut k, sender, dst, b"alias");
        let mut out = Vec::new();
        k.egress(&mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].src, alias);
        let Addr::Inet(local) = k.local_addr(sender).unwrap() else {
            unreachable!()
        };
        assert!(local.ip().is_unspecified());

        let receiver = bind(&mut k, Ipv4Addr::UNSPECIFIED.into(), 9000);
        k.set_option(
            receiver,
            SocketOption::IpAddMembership {
                group: "239.10.10.10".parse().unwrap(),
                iface: Ipv4Addr::LOCALHOST,
            },
        )
        .unwrap();
        k.set_option(sender, SocketOption::IpMulticastIf(Ipv4Addr::LOCALHOST))
            .unwrap();
        send(&mut k, sender, dst, b"lo");
        k.egress(&mut out);
        assert_eq!(
            out.len(),
            1,
            "changing interface must not retain the old source"
        );
        assert_eq!(pending(&k, receiver), 1);
    }

    #[test]
    fn reused_udp_ports_select_one_unicast_receiver_per_flow() {
        for reuse_port in [false, true] {
            let mut k = Kernel::new();
            let mut receivers = Vec::new();
            for _ in 0..2 {
                let fd = k.open(Domain::Inet, Type::Dgram);
                k.set_option(
                    fd,
                    if reuse_port {
                        SocketOption::ReusePort(true)
                    } else {
                        SocketOption::ReuseAddr(true)
                    },
                )
                .unwrap();
                k.bind_socket(fd, &Addr::Inet("127.0.0.1:9000".parse().unwrap()))
                    .unwrap();
                receivers.push(fd);
            }
            for port in 10000..10032 {
                let sender = bind(&mut k, Ipv4Addr::LOCALHOST.into(), port);
                for _ in 0..2 {
                    send(
                        &mut k,
                        sender,
                        "127.0.0.1:9000".parse().unwrap(),
                        b"same flow",
                    );
                }
            }
            k.egress(&mut Vec::new());
            let counts = receivers
                .iter()
                .map(|&fd| pending(&k, fd))
                .collect::<Vec<_>>();
            assert_eq!(counts.iter().sum::<usize>(), 64);
            if reuse_port {
                assert!(counts.iter().all(|&n| n > 0));
                // Both datagrams from each flow reach exactly one receiver.
                for &fd in &receivers {
                    let queue = &k.lookup(fd).unwrap().recv_queue;
                    for chunk in queue.as_slices().0.as_chunks::<2>().0 {
                        assert_eq!(chunk[0].0, chunk[1].0);
                    }
                }
            } else {
                assert_eq!(counts, [0, 64]);
            }
        }
    }

    #[test]
    fn indexed_unicast_preserves_peer_and_address_precedence() {
        for (local, remote) in [("10.0.0.1", "10.0.0.2"), ("fd00::1", "fd00::2")] {
            let local = local.parse().unwrap();
            let remote = remote.parse().unwrap();
            let mut k = Kernel::new();
            k.add_address(local);
            let wildcard = reuse_bind(&mut k, unspecified(local), 9000);
            let exact = reuse_bind(&mut k, local, 9000);
            let pkt = Packet {
                src: remote,
                dst: local,
                ttl: 64,
                payload: Transport::Udp(UdpDatagram {
                    src_port: 8000,
                    dst_port: 9000,
                    payload: Bytes::from_static(b"unicast"),
                }),
            };
            k.deliver(pkt.clone());
            assert_eq!((pending(&k, exact), pending(&k, wildcard)), (1, 0));

            // A connected wildcard outranks an unconnected specific bind.
            let peer = Addr::Inet(SocketAddr::new(remote, 8000));
            k.lookup_mut(wildcard).unwrap().peer = Some(peer.clone());
            k.deliver(pkt.clone());
            assert_eq!((pending(&k, exact), pending(&k, wildcard)), (1, 1));

            k.lookup_mut(exact).unwrap().peer = Some(peer);
            k.deliver(pkt.clone());
            assert_eq!((pending(&k, exact), pending(&k, wildcard)), (2, 1));

            // Rejecting one connected candidate must still try the other bind.
            let other = Addr::Inet(SocketAddr::new(remote, 8001));
            k.lookup_mut(exact).unwrap().peer = Some(other.clone());
            k.deliver(pkt.clone());
            assert_eq!((pending(&k, exact), pending(&k, wildcard)), (2, 2));
            k.lookup_mut(wildcard).unwrap().peer = Some(other);
            k.deliver(pkt);
            assert_eq!((pending(&k, exact), pending(&k, wildcard)), (2, 2));
        }
    }

    #[test]
    fn failed_joins_and_leaves_do_not_leak_host_membership() {
        let mut k = Kernel::new();
        k.add_address("10.0.0.1".parse().unwrap());
        let receiver = bind(&mut k, Ipv4Addr::UNSPECIFIED.into(), 9000);
        // Membership can belong to an unbound socket on a different port.
        let member = k.open(Domain::Inet, Type::Dgram);
        let sender = bind(&mut k, Ipv4Addr::UNSPECIFIED.into(), 0);
        let group = "239.10.10.10".parse().unwrap();
        let join = SocketOption::IpAddMembership {
            group,
            iface: Ipv4Addr::UNSPECIFIED,
        };
        let leave = SocketOption::IpDropMembership {
            group,
            iface: Ipv4Addr::UNSPECIFIED,
        };
        let dst = SocketAddr::new(group.into(), 9000);
        k.set_option(member, join.clone()).unwrap();
        assert_eq!(
            k.set_option(member, join.clone()).unwrap_err().kind(),
            ErrorKind::AddrInUse
        );
        send(&mut k, sender, dst, b"joined");
        k.egress(&mut Vec::new());
        assert_eq!(pending(&k, receiver), 1);

        k.set_option(member, leave.clone()).unwrap();
        assert_eq!(
            k.set_option(member, leave).unwrap_err().kind(),
            ErrorKind::AddrNotAvailable
        );
        send(&mut k, sender, dst, b"left");
        k.egress(&mut Vec::new());
        assert_eq!(pending(&k, receiver), 1);

        k.set_option(member, join).unwrap();
        k.close(member);
        k.close(member);
        send(&mut k, sender, dst, b"closed");
        k.egress(&mut Vec::new());
        assert_eq!(pending(&k, receiver), 1);
    }
}
