//! The same socket scenarios run against the shim and real Linux Tokio.
//!
//! Normal `cargo test` uses the simulator. See `linux/udp_cast.py` for
//! running the real-socket build inside a private network namespace.

use std::future::Future;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::Duration;

#[cfg(parity_tokio)]
use tokio::net::UdpSocket;
#[cfg(not(parity_tokio))]
use turmoil_net::shim::tokio::net::UdpSocket;

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 10, 10, 10);
const LOCAL: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
const ALIAS: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 2);
const GROUP6: Ipv6Addr = Ipv6Addr::new(0xff05, 0, 0, 0, 0, 0, 0, 0x1234);

fn run(test: impl Future<Output = ()> + 'static) {
    #[cfg(not(parity_tokio))]
    turmoil_net::fixture::ClientServer::new()
        .server([] as [IpAddr; 0], async {})
        .run(
            [
                IpAddr::V4(LOCAL),
                IpAddr::V4(ALIAS),
                "fd00::1".parse().unwrap(),
                "fe80::1".parse().unwrap(),
            ],
            test,
        );
    #[cfg(parity_tokio)]
    {
        assert_eq!(
            std::env::var("TURMOIL_NET_PRIVATE_NETNS").as_deref(),
            Ok("1"),
            "run using tests/linux/udp_cast.py"
        );
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(test);
    }
}

async fn received(socket: &UdpSocket) -> bool {
    let mut buf = [0; 8];
    match tokio::time::timeout(Duration::from_millis(40), socket.recv_from(&mut buf)).await {
        Ok(Ok((1, _))) => true,
        Err(_) => false,
        other => panic!("unexpected receive: {other:?}"),
    }
}

#[test]
#[cfg_attr(parity_tokio, ignore = "requires private Linux network namespace")]
fn membership_uses_interfaces_and_routes() {
    run(async {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket
            .join_multicast_v4(GROUP, Ipv4Addr::UNSPECIFIED)
            .unwrap();
        for alias in [LOCAL, ALIAS, Ipv4Addr::UNSPECIFIED] {
            assert_eq!(
                socket.join_multicast_v4(GROUP, alias).unwrap_err().kind(),
                ErrorKind::AddrInUse
            );
        }
        socket
            .join_multicast_v4(GROUP, Ipv4Addr::LOCALHOST)
            .unwrap();
        socket.leave_multicast_v4(GROUP, ALIAS).unwrap();
        assert_eq!(
            socket.leave_multicast_v4(GROUP, LOCAL).unwrap_err().kind(),
            ErrorKind::AddrNotAvailable
        );
        socket
            .leave_multicast_v4(GROUP, Ipv4Addr::LOCALHOST)
            .unwrap();
        let unknown = Ipv4Addr::new(10, 9, 9, 9);
        assert_eq!(
            socket
                .join_multicast_v4(GROUP, unknown)
                .unwrap_err()
                .raw_os_error(),
            Some(19)
        );
        assert_eq!(
            socket
                .leave_multicast_v4(GROUP, unknown)
                .unwrap_err()
                .kind(),
            ErrorKind::AddrNotAvailable
        );
    });
}

#[test]
#[cfg_attr(parity_tokio, ignore = "requires private Linux network namespace")]
fn ipv6_membership_indices_and_zero_leave() {
    run(async {
        let socket = UdpSocket::bind("[::1]:0").await.unwrap();
        socket.join_multicast_v6(&GROUP6, 0).unwrap();
        assert_eq!(
            socket.join_multicast_v6(&GROUP6, 2).unwrap_err().kind(),
            ErrorKind::AddrInUse
        );
        socket.join_multicast_v6(&GROUP6, 1).unwrap();
        socket.leave_multicast_v6(&GROUP6, 0).unwrap();
        assert_eq!(
            socket.leave_multicast_v6(&GROUP6, 1).unwrap_err().kind(),
            ErrorKind::AddrNotAvailable
        );
        socket.leave_multicast_v6(&GROUP6, 2).unwrap();
        assert_eq!(
            socket
                .join_multicast_v6(&GROUP6, 99999)
                .unwrap_err()
                .raw_os_error(),
            Some(19)
        );
    });
}

