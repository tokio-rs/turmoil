use bytes::Bytes;
use indexmap::{IndexMap, IndexSet};
use tokio::{
    sync::{mpsc, Mutex},
    time::sleep,
};

use crate::{
    envelope::{Datagram, Envelope, Protocol},
    host::is_same,
    ToSocketAddrs, World, TRACING_TARGET,
};

use std::{
    cmp,
    io::{self, Error, ErrorKind, Result},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

/// A simulated UDP socket.
///
/// All methods must be called from a host within a Turmoil simulation.
pub struct UdpSocket {
    local_addr: SocketAddr,
    rx: Mutex<Rx>,
}

impl std::fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpSocket")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

#[derive(Debug, Default)]
pub(crate) struct MulticastGroups(IndexMap<SocketAddr, IndexSet<SocketAddr>>);

impl MulticastGroups {
    fn destination_addresses(&self, group: SocketAddr) -> IndexSet<SocketAddr> {
        self.0.get(&group).cloned().unwrap_or_default()
    }

    fn contains_destination_address(&self, group: IpAddr, member: SocketAddr) -> bool {
        self.0
            .get(&SocketAddr::new(group, member.port()))
            .and_then(|members| members.get(&member))
            .is_some()
    }

    fn join(&mut self, group: IpAddr, member: SocketAddr) {
        self.0
            .entry(SocketAddr::new(group, member.port()))
            .and_modify(|members| {
                members.insert(member);
                tracing::info!(target: TRACING_TARGET, ?member, group = ?group, protocol = %"UDP", "Join multicast group");
            })
            .or_insert_with(|| IndexSet::from([member]));
    }

    fn leave(&mut self, group: IpAddr, member: SocketAddr) {
        let index = self
            .0
            .entry(SocketAddr::new(group, member.port()))
            .and_modify(|members| {
                members.swap_remove(&member);
                tracing::info!(target: TRACING_TARGET, ?member, group = ?group, protocol = %"UDP", "Leave multicast group");
            })
            .index();

        if self
            .0
            .get_index(index)
            .map(|(_, members)| members.is_empty())
            .unwrap_or(false)
        {
            self.0.swap_remove_index(index);
        }
    }

    fn leave_all(&mut self, member: SocketAddr) {
        for (group, members) in self.0.iter_mut() {
            members.swap_remove(&member);
            tracing::info!(target: TRACING_TARGET, ?member, group = ?group, protocol = %"UDP", "Leave multicast group");
        }
        self.0.retain(|_, members| !members.is_empty());
    }
}

struct Rx {
    recv: mpsc::Receiver<(Datagram, SocketAddr)>,
    /// A buffered received message.
    ///
    /// This is used to support the `readable` method, as [`mpsc::Receiver`]
    /// doesn't expose a way to query channel readiness.
    buffer: Option<(Datagram, SocketAddr)>,
}

impl Rx {
    /// Tries to receive from either the buffered message or the mpsc channel
    pub fn try_recv_from(&mut self, buf: &mut [u8]) -> Result<(usize, Datagram, SocketAddr)> {
        let (datagram, origin) = if let Some(datagram) = self.buffer.take() {
            datagram
        } else {
            self.recv.try_recv().map_err(|_| {
                io::Error::new(io::ErrorKind::WouldBlock, "socket receive queue is empty")
            })?
        };

        let bytes = &datagram.0;
        let limit = cmp::min(buf.len(), bytes.len());

        buf[..limit].copy_from_slice(&bytes[..limit]);

        Ok((limit, datagram, origin))
    }

    /// Waits for the socket to become readable.
    ///
    /// This function is usually paired with `try_recv_from()`.
    ///
    /// The function may complete without the socket being readable. This is a
    /// false-positive and attempting a `try_recv_from()` will return with
    /// `io::ErrorKind::WouldBlock`.
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe. Once a readiness event occurs, the method
    /// will continue to return immediately until the readiness event is
    /// consumed by an attempt to read that fails with `WouldBlock` or
    /// `Poll::Pending`.
    pub async fn readable(&mut self) -> Result<()> {
        if self.buffer.is_some() {
            return Ok(());
        }

        let datagram = self
            .recv
            .recv()
            .await
            .expect("sender should never be dropped");

        self.buffer = Some(datagram);

        Ok(())
    }
}

impl UdpSocket {
    pub(crate) fn new(local_addr: SocketAddr, rx: mpsc::Receiver<(Datagram, SocketAddr)>) -> Self {
        Self {
            local_addr,
            rx: Mutex::new(Rx {
                recv: rx,
                buffer: None,
            }),
        }
    }

    /// This function will create a new UDP socket and attempt to bind it to
    /// the `addr` provided.
    ///
    /// Binding with a port number of 0 will request that the OS assigns a port
    /// to this listener. The port allocated can be queried via the `local_addr`
    /// method.
    ///
    /// Only `0.0.0.0`, `::`, or localhost are currently supported.
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<UdpSocket> {
        World::current(|world| {
            let mut addr = addr.to_socket_addr(&world.dns);
            let host = world.current_host_mut();

            verify_ipv4_bind_interface(addr.ip(), host.addr)?;

            if addr.port() == 0 {
                addr.set_port(host.assign_ephemeral_port());
            }

            host.udp.bind(addr)
        })
    }

    /// Sends data on the socket to the given address. On success, returns the
    /// number of bytes written.
    ///
    /// Address type can be any implementor of [`ToSocketAddrs`] trait. See its
    /// documentation for concrete examples.
    ///
    /// It is possible for `addr` to yield multiple addresses, but `send_to`
    /// will only send data to the first address yielded by `addr`.
    ///
    /// This will return an error when the IP version of the local socket does
    /// not match that returned from [`ToSocketAddrs`].
    ///
    /// [`ToSocketAddrs`]: crate::ToSocketAddrs
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe. If `send_to` is used as the event in a
    /// [`tokio::select!`](tokio::select) statement and some other branch
    /// completes first, then it is guaranteed that the message was not sent.
    pub async fn send_to<A: ToSocketAddrs>(&self, buf: &[u8], target: A) -> Result<usize> {
        World::current(|world| {
            let dst = target.to_socket_addr(&world.dns);
            self.send(world, dst, Datagram(Bytes::copy_from_slice(buf)))?;
            Ok(buf.len())
        })
    }

    /// Tries to send data on the socket to the given address, but if the send is
    /// blocked this will return right away.
    ///
    /// This function is usually paired with `writable()`.
    ///
    /// # Returns
    ///
    /// If successful, returns the number of bytes sent
    ///
    /// Users should ensure that when the remote cannot receive, the
    /// [`ErrorKind::WouldBlock`] is properly handled. An error can also occur
    /// if the IP version of the socket does not match that of `target`.
    ///
    /// [`ErrorKind::WouldBlock`]: std::io::ErrorKind::WouldBlock
    pub fn try_send_to<A: ToSocketAddrs>(&self, buf: &[u8], target: A) -> Result<usize> {
        World::current(|world| {
            let dst = target.to_socket_addr(&world.dns);
            self.send(world, dst, Datagram(Bytes::copy_from_slice(buf)))?;
            Ok(buf.len())
        })
    }

    /// Waits for the socket to become writable.
    ///
    /// This function is usually paired with `try_send_to()`.
    ///
    /// The function may complete without the socket being writable. This is a
    /// false-positive and attempting a `try_send_to()` will return with
    /// `io::ErrorKind::WouldBlock`.
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe. Once a readiness event occurs, the method
    /// will continue to return immediately until the readiness event is
    /// consumed by an attempt to write that fails with `WouldBlock` or
    /// `Poll::Pending`.
    pub async fn writable(&self) -> Result<()> {
        // UDP sockets currently don't have any backpressure mechanisms so the socket is always writable
        Ok(())
    }

    /// Receives a single datagram message on the socket. On success, returns
    /// the number of bytes read and the origin.
    ///
    /// The function must be called with valid byte array buf of sufficient size
    /// to hold the message bytes. If a message is too long to fit in the
    /// supplied buffer, excess bytes may be discarded.
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let mut rx = self.rx.lock().await;
        rx.readable().await?;

        let (limit, datagram, origin) = rx
            .try_recv_from(buf)
            .expect("queue should be ready after readable yields");

        tracing::trace!(target: TRACING_TARGET, src = ?origin, dst = ?self.local_addr, protocol = %datagram, "Recv");

        Ok((limit, origin))
    }

    /// Tries to receive a single datagram message on the socket. On success,
    /// returns the number of bytes read and the origin.
    ///
    /// The function must be called with valid byte array buf of sufficient size
    /// to hold the message bytes. If a message is too long to fit in the
    /// supplied buffer, excess bytes may be discarded.
    ///
    /// When there is no pending data, `Err(io::ErrorKind::WouldBlock)` is
    /// returned. This function is usually paired with `readable()`.
    pub fn try_recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let mut rx = self.rx.try_lock().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "socket is being read by another task",
            )
        })?;

        let (limit, datagram, origin) = rx.try_recv_from(buf).map_err(|_| {
            io::Error::new(io::ErrorKind::WouldBlock, "socket receive queue is empty")
        })?;

        tracing::trace!(target: TRACING_TARGET, src = ?origin, dst = ?self.local_addr, protocol = %datagram, "Recv");

        Ok((limit, origin))
    }

    /// Waits for the socket to become readable.
    ///
    /// This function is usually paired with `try_recv_from()`.
    ///
    /// The function may complete without the socket being readable. This is a
    /// false-positive and attempting a `try_recv_from()` will return with
    /// `io::ErrorKind::WouldBlock`.
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe. Once a readiness event occurs, the method
    /// will continue to return immediately until the readiness event is
    /// consumed by an attempt to read that fails with `WouldBlock` or
    /// `Poll::Pending`.
    pub async fn readable(&self) -> Result<()> {
        let mut rx = self.rx.lock().await;
        rx.readable().await?;
        Ok(())
    }

    /// Returns the local address that this socket is bound to.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use turmoil::net::UdpSocket;
    /// # use std::{io, net::SocketAddr};
    ///
    /// # #[tokio::main]
    /// # async fn main() -> io::Result<()> {
    /// let addr = "0.0.0.0:8080".parse::<SocketAddr>().unwrap();
    /// let sock = UdpSocket::bind(addr).await?;
    /// // the address the socket is bound to
    /// let local_addr = sock.local_addr()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_addr)
    }

    fn send(&self, world: &mut World, dst: SocketAddr, packet: Datagram) -> Result<()> {
        let mut src = self.local_addr;
        if dst.ip().is_loopback() {
            src.set_ip(dst.ip());
        }
        if src.ip().is_unspecified() {
            src.set_ip(world.current_host_mut().addr);
        }

        match dst {
            SocketAddr::V4(dst) if dst.ip().is_broadcast() => {
                let host = world.current_host();
                match host.udp.is_broadcast_enabled(src.port()) {
                    true => world
                        .hosts
                        .iter()
                        .filter(|(_, host)| host.udp.is_port_assigned(dst.port()))
                        .map(|(addr, _)| SocketAddr::new(*addr, dst.port()))
                        .collect::<Vec<_>>()
                        .into_iter()
                        .try_for_each(|dst| match dst {
                            dst if src.ip() == dst.ip() => {
                                send_loopback(src, dst, Protocol::Udp(packet.clone()));
                                Ok(())
                            }
                            dst => world.send_message(src, dst, Protocol::Udp(packet.clone())),
                        }),
                    false => Err(Error::new(
                        ErrorKind::PermissionDenied,
                        "Broadcast is not enabled",
                    )),
                }
            }
            dst if dst.ip().is_multicast() => world
                .multicast_groups
                .destination_addresses(dst)
                .into_iter()
                .try_for_each(|dst| match dst {
                    dst if src.ip() == dst.ip() => {
                        let host = world.current_host();
                        if host.udp.is_multicast_loop_enabled(dst.port()) {
                            send_loopback(src, dst, Protocol::Udp(packet.clone()));
                        }
                        Ok(())
                    }
                    dst => world.send_message(src, dst, Protocol::Udp(packet.clone())),
                }),
            dst if is_same(src, dst) => {
                send_loopback(src, dst, Protocol::Udp(packet));
                Ok(())
            }
            _ => world.send_message(src, dst, Protocol::Udp(packet)),
        }
    }

    /// Gets the value of the `SO_BROADCAST` option for this socket.
    ///
    /// For more information about this option, see [`set_broadcast`].
    ///
    /// [`set_broadcast`]: method@Self::set_broadcast
    pub fn broadcast(&self) -> io::Result<bool> {
        let local_port = self.local_addr.port();
        World::current(|world| Ok(world.current_host().udp.is_broadcast_enabled(local_port)))
    }

    /// Sets the value of the `SO_BROADCAST` option for this socket.
    ///
    /// When enabled, this socket is allowed to send packets to a broadcast
    /// address.
    pub fn set_broadcast(&self, on: bool) -> io::Result<()> {
        let local_port = match self.local_addr {
            SocketAddr::V4(addr) => addr.port(),
            _ => return Ok(()),
        };
        World::current(|world| {
            world.current_host_mut().udp.set_broadcast(local_port, on);
            Ok(())
        })
    }

    /// Gets the value of the `IP_MULTICAST_LOOP` option for this socket.
    ///
    /// For more information about this option, see [`set_multicast_loop_v4`].
    ///
    /// [`set_multicast_loop_v4`]: method@Self::set_multicast_loop_v4
    pub fn multicast_loop_v4(&self) -> io::Result<bool> {
        let local_port = self.local_addr.port();
        World::current(|world| {
            Ok(world
                .current_host()
                .udp
                .is_multicast_loop_enabled(local_port))
        })
    }

    /// Sets the value of the `IP_MULTICAST_LOOP` option for this socket.
    ///
    /// If enabled, multicast packets will be looped back to the local socket.
    ///
    /// # Note
    ///
    /// This may not have any affect on IPv6 sockets.
    pub fn set_multicast_loop_v4(&self, on: bool) -> io::Result<()> {
        let local_port = match self.local_addr {
            SocketAddr::V4(addr) => addr.port(),
            _ => return Ok(()),
        };
        World::current(|world| {
            world
                .current_host_mut()
                .udp
                .set_multicast_loop(local_port, on);
            Ok(())
        })
    }

    /// Gets the value of the `IPV6_MULTICAST_LOOP` option for this socket.
    ///
    /// For more information about this option, see [`set_multicast_loop_v6`].
    ///
    /// [`set_multicast_loop_v6`]: method@Self::set_multicast_loop_v6
    pub fn multicast_loop_v6(&self) -> io::Result<bool> {
        let local_port = self.local_addr.port();
        World::current(|world| {
            Ok(world
                .current_host()
                .udp
                .is_multicast_loop_enabled(local_port))
        })
    }

    /// Sets the value of the `IPV6_MULTICAST_LOOP` option for this socket.
    ///
    /// Controls whether this socket sees the multicast packets it sends itself.
    ///
    /// # Note
    ///
    /// This may not have any affect on IPv4 sockets.
    pub fn set_multicast_loop_v6(&self, on: bool) -> Result<()> {
        let local_port = match self.local_addr {
            SocketAddr::V6(addr) => addr.port(),
            _ => return Ok(()),
        };
        World::current(|world| {
            world
                .current_host_mut()
                .udp
                .set_multicast_loop(local_port, on);
            Ok(())
        })
    }

    /// Executes an operation of the `IP_ADD_MEMBERSHIP` type.
    ///
    /// This function specifies a new multicast group for this socket to join.
    /// The address must be a valid multicast address, and `interface` is the
    /// address of the local interface with which the system should join the
    /// multicast group. If it's equal to `INADDR_ANY` then an appropriate
    /// interface is chosen by the system.
    ///
    /// Currently, the `interface` argument only supports `127.0.0.1` and `0.0.0.0`.
    pub fn join_multicast_v4(&self, multiaddr: Ipv4Addr, interface: Ipv4Addr) -> Result<()> {
        if !multiaddr.is_multicast() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid multicast address",
            ));
        }

        World::current(|world| {
            let dst = destination_address(world, self);
            verify_ipv4_bind_interface(interface, dst.ip())?;

            world.multicast_groups.join(IpAddr::V4(multiaddr), dst);

            Ok(())
        })
    }

    /// Executes an operation of the `IPV6_ADD_MEMBERSHIP` type.
    ///
    /// This function specifies a new multicast group for this socket to join.
    /// The address must be a valid multicast address, and `interface` is the
    /// index of the interface to join/leave (or 0 to indicate any interface).
    ///
    /// Currently, the `interface` argument only supports `0`.
    pub fn join_multicast_v6(&self, multiaddr: &Ipv6Addr, interface: u32) -> Result<()> {
        verify_ipv6_bind_interface(interface)?;
        if !multiaddr.is_multicast() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid multicast address",
            ));
        }

        World::current(|world| {
            let dst = destination_address(world, self);

            world.multicast_groups.join(IpAddr::V6(*multiaddr), dst);

            Ok(())
        })
    }

    /// Executes an operation of the `IP_DROP_MEMBERSHIP` type.
    ///
    /// For more information about this option, see [`join_multicast_v4`].
    ///
    /// [`join_multicast_v4`]: method@Self::join_multicast_v4
    pub fn leave_multicast_v4(&self, multiaddr: Ipv4Addr, interface: Ipv4Addr) -> io::Result<()> {
        if !multiaddr.is_multicast() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid multicast address",
            ));
        }

        World::current(|world| {
            let dst = destination_address(world, self);
            verify_ipv4_bind_interface(interface, dst.ip())?;

            if !world
                .multicast_groups
                .contains_destination_address(IpAddr::V4(multiaddr), dst)
            {
                return Err(Error::new(
                    ErrorKind::AddrNotAvailable,
                    "Leaving a multicast group that has not been previously joined",
                ));
            }

            world.multicast_groups.leave(IpAddr::V4(multiaddr), dst);

            Ok(())
        })
    }

    /// Executes an operation of the `IPV6_DROP_MEMBERSHIP` type.
    ///
    /// For more information about this option, see [`join_multicast_v6`].
    ///
    /// [`join_multicast_v6`]: method@Self::join_multicast_v6
    pub fn leave_multicast_v6(&self, multiaddr: &Ipv6Addr, interface: u32) -> io::Result<()> {
        verify_ipv6_bind_interface(interface)?;
        if !multiaddr.is_multicast() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid multicast address",
            ));
        }

        World::current(|world| {
            let dst = destination_address(world, self);

            if !world
                .multicast_groups
                .contains_destination_address(IpAddr::V6(*multiaddr), dst)
            {
                return Err(Error::new(
                    ErrorKind::AddrNotAvailable,
                    "Leaving a multicast group that has not been previously joined",
                ));
            }

            world.multicast_groups.leave(IpAddr::V6(*multiaddr), dst);

            Ok(())
        })
    }
}

