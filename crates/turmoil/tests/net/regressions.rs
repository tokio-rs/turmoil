//! Regressions found while reviewing the turmoil-net migration.

use std::{io::ErrorKind, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{sleep, timeout},
};
use turmoil::{
    net::{TcpListener, TcpStream, UdpSocket},
    Builder, IpVersion, Result, Transport,
};
use turmoil_net::{NetstatState, Verdict};

fn builder() -> Builder {
    let mut b = Builder::new();
    b.rng_seed(42)
        .min_message_latency(Duration::ZERO)
        .max_message_latency(Duration::ZERO)
        .simulation_duration(Duration::from_secs(20));
    b
}

#[test]
fn loopback_bound_casts_do_not_create_self_links() -> Result {
    for extra_host in [false, true] {
        let mut sim = builder().build();
        if extra_host {
            sim.host("server", std::future::pending);
        }
        sim.client("client", async {
            let socket = UdpSocket::bind("127.0.0.1:0").await?;
            socket.set_broadcast(true)?;
            for target in ["255.255.255.255:1738", "239.10.10.10:1738"] {
                assert_eq!(socket.send_to(b"hello", target).await?, 5);
            }
            // Exercise egress while the sender is still running.
            sleep(Duration::from_millis(20)).await;
            Ok(())
        });
        sim.run()?;
    }
    Ok(())
}