#[test]
#[cfg_attr(parity_tokio, ignore = "requires private Linux network namespace")]
fn cast_filters_bound_destination() {
    run(async {
        for (group, local, wildcard) in [
            (
                IpAddr::V4(GROUP),
                IpAddr::V4(LOCAL),
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ),
            (
                IpAddr::V6(GROUP6),
                "fd00::1".parse().unwrap(),
                IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            ),
            (
                IpAddr::V4(Ipv4Addr::BROADCAST),
                IpAddr::V4(LOCAL),
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ),
        ] {
            for bind in [wildcard, group, local] {
                let receiver = UdpSocket::bind(SocketAddr::new(bind, 0)).await.unwrap();
                let sender = UdpSocket::bind(SocketAddr::new(local, 0)).await.unwrap();
                if group.is_multicast() {
                    match group {
                        IpAddr::V4(group) => receiver.join_multicast_v4(group, LOCAL).unwrap(),
                        IpAddr::V6(group) => receiver.join_multicast_v6(&group, 2).unwrap(),
                    }
                } else {
                    assert_eq!(receiver.local_addr().unwrap().ip(), bind);
                    let dst = SocketAddr::new(group, receiver.local_addr().unwrap().port());
                    assert_eq!(
                        sender.send_to(b"x", dst).await.unwrap_err().kind(),
                        ErrorKind::PermissionDenied
                    );
                    sender.set_broadcast(true).unwrap();
                }
                sender
                    .send_to(
                        b"x",
                        SocketAddr::new(group, receiver.local_addr().unwrap().port()),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    received(&receiver).await,
                    bind != local,
                    "group={group}, bind={bind}"
                );
            }
        }
    });
}

#[test]
#[cfg_attr(parity_tokio, ignore = "requires private Linux network namespace")]
fn multicast_all_uses_host_membership_and_close() {
    run(async {
        for v4 in [true, false] {
            let wildcard = if v4 { "0.0.0.0:0" } else { "[::]:0" };
            let member = UdpSocket::bind(wildcard).await.unwrap();
            let receiver = UdpSocket::bind(wildcard).await.unwrap();
            let sender = UdpSocket::bind(wildcard).await.unwrap();
            let group = if v4 {
                member.join_multicast_v4(GROUP, LOCAL).unwrap();
                IpAddr::V4(GROUP)
            } else {
                member.join_multicast_v6(&GROUP6, 2).unwrap();
                IpAddr::V6(GROUP6)
            };
            let dst = SocketAddr::new(group, receiver.local_addr().unwrap().port());
            sender.send_to(b"x", dst).await.unwrap();
            assert!(received(&receiver).await);
            drop(member);
            sender.send_to(b"x", dst).await.unwrap();
            assert!(!received(&receiver).await);
        }
    });
}

#[test]
#[cfg_attr(parity_tokio, ignore = "requires private Linux network namespace")]
fn sender_loop_flag_and_zero_ttl_on_link() {
    run(async {
        let receiver = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        receiver.join_multicast_v4(GROUP, LOCAL).unwrap();
        receiver.set_multicast_loop_v4(false).unwrap();
        let sender = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let dst = SocketAddr::new(GROUP.into(), receiver.local_addr().unwrap().port());
        for ttl in [0, 1] {
            sender.set_multicast_ttl_v4(ttl).unwrap();
            for enabled in [false, true] {
                sender.set_multicast_loop_v4(enabled).unwrap();
                sender.send_to(b"x", dst).await.unwrap();
                assert_eq!(received(&receiver).await, enabled);
            }
        }
    });
}

#[test]
#[cfg_attr(parity_tokio, ignore = "requires private Linux network namespace")]
fn broadcast_bind_filters_unicast_and_retains_local_address() {
    run(async {
        let receiver = UdpSocket::bind("255.255.255.255:0").await.unwrap();
        assert_eq!(receiver.local_addr().unwrap().ip(), Ipv4Addr::BROADCAST);
        let sender = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        sender
            .send_to(b"x", (LOCAL, receiver.local_addr().unwrap().port()))
            .await
            .unwrap();
        assert!(!received(&receiver).await);
    });
}

#[test]
#[cfg_attr(parity_tokio, ignore = "requires private Linux network namespace")]
fn multicast_bound_sender_uses_unicast_source() {
    run(async {
        let socket = UdpSocket::bind((GROUP, 0)).await.unwrap();
        socket.join_multicast_v4(GROUP, LOCAL).unwrap();
        socket
            .send_to(b"x", socket.local_addr().unwrap())
            .await
            .unwrap();
        let mut buf = [0; 8];
        let (_, source) = tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(source.ip(), LOCAL);
        assert_eq!(socket.local_addr().unwrap().ip(), GROUP);
    });
}

#[test]
#[cfg_attr(parity_tokio, ignore = "requires private Linux network namespace")]
fn ipv6_link_local_sender_reports_ingress_interface() {
    run(async {
        let receiver = UdpSocket::bind("[::]:0").await.unwrap();
        receiver.join_multicast_v6(&GROUP6, 2).unwrap();
        let link_local = "fe80::1".parse().unwrap();
        let sender = UdpSocket::bind(SocketAddrV6::new(link_local, 0, 0, 2))
            .await
            .unwrap();
        let dst = SocketAddrV6::new(GROUP6, receiver.local_addr().unwrap().port(), 0, 2);
        sender.send_to(b"x", dst).await.unwrap();
        let mut buf = [0; 8];
        let (_, source) =
            tokio::time::timeout(Duration::from_secs(1), receiver.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
        let SocketAddr::V6(source) = source else {
            panic!("expected IPv6 source")
        };
        assert_eq!(*source.ip(), link_local);
        assert_eq!(source.scope_id(), 2);
    });
}