fn send_loopback(src: SocketAddr, dst: SocketAddr, message: Protocol) {
    tokio::spawn(async move {
        // FIXME: Forces delivery on the next step which better aligns with the
        // remote networking behavior.
        // https://github.com/tokio-rs/turmoil/issues/132
        let tick_duration = World::current(|world| world.tick_duration);
        sleep(tick_duration).await;

        World::current(|world| {
            world
                .current_host_mut()
                .receive_from_network(Envelope { src, dst, message })
                .expect("UDP does not get feedback on delivery errors");
        })
    });
}

fn verify_ipv4_bind_interface<A>(interface: A, addr: IpAddr) -> Result<()>
where
    A: Into<IpAddr>,
{
    let interface = interface.into();

    if !interface.is_unspecified() && !interface.is_loopback() {
        return Err(Error::new(
            ErrorKind::AddrNotAvailable,
            format!("{interface} is not supported"),
        ));
    }

    if interface.is_ipv4() != addr.is_ipv4() {
        panic!("ip version mismatch: {:?} host: {:?}", interface, addr)
    }

    Ok(())
}

fn verify_ipv6_bind_interface(interface: u32) -> Result<()> {
    if interface != 0 {
        return Err(Error::new(
            ErrorKind::AddrNotAvailable,
            format!("interface {interface} is not supported"),
        ));
    }

    Ok(())
}

