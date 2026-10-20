use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use turmoil_net::fixture::{self, ClientServer};
use turmoil_net::shim::tokio::net::UdpSocket;
use turmoil_net::{Packet, Verdict};

const V4_GROUP: Ipv4Addr = Ipv4Addr::new(239, 0, 0, 255);
const V6_GROUP: Ipv6Addr = Ipv6Addr::new(0xff08, 0, 0, 0, 0, 0, 0, 1);

fn join(socket: &UdpSocket, group: IpAddr) {
    match group {
        IpAddr::V4(group) => socket.join_multicast_v4(group, Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(group) => socket.join_multicast_v6(&group, 0),
    }
    .unwrap();
}

fn leave(socket: &UdpSocket, group: IpAddr) {
    match group {
        IpAddr::V4(group) => socket.leave_multicast_v4(group, Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(group) => socket.leave_multicast_v6(&group, 0),
    }
    .unwrap();
}

fn set_loop(socket: &UdpSocket, group: IpAddr, enabled: bool) {
    match group {
        IpAddr::V4(_) => socket.set_multicast_loop_v4(enabled),
        IpAddr::V6(_) => socket.set_multicast_loop_v6(enabled),
    }
    .unwrap();
}

async fn recv(socket: &UdpSocket, expected: &[u8]) -> SocketAddr {
    let mut buf = [0; 32];
    let (n, from) = tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut buf))
        .await
        .expect("datagram was not delivered")
        .unwrap();
    assert_eq!(&buf[..n], expected);
    assert!(!from.ip().is_unspecified());
    from
}

async fn empty(socket: &UdpSocket) {
    let mut buf = [0; 32];
    assert!(
        tokio::time::timeout(Duration::from_millis(10), socket.recv_from(&mut buf))
            .await
            .is_err(),
        "unexpected datagram"
    );
}

#[test]
fn multicast_loop_membership_and_close() {
    for group in [V4_GROUP.into(), V6_GROUP.into()] {
        fixture::lo(async move {
            let local = match group {
                IpAddr::V4(_) => "0.0.0.0:9000",
                IpAddr::V6(_) => "[::]:9000",
            };
            let socket = UdpSocket::bind(local).await.unwrap();
            let dst = SocketAddr::new(group, 9000);
            assert!(socket.multicast_loop_v4().unwrap());
            assert!(socket.multicast_loop_v6().unwrap());

            join(&socket, group);
            join(&socket, group); // Repeated joins stay idempotent.
            socket.send_to(b"default", dst).await.unwrap();
            // Changing the flag cannot retroactively cancel this send.
            set_loop(&socket, group, false);
            recv(&socket, b"default").await;
            empty(&socket).await; // No duplicate from the second join.

            socket.send_to(b"disabled", dst).await.unwrap();
            set_loop(&socket, group, true);
            empty(&socket).await;
            socket.send_to(b"enabled", dst).await.unwrap();
            recv(&socket, b"enabled").await;

            leave(&socket, group);
            socket.send_to(b"left", dst).await.unwrap();
            empty(&socket).await;

            join(&socket, group);
            drop(socket);
            let socket = UdpSocket::bind(local).await.unwrap();
            socket.send_to(b"closed", dst).await.unwrap();
            empty(&socket).await;
        });
    }
}

#[test]
fn multicast_validates_groups_and_interfaces() {
    fixture::lo(async {
        let v4 = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let v6 = UdpSocket::bind("[::]:0").await.unwrap();
        for result in [
            v4.join_multicast_v4(Ipv4Addr::LOCALHOST, Ipv4Addr::UNSPECIFIED),
            v4.leave_multicast_v4(Ipv4Addr::LOCALHOST, Ipv4Addr::UNSPECIFIED),
            v6.join_multicast_v6(&Ipv6Addr::LOCALHOST, 0),
            v6.leave_multicast_v6(&Ipv6Addr::LOCALHOST, 0),
            v4.join_multicast_v6(&V6_GROUP, 0),
            v6.join_multicast_v4(V4_GROUP, Ipv4Addr::UNSPECIFIED),
        ] {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidInput);
        }
        for result in [
            v4.join_multicast_v4(V4_GROUP, Ipv4Addr::new(10, 0, 0, 99)),
            v4.leave_multicast_v4(V4_GROUP, Ipv4Addr::new(10, 0, 0, 99)),
            v6.join_multicast_v6(&V6_GROUP, 1),
            v6.leave_multicast_v6(&V6_GROUP, 1),
            v4.leave_multicast_v4(V4_GROUP, Ipv4Addr::UNSPECIFIED),
            v6.leave_multicast_v6(&V6_GROUP, 0),
        ] {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::AddrNotAvailable);
        }
        // Loopback is an implicit local address.
        v4.join_multicast_v4(V4_GROUP, Ipv4Addr::LOCALHOST).unwrap();
        v4.leave_multicast_v4(V4_GROUP, Ipv4Addr::LOCALHOST)
            .unwrap();
        assert_eq!(v4.multicast_ttl_v4().unwrap(), 1);
        v4.set_multicast_ttl_v4(7).unwrap();
        assert_eq!(v4.multicast_ttl_v4().unwrap(), 7);
        assert_eq!(
            v4.set_multicast_ttl_v4(256).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    });
}