#[test]
fn ipv6_udp_connect_requires_scope_and_receives_scoped_reply() -> Result {
    let mut sim = builder().ip_version(IpVersion::V6).build();
    sim.host("server", || async {
        let socket = UdpSocket::bind("[::]:1738").await?;
        let mut buf = [0; 4];
        let (n, peer) = socket.recv_from(&mut buf).await?;
        assert_eq!(&buf[..n], b"ping");
        socket.send_to(b"pong", peer).await?;
        std::future::pending().await
    });
    sim.client("client", async {
        let socket = UdpSocket::bind("[::]:0").await?;
        // The simulation's fe80::/64 DNS addresses, like Linux link-local
        // peers, require a scope when the socket is bound to a wildcard.
        assert_eq!(
            socket.connect("server:1738").await.unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        let std::net::IpAddr::V6(ip) = turmoil::lookup("server") else {
            unreachable!()
        };
        let peer = std::net::SocketAddrV6::new(ip, 1738, 0, 2);
        socket.connect(peer).await?;
        assert_eq!(socket.peer_addr()?, peer.into());
        socket.send(b"ping").await?;
        let mut buf = [0; 4];
        let n = timeout(Duration::from_secs(1), socket.recv(&mut buf)).await??;
        assert_eq!(&buf[..n], b"pong");
        Ok(())
    });
    sim.run()
}

#[test]
fn timed_out_inbound_handshake_does_not_poison_tuple() -> Result {
    let mut sim = builder().ephemeral_ports(50000..=50000).build();
    sim.host("server", || async {
        let _listener = TcpListener::bind("0.0.0.0:1738").await?;
        std::future::pending().await
    });
    sim.client("client", async {
        turmoil::partition_oneway("server", "client");
        let failed = TcpStream::connect("server:1738").await.unwrap_err();
        assert_eq!(failed.kind(), ErrorKind::TimedOut);
        turmoil::repair_oneway("server", "client");
        sleep(Duration::from_millis(20)).await;
        let retried = timeout(Duration::from_secs(2), TcpStream::connect("server:1738")).await;
        assert!(
            matches!(retried, Ok(Ok(_))),
            "tuple stayed poisoned: {retried:?}"
        );
        Ok(())
    });
    sim.run()
}

#[test]
fn delayed_flow_does_not_delay_another_connection() -> Result {
    let mut sim = builder().build();
    sim.host("server", || async {
        let listener_a = TcpListener::bind("0.0.0.0:1738").await?;
        let listener_b = TcpListener::bind("0.0.0.0:1739").await?;
        let ((mut a, _), (mut b, _)) = tokio::try_join!(listener_a.accept(), listener_b.accept())?;
        tokio::try_join!(
            async {
                let byte = a.read_u8().await?;
                a.write_u8(byte).await
            },
            async {
                let byte = b.read_u8().await?;
                b.write_u8(byte).await
            }
        )?;
        Ok(())
    });
    sim.client("client", async {
        let mut a = TcpStream::connect("server:1738").await?;
        let mut b = TcpStream::connect("server:1739").await?;
        sleep(Duration::from_millis(10)).await;
        let _delay_a = turmoil_net::rule(|packet: &turmoil::Packet| {
            if matches!(&packet.payload, Transport::Tcp(s) if s.dst_port == 1738 && !s.payload.is_empty()) {
                return Verdict::Deliver(Duration::from_secs(1));
            }
            Verdict::Pass
        });
        a.write_u8(1).await?;
        b.write_u8(2).await?;
        let reply = timeout(Duration::from_millis(100), b.read_u8()).await;
        assert!(
            matches!(reply, Ok(Ok(2))),
            "unrelated flow stalled: {reply:?}"
        );
        Ok(())
    });
    sim.run()
}

#[test_case::test_case(false; "retained socket")]
#[test_case::test_case(true; "dropped socket")]
fn lost_final_ack_does_not_turn_clean_eof_into_timeout(drop_closed: bool) -> Result {
    let mut sim = builder().build();
    sim.host("server", || async {
        let listener = TcpListener::bind("0.0.0.0:1738").await?;
        let (mut server, _) = listener.accept().await?;
        assert_eq!(server.read(&mut [0; 1]).await?, 0);
        // Client already half-closed; our FIN will be the second FIN.
        server.shutdown().await?;
        sleep(Duration::from_secs(8)).await;
        let eof = server.read(&mut [0; 1]).await;
        assert!(
            matches!(eof, Ok(0)),
            "clean EOF changed after lost ACK: {eof:?}"
        );
        Ok(())
    });
    sim.client("client", async move {
        let mut client = TcpStream::connect("server:1738").await?;
        sleep(Duration::from_millis(10)).await;
        let mut saw_server_fin = false;
        let mut dropped = false;
        let _drop_final_ack = turmoil_net::rule(move |packet: &turmoil::Packet| {
            if let Transport::Tcp(s) = &packet.payload {
                if s.src_port == 1738 && s.flags.fin {
                    saw_server_fin = true;
                }
                if saw_server_fin
                    && !dropped
                    && s.dst_port == 1738
                    && s.flags.ack
                    && !s.flags.fin
                    && s.payload.is_empty()
                {
                    dropped = true;
                    return Verdict::Drop;
                }
            }
            Verdict::Pass
        });
        client.shutdown().await?;
        assert_eq!(client.read(&mut [0; 1]).await?, 0);
        // Recover with either a live fd or only the close record.
        if drop_closed {
            drop(client);
        }
        sleep(Duration::from_secs(10)).await;
        Ok(())
    });
    sim.run()
}

#[test]
fn dropped_client_eventually_releases_fin_wait_2() -> Result {
    let mut sim = builder()
        .ephemeral_ports(50000..=50000)
        .tick_duration(Duration::from_millis(10))
        .simulation_duration(Duration::from_secs(180))
        .build();
    sim.host("server", || async {
        let listener = TcpListener::bind("0.0.0.0:1738").await?;
        let (_server, _) = listener.accept().await?;
        std::future::pending().await
    });
    sim.client("client", async {
        let client = TcpStream::connect("server:1738").await?;
        sleep(Duration::from_millis(100)).await;
        drop(client);
        sleep(Duration::from_secs(130)).await;
        let snapshot = turmoil_net::netstat(turmoil::lookup("client"));
        assert!(
            !snapshot
                .entries
                .iter()
                .any(|e| e.state == Some(NetstatState::FinWait2)),
            "orphaned FIN_WAIT2 remains after 130 seconds: {snapshot}"
        );
        Ok(())
    });
    sim.run()
}

#[test]
fn netstat_resolves_turmoil_hostnames() -> Result {
    let mut sim = builder().build();
    sim.client("client", async {
        let _socket = UdpSocket::bind("0.0.0.0:0").await?;
        let snapshot = turmoil_net::netstat("client");
        assert_eq!(snapshot.entries.len(), 1);
        Ok(())
    });
    sim.run()
}

#[test]
fn bounce_can_rebind_while_old_connection_closes() -> Result {
    let mut sim = builder().build();
    sim.host("server", || async {
        let listener = TcpListener::bind("0.0.0.0:1738").await?;
        let (_server, _) = listener.accept().await?;
        std::future::pending().await
    });
    sim.client("client", async {
        let _client = TcpStream::connect("server:1738").await?;
        sleep(Duration::from_secs(1)).await;
        Ok(())
    });
    for _ in 0..20 {
        sim.step()?;
    }
    sim.bounce("server");
    sim.run()
}

#[test]
fn same_latency_setter_and_builder_support_slow_links() -> Result {
    let mut sim = builder()
        .tick_duration(Duration::from_millis(10))
        .simulation_duration(Duration::from_secs(30))
        .build();
    sim.host("server", || async {
        let listener = TcpListener::bind("0.0.0.0:1738").await?;
        let (_server, _) = listener.accept().await?;
        std::future::pending().await
    });
    sim.client("client", async {
        let connection = TcpStream::connect("server:1738").await;
        assert!(
            connection.is_ok(),
            "reliable slow link failed: {connection:?}"
        );
        Ok(())
    });
    sim.set_link_latency("server", "client", Duration::from_secs(4));
    sim.run()
}

#[test]
fn handshake_ack_can_carry_the_first_payload() -> Result {
    let mut sim = builder().build();
    sim.host("server", || async {
        let listener = TcpListener::bind("0.0.0.0:1738").await?;
        let (mut server, _) = listener.accept().await?;
        let received = timeout(Duration::from_millis(100), server.read_u8()).await;
        assert!(
            matches!(received, Ok(Ok(7))),
            "ACK's payload was dropped: {received:?}"
        );
        Ok(())
    });
    sim.client("client", async {
        let mut dropped = false;
        let _drop_bare_ack = turmoil_net::rule(move |packet: &turmoil::Packet| {
            if let Transport::Tcp(s) = &packet.payload {
                if !dropped
                    && s.dst_port == 1738
                    && s.flags.ack
                    && !s.flags.syn
                    && !s.flags.fin
                    && s.payload.is_empty()
                {
                    dropped = true;
                    return Verdict::Drop;
                }
            }
            Verdict::Pass
        });
        let mut client = TcpStream::connect("server:1738").await?;
        client.write_u8(7).await?;
        sleep(Duration::from_secs(1)).await;
        Ok(())
    });
    sim.run()
}

#[test_case::test_case(turmoil::IpVersion::V4; "ipv4")]
#[test_case::test_case(turmoil::IpVersion::V6; "ipv6")]
fn udp_reassembles_reordered_fragments(version: turmoil::IpVersion) -> Result {
    let mut sim = builder().ip_version(version.clone()).build();
    let bind = match version {
        turmoil::IpVersion::V4 => "0.0.0.0:1738",
        turmoil::IpVersion::V6 => "[::]:1738",
    };
    sim.host("server", move || async move {
        let socket = UdpSocket::bind(bind).await?;
        let mut buf = vec![0; 8192];
        let (n, peer) = socket.recv_from(&mut buf).await?;
        assert_eq!(n, 4096);
        socket.send_to(&buf[..n], peer).await?;
        Ok(())
    });
    sim.client("client", async move {
        let _reorder = turmoil_net::rule(|packet: &turmoil::Packet| {
            if let Transport::Fragment(fragment) = &packet.payload {
                assert!(packet.size() <= 1500);
                return Verdict::Deliver(Duration::from_millis(
                    30 - (fragment.offset as u64 / 1000).min(20),
                ));
            }
            Verdict::Pass
        });
        let socket = UdpSocket::bind(bind).await?;
        let expected: Vec<_> = (0..4096).map(|i| i as u8).collect();
        assert_eq!(socket.send_to(&expected, "server:1738").await?, 4096);
        let mut received = vec![0; 8192];
        let (n, _) = socket.recv_from(&mut received).await?;
        assert_eq!(&received[..n], expected);
        Ok(())
    });
    sim.run()
}

#[test]
fn losing_one_udp_fragment_discards_only_its_datagram() -> Result {
    let mut sim = builder().build();
    sim.client("server", async {
        let socket = UdpSocket::bind("0.0.0.0:1738").await?;
        let mut buf = vec![0; 8192];
        let (n, _) = socket.recv_from(&mut buf).await?;
        assert_eq!(&buf[..n], &[2; 4096]);
        assert!(
            timeout(Duration::from_millis(100), socket.recv_from(&mut buf))
                .await
                .is_err()
        );
        Ok(())
    });
    sim.client("client", async {
        let mut dropped = false;
        let _lose_fragment = turmoil_net::rule(move |packet: &turmoil::Packet| {
            if let Transport::Fragment(f) = &packet.payload {
                if !dropped && f.offset > 0 && f.more {
                    dropped = true;
                    return Verdict::Drop;
                }
            }
            Verdict::Pass
        });
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.send_to(&[1; 4096], "server:1738").await?;
        socket.send_to(&[2; 4096], "server:1738").await?;
        sleep(Duration::from_millis(200)).await;
        Ok(())
    });
    sim.run()
}

#[test]
fn fragmented_broadcast_respects_each_receivers_link() -> Result {
    let mut sim = builder().build();
    for name in ["reachable", "partitioned"] {
        sim.client(name, async move {
            let socket = UdpSocket::bind("0.0.0.0:1738").await?;
            let mut buf = vec![0; 8192];
            let received = timeout(Duration::from_millis(100), socket.recv_from(&mut buf)).await;
            if name == "partitioned" {
                assert!(received.is_err());
            } else {
                let (n, _) = received??;
                assert_eq!(&buf[..n], &[3; 4096]);
            }
            Ok(())
        });
    }
    sim.client("sender", async {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.set_broadcast(true)?;
        socket.send_to(&[3; 4096], "255.255.255.255:1738").await?;
        sleep(Duration::from_millis(200)).await;
        Ok(())
    });
    sim.partition("sender", "partitioned");
    sim.run()
}

#[test]
fn crash_discards_incomplete_ip_reassembly() -> Result {
    let mut sim = builder().build();
    sim.host("server", || async {
        let socket = UdpSocket::bind("0.0.0.0:1738").await?;
        let mut buf = vec![0; 8192];
        let (n, peer) = socket.recv_from(&mut buf).await?;
        assert_eq!(&buf[..n], &[2; 4096]);
        socket.send_to(b"ok", peer).await?;
        Ok(())
    });
    sim.client("client", async {
        let _delay = turmoil_net::rule(|packet: &turmoil::Packet| {
            if let Transport::Fragment(f) = &packet.payload {
                if f.offset > 0 {
                    return Verdict::Deliver(Duration::from_millis(100));
                }
            }
            Verdict::Pass
        });
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.send_to(&[1; 4096], "server:1738").await?;
        sleep(Duration::from_millis(200)).await;
        socket.send_to(&[2; 4096], "server:1738").await?;
        let mut reply = [0; 2];
        assert_eq!(socket.recv_from(&mut reply).await?.0, 2);
        assert_eq!(&reply, b"ok");
        Ok(())
    });
    for _ in 0..20 {
        sim.step()?;
    }
    sim.crash("server");
    sim.bounce("server");
    sim.run()
}