fn destination_address(world: &World, socket: &UdpSocket) -> SocketAddr {
    let local_port = socket
        .local_addr()
        .expect("local_addr is always present in simulation")
        .port();
    let host_addr = world.current_host().addr;
    SocketAddr::from((host_addr, local_port))
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        World::current_if_set(|world| {
            world
                .multicast_groups
                .leave_all(destination_address(world, self));
            world.current_host_mut().udp.unbind(self.local_addr);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod multicast_group {
        use super::*;

        #[test]
        fn joining_does_not_produce_duplicate_addresses() {
            let member = "[fe80::1]:9000".parse().unwrap();
            let group = "ff08::1".parse().unwrap();
            let mut groups = MulticastGroups::default();
            groups.join(group, member);
            groups.join(group, member);

            let addrs = groups.0.values().flatten().collect::<Vec<_>>();
            assert_eq!(addrs.as_slice(), &[&member]);
        }

        #[test]
        fn leaving_does_not_remove_entire_group() {
            let member1 = "[fe80::1]:9000".parse().unwrap();
            let memeber2 = "[fe80::2]:9000".parse().unwrap();
            let group = "ff08::1".parse().unwrap();
            let mut groups = MulticastGroups::default();
            groups.join(group, member1);
            groups.join(group, memeber2);
            groups.leave(group, memeber2);

            let addrs = groups.0.values().flatten().collect::<Vec<_>>();
            assert_eq!(addrs.as_slice(), &[&member1]);
        }

        #[test]
        fn leaving_removes_empty_group() {
            let member = "[fe80::1]:9000".parse().unwrap();
            let group = "ff08::1".parse().unwrap();
            let mut groups = MulticastGroups::default();
            groups.join(group, member);
            groups.leave(group, member);

            assert_eq!(groups.0.len(), 0);
        }

        #[test]
        fn leaving_removes_empty_groups() {
            let member = "[fe80::1]:9000".parse().unwrap();
            let group1 = "ff08::1".parse().unwrap();
            let group2 = "ff08::2".parse().unwrap();
            let mut groups = MulticastGroups::default();
            groups.join(group1, member);
            groups.join(group2, member);
            groups.leave_all(member);

            assert_eq!(groups.0.len(), 0);
        }
    }
}