#[test]
fn cast_reaches_multiple_hosts() {
    for dst in [
        SocketAddr::new(Ipv4Addr::BROADCAST.into(), 9000),
        SocketAddr::new(V4_GROUP.into(), 9000),
        SocketAddr::new(V6_GROUP.into(), 9000),
    ] {
        let v4 = dst.is_ipv4();
        let wildcard = if v4 { "0.0.0.0:9000" } else { "[::]:9000" };
        let hosts = if v4 {
            ["10.0.0.1", "10.0.0.2", "10.0.0.3"]
        } else {
            ["fd00::1", "fd00::2", "fd00::3"]
        };
        let mut fixture = ClientServer::new();
        for host in &hosts[..2] {
            fixture = fixture.server(*host, async move {
                let socket = UdpSocket::bind(wildcard).await.unwrap();
                if dst.ip().is_multicast() {
                    join(&socket, dst.ip());
                }
                let from = recv(&socket, b"cast").await;
                socket.send_to(b"ack", from).await.unwrap();
            });
        }
        fixture.run(hosts[2], async move {
            let socket = UdpSocket::bind(wildcard).await.unwrap();
            if dst.ip().is_multicast() {
                join(&socket, dst.ip());
            } else {
                assert!(!socket.broadcast().unwrap());
                assert_eq!(
                    socket.send_to(b"denied", dst).await.unwrap_err().kind(),
                    ErrorKind::PermissionDenied
                );
                socket.set_broadcast(true).unwrap();
            }
            socket.send_to(b"cast", dst).await.unwrap();
            // The sender gets exactly one local copy, then two replies.
            recv(&socket, b"cast").await;
            let first = recv(&socket, b"ack").await;
            let second = recv(&socket, b"ack").await;
            assert_ne!(first.ip(), second.ip());
            empty(&socket).await;
            if !dst.ip().is_multicast() {
                socket.set_broadcast(false).unwrap();
                assert!(!socket.broadcast().unwrap());
                assert_eq!(
                    socket.try_send_to(b"denied again", dst).unwrap_err().kind(),
                    ErrorKind::PermissionDenied
                );
            }
        });
    }
}

#[test]
fn multicast_rules_affect_remote_delivery_only() {
    ClientServer::new()
        .server("server", async {
            let socket = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            join(&socket, V4_GROUP.into());
            let from = recv(&socket, b"delayed").await;
            socket.send_to(b"ack", from).await.unwrap();
        })
        .run("client", async {
            let socket = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            join(&socket, V4_GROUP.into());
            let rule = turmoil_net::rule(|pkt: &Packet| {
                if pkt.dst.is_multicast() {
                    Verdict::Drop
                } else {
                    Verdict::Pass
                }
            });
            socket.send_to(b"dropped", (V4_GROUP, 9000)).await.unwrap();
            recv(&socket, b"dropped").await;
            drop(rule);

            let _rule = turmoil_net::rule(|pkt: &Packet| {
                if pkt.dst.is_multicast() {
                    Verdict::Deliver(Duration::from_millis(20))
                } else {
                    Verdict::Pass
                }
            });
            socket.send_to(b"delayed", (V4_GROUP, 9000)).await.unwrap();
            recv(&socket, b"delayed").await;
            empty(&socket).await;
            recv(&socket, b"ack").await;
            empty(&socket).await;
        });
}

#[test]
fn ipv4_memberships_track_configured_addresses_independently() {
    let first = Ipv4Addr::new(10, 0, 0, 2);
    let second = Ipv4Addr::new(10, 0, 0, 3);
    ClientServer::new()
        .server("10.0.0.1", async {
            let socket = UdpSocket::bind("0.0.0.0:9001").await.unwrap();
            let mut buf = [0; 32];
            loop {
                let (n, _) = socket.recv_from(&mut buf).await.unwrap();
                socket.send_to(&buf[..n], (V4_GROUP, 9000)).await.unwrap();
            }
        })
        .run([IpAddr::V4(first), IpAddr::V4(second)], async move {
            let socket = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            socket
                .join_multicast_v4(V4_GROUP, Ipv4Addr::UNSPECIFIED)
                .unwrap();
            socket.join_multicast_v4(V4_GROUP, first).unwrap();
            socket.join_multicast_v4(V4_GROUP, second).unwrap();
            // A remote host's address is still not a local interface.
            assert_eq!(
                socket
                    .join_multicast_v4(V4_GROUP, Ipv4Addr::new(10, 0, 0, 1))
                    .unwrap_err()
                    .kind(),
                ErrorKind::AddrNotAvailable
            );

            socket.send_to(b"both", "10.0.0.1:9001").await.unwrap();
            recv(&socket, b"both").await;
            empty(&socket).await; // Several memberships deliver only one copy.

            // Unspecified selected `first`, so an explicit leave removes it.
            socket.leave_multicast_v4(V4_GROUP, first).unwrap();
            assert_eq!(
                socket
                    .leave_multicast_v4(V4_GROUP, Ipv4Addr::UNSPECIFIED)
                    .unwrap_err()
                    .kind(),
                ErrorKind::AddrNotAvailable
            );
            socket.send_to(b"second", "10.0.0.1:9001").await.unwrap();
            recv(&socket, b"second").await;

            socket.leave_multicast_v4(V4_GROUP, second).unwrap();
            socket.send_to(b"neither", "10.0.0.1:9001").await.unwrap();
            empty(&socket).await;

            socket.join_multicast_v4(V4_GROUP, first).unwrap();
            socket.join_multicast_v4(V4_GROUP, second).unwrap();
            drop(socket);
            let socket = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            socket.send_to(b"closed", "10.0.0.1:9001").await.unwrap();
            empty(&socket).await;
        });
}

#[test]
fn ipv4_loopback_membership_is_separate_from_the_shared_link() {
    let local = Ipv4Addr::new(10, 0, 0, 2);
    ClientServer::new()
        .server("10.0.0.1", async {
            let socket = UdpSocket::bind("0.0.0.0:9001").await.unwrap();
            let mut buf = [0; 32];
            loop {
                let (n, _) = socket.recv_from(&mut buf).await.unwrap();
                socket.send_to(&buf[..n], (V4_GROUP, 9000)).await.unwrap();
            }
        })
        .run(IpAddr::V4(local), async move {
            let socket = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            socket
                .join_multicast_v4(V4_GROUP, Ipv4Addr::LOCALHOST)
                .unwrap();
            socket
                .send_to(b"remote link", "10.0.0.1:9001")
                .await
                .unwrap();
            empty(&socket).await;
            socket
                .send_to(b"local link", (V4_GROUP, 9000))
                .await
                .unwrap();
            empty(&socket).await;

            let lo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            lo.send_to(b"loopback", (V4_GROUP, 9000)).await.unwrap();
            assert!(recv(&socket, b"loopback").await.ip().is_loopback());

            // Joining the network link doesn't replace loopback membership.
            socket.join_multicast_v4(V4_GROUP, local).unwrap();
            socket
                .send_to(b"remote link", "10.0.0.1:9001")
                .await
                .unwrap();
            assert!(!recv(&socket, b"remote link").await.ip().is_loopback());
            lo.send_to(b"still loopback", (V4_GROUP, 9000))
                .await
                .unwrap();
            recv(&socket, b"still loopback").await;
            empty(&socket).await;

            socket
                .leave_multicast_v4(V4_GROUP, Ipv4Addr::LOCALHOST)
                .unwrap();
            lo.send_to(b"left loopback", (V4_GROUP, 9000))
                .await
                .unwrap();
            empty(&socket).await;
            socket
                .send_to(b"still network", (V4_GROUP, 9000))
                .await
                .unwrap();
            recv(&socket, b"still network").await;
        });
}

#[test]
fn ipv4_address_ending_in_255_is_unicast() {
    ClientServer::new()
        .server("10.0.0.255", async {
            let socket = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            let from = recv(&socket, b"request").await;
            socket.send_to(b"reply", from).await.unwrap();
        })
        .run("10.0.0.1", async {
            let socket = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            assert!(!socket.broadcast().unwrap());
            socket.send_to(b"request", "10.0.0.255:9000").await.unwrap();
            let from = recv(&socket, b"reply").await;
            assert_eq!(from.ip(), "10.0.0.255".parse::<IpAddr>().unwrap());
            empty(&socket).await;
        });
}
