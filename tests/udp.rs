use std::{
    io::{self, ErrorKind},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    rc::Rc,
    sync::{atomic::AtomicUsize, atomic::Ordering},
    time::Duration,
};

use tokio::{
    sync::oneshot,
    time::{self, timeout},
};
use turmoil::{
    lookup,
    net::{self, UdpSocket},
    Builder, IpSubnet, IpSubnets, Ipv4Subnet, Ipv6Subnet, Result,
};

const PORT: u16 = 1738;

fn assert_error_kind<T>(res: io::Result<T>, kind: io::ErrorKind) {
    assert_eq!(res.err().map(|e| e.kind()), Some(kind));
}

async fn bind() -> std::result::Result<net::UdpSocket, std::io::Error> {
    bind_to_v4(PORT).await
}

async fn bind_to_v4(port: u16) -> std::result::Result<net::UdpSocket, std::io::Error> {
    net::UdpSocket::bind((IpAddr::from(Ipv4Addr::UNSPECIFIED), port)).await
}

async fn send_ping(sock: &net::UdpSocket) -> Result<()> {
    sock.send_to(b"ping", (lookup("server")[0], 1738)).await?;

    Ok(())
}

fn try_send_ping(sock: &net::UdpSocket) -> Result<()> {
    sock.try_send_to(b"ping", (lookup("server")[0], 1738))?;

    Ok(())
}

async fn send_pong(sock: &net::UdpSocket, target: SocketAddr) -> Result<()> {
    sock.send_to(b"pong", target).await?;

    Ok(())
}

fn try_send_pong(sock: &net::UdpSocket, target: SocketAddr) -> Result<()> {
    sock.try_send_to(b"pong", target)?;

    Ok(())
}

async fn recv_ping(sock: &net::UdpSocket) -> Result<SocketAddr> {
    let mut buf = vec![123; 8];
    let (_, origin) = sock.recv_from(&mut buf).await?;

    assert_eq!(b"ping", &buf[..4]);
    assert_eq!(&[123; 4], &buf[4..]);

    Ok(origin)
}

fn try_recv_ping(sock: &net::UdpSocket) -> Result<SocketAddr> {
    let mut buf = vec![123; 8];
    let (_, origin) = sock.try_recv_from(&mut buf)?;

    assert_eq!(b"ping", &buf[..4]);
    assert_eq!(&[123; 4], &buf[4..]);

    Ok(origin)
}

async fn recv_pong(sock: &net::UdpSocket) -> Result<()> {
    let mut buf = vec![123; 8];
    sock.recv_from(&mut buf).await?;

    assert_eq!(b"pong", &buf[..4]);
    assert_eq!(&[123; 4], &buf[4..]);

    Ok(())
}

fn try_recv_pong(sock: &net::UdpSocket) -> Result<()> {
    let mut buf = vec![123; 8];
    sock.try_recv_from(&mut buf)?;

    assert_eq!(b"pong", &buf[..4]);
    assert_eq!(&[123; 4], &buf[4..]);

    Ok(())
}

#[test]
fn ping_pong() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        let sock = bind().await?;

        let origin = recv_ping(&sock).await?;
        send_pong(&sock, origin).await
    });

    sim.client("client", async {
        let sock = bind().await?;

        send_ping(&sock).await?;
        recv_pong(&sock).await
    });

    sim.run()
}

#[test]
fn ephemeral_port() -> Result {
    let mut sim = Builder::new().build();

    sim.client("client", async {
        let sock = bind_to_v4(0).await?;

        assert_ne!(sock.local_addr()?.port(), 0);
        assert!(sock.local_addr()?.port() >= 49152);

        Ok(())
    });

    sim.run()
}

#[test]
fn try_ping_pong() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        let sock = bind().await?;

        sock.readable().await?;
        let origin = try_recv_ping(&sock)?;

        sock.writable().await?;
        try_send_pong(&sock, origin)
    });

    sim.client("client", async {
        let sock = bind().await?;

        sock.writable().await?;
        try_send_ping(&sock)?;

        sock.readable().await?;
        try_recv_pong(&sock)
    });

    sim.run()
}

#[test]
fn recv_buf_is_clipped() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let sock = bind().await?;

        let mut buf = vec![0; 8];
        let _ = sock.recv_from(&mut buf).await?;

        assert_eq!(b"hello, w", &buf[..]);

        Ok(())
    });

    // register a client (this is the test code)
    sim.client("client", async move {
        let sock = bind().await?;

        let server_addr = lookup("server")[0];
        sock.send_to(b"hello, world", (server_addr, PORT)).await?;

        Ok(())
    });

    sim.run()
}

#[test]
fn hold_and_release() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let sock = bind().await?;

        while let Ok(origin) = recv_ping(&sock).await {
            let _ = send_pong(&sock, origin).await;
        }

        Ok(())
    });

    sim.client("client", async {
        // pause delivery of packets between the client and server
        turmoil::hold("client", "server");

        let sock = bind().await?;
        send_ping(&sock).await?;

        let res = timeout(Duration::from_secs(1), recv_pong(&sock)).await;
        assert!(res.is_err());

        // resume the network. note that the client ping does not have to be
        // resent.
        turmoil::release("client", "server");

        recv_pong(&sock).await
    });

    sim.run()
}

#[test]
fn network_partition() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let sock = bind().await?;

        while let Ok(origin) = recv_ping(&sock).await {
            let _ = send_pong(&sock, origin).await;
        }

        Ok(())
    });

    sim.client("client", async {
        // introduce the partition
        turmoil::partition("client", "server");

        let sock = bind().await?;
        send_ping(&sock).await?;

        assert!(timeout(Duration::from_secs(1), recv_pong(&sock))
            .await
            .is_err());

        Ok(())
    });

    sim.run()
}

#[test]
fn bounce() -> Result {
    // The server publishes the number of requests it thinks it processed into
    // this usize. Importantly, it resets when the server is rebooted.
    let reqs = Rc::new(AtomicUsize::new(0));

    let mut sim = Builder::new().build();

    sim.host("server", || {
        let publish = reqs.clone();
        let mut reqs = 0;
        async move {
            let sock = bind().await?;

            while let Ok(origin) = recv_ping(&sock).await {
                reqs += 1;
                publish.store(reqs, Ordering::SeqCst);

                let _ = send_pong(&sock, origin).await;
            }

            Ok(())
        }
    });

    for i in 0..3 {
        sim.client(format!("client-{i}"), async move {
            let sock = bind_to_v4(PORT + i).await?;

            send_ping(&sock).await?;
            recv_pong(&sock).await
        });

        sim.run()?;

        // The server always thinks it has only server 1 request.
        assert_eq!(1, reqs.load(Ordering::SeqCst));
        sim.bounce("server");
    }

    Ok(())
}

#[test]
fn bulk_transfer() -> Result {
    // set the latency to a well-known value
    let latency = Duration::from_millis(1);
    // perform several rounds of sending packets and sleeping for the latency
    let send_rounds = 10;
    // the UDP socket currently has a queue size of 64 packets; it should drop any packets that
    // exceed this amount.
    let queue_size = 64;
    // for each send round, we'll send double the number of packets that can be received by the
    // peer.
    let send_batch_size = queue_size * 2;

    // make the test deterministic
    let mut sim = Builder::new()
        .fail_rate(0.0)
        .min_message_latency(latency)
        .max_message_latency(latency)
        .build();

    sim.client("server", async move {
        let sock = bind_to_v4(123).await?;

        let mut total = 0;
        loop {
            let recv = recv_ping(&sock);
            let recv = tokio::time::timeout(Duration::from_secs(1), recv);
            if recv.await.is_err() {
                break;
            }
            total += 1;
        }

        // the receiver should be bounded by its queue size
        assert_eq!(total, send_rounds * queue_size);

        Ok(())
    });

    sim.client("client", async move {
        let sock = bind_to_v4(456).await?;

        let server = (lookup("server")[0], 123);

        for _ in 0..send_rounds {
            for _ in 0..send_batch_size {
                let _ = sock.send_to(b"ping", server).await?;
            }
            // sleep for the latency to allow the peer to flush its receive queue
            tokio::time::sleep(latency).await;
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn direct_ipv4_bind() -> Result {
    let mut sim = Builder::new().build();
    sim.node("alice").build_client(async {
        let fail = UdpSocket::bind(":::0").await?;
        let err = fail
            .send_to(b"Never", "192.168.0.42:2000")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);

        time::sleep(Duration::from_secs(1)).await;
        let udp = UdpSocket::bind("0.0.0.0:0").await?;
        udp.send_to(b"Hello world", "192.168.0.42:2000").await?;
        Ok(())
    });

    sim.node("bob")
        .with_addr(Ipv4Addr::new(192, 168, 0, 42))
        .build_client(async {
            let udp = UdpSocket::bind("192.168.0.42:2000").await?;
            let mut buf = [0; 128];
            let (n, from) = udp.recv_from(&mut buf).await?;
            assert_eq!(from.ip(), Ipv4Addr::new(192, 168, 0, 1));
            assert_eq!(&buf[..n], b"Hello world");
            Ok(())
        });

    sim.run()
}

fn double_ipv4_subnets() -> IpSubnets {
    let mut subnets = IpSubnets::new();
    subnets.add(IpSubnet::V4(Ipv4Subnet::new(
        Ipv4Addr::new(192, 168, 0, 0),
        16,
    )));
    subnets.add(IpSubnet::V4(Ipv4Subnet::new(
        Ipv4Addr::new(10, 1, 0, 0),
        16,
    )));
    subnets
}

#[test]
fn direct_ipv6_bind() -> Result {
    let mut sim = Builder::new().build();
    sim.node("alice").build_client(async {
        let fail = UdpSocket::bind("0.0.0.0:0").await?;
        let err = fail.send_to(b"Never", "fe80::42:2000").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);

        time::sleep(Duration::from_secs(1)).await;
        let udp = UdpSocket::bind(":::0").await?;
        udp.send_to(b"Hello world", "fe80::42:2000").await?;
        Ok(())
    });

    sim.node("bob")
        .with_addr(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x42))
        .build_client(async {
            let udp = UdpSocket::bind("fe80::42:2000").await?;
            let mut buf = [0; 128];
            let (n, from) = udp.recv_from(&mut buf).await?;
            assert_eq!(from.ip(), Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
            assert_eq!(&buf[..n], b"Hello world");
            Ok(())
        });

    sim.run()
}

#[test]
fn interface_exclusivity() -> Result {
    let mut sim = Builder::new().ip_subnets(double_ipv4_subnets()).build();
    sim.node("bob").build_client(async {
        let sock = UdpSocket::bind("192.168.0.1:2000").await?;
        let mut buf = [0; 128];
        let (n, _) = sock.recv_from(&mut buf).await?;
        assert_eq!(&buf[..n], b"Hello");
        Ok(())
    });

    sim.node("alice").build_client(async {
        let fail = UdpSocket::bind("10.1.0.2:0").await?;
        let err = fail
            .send_to(b"Never", "192.168.0.1:2000")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ConnectionRefused);

        let succ = UdpSocket::bind("192.168.0.2:0").await?;
        succ.send_to(b"Hello", "192.168.0.1:2000").await?;

        Ok(())
    });

    sim.run()
}

#[test]
fn unspecified_bind_all_interface_receiver() -> Result {
    let mut sim = Builder::new().ip_subnets(double_ipv4_subnets()).build();
    sim.node("rx").build_client(async move {
        let sock = UdpSocket::bind("0.0.0.0:2000").await?;
        let mut buf = [0; 128];

        let (n, origin) = sock.recv_from(&mut buf).await?;
        assert_eq!(&buf[..n], b"Hello from TX 1");
        assert_eq!(origin.ip(), Ipv4Addr::new(192, 168, 0, 2));

        let (n, origin) = sock.recv_from(&mut buf).await?;
        assert_eq!(&buf[..n], b"Hello from TX 2");
        assert_eq!(origin.ip(), Ipv4Addr::new(10, 1, 0, 3));

        Ok(())
    });

    sim.node("tx1").build_client(async move {
        UdpSocket::bind("192.168.0.2:0")
            .await?
            .send_to(b"Hello from TX 1", "192.168.0.1:2000")
            .await?;
        Ok(())
    });

    sim.node("tx2").build_client(async move {
        time::sleep(Duration::from_secs(1)).await;
        UdpSocket::bind("10.1.0.3:0")
            .await?
            .send_to(b"Hello from TX 2", "10.1.0.1:2000")
            .await?;
        Ok(())
    });

    sim.run()
}

#[test]
fn unspecified_bind_all_interfaces_sender() -> Result {
    let mut sim = Builder::new().ip_subnets(double_ipv4_subnets()).build();
    sim.node("rx1")
        .build_client(rx_once("192.168.0.1:2000", "Hello to RX 1"));
    sim.node("rx2")
        .build_client(rx_once("10.1.0.2:2000", "Hello to RX 2"));

    sim.node("tx").build_client(async move {
        let sock = UdpSocket::bind("0.0.0.0:0").await?;
        sock.send_to(b"Hello to RX 1", "192.168.0.1:2000").await?;
        sock.send_to(b"Hello to RX 2", "10.1.0.2:2000").await?;
        Ok(())
    });

    sim.run()
}

async fn rx_once(bind: &str, expect: &str) -> Result<()> {
    let sock = UdpSocket::bind(bind).await?;
    let mut buf = [0; 128];
    let (n, _) = sock.recv_from(&mut buf).await?;
    assert_eq!(&buf[..n], expect.as_bytes());
    Ok(())
}

#[test]
#[should_panic]
fn direct_bind_ip_version_not_supported() {
    let mut subnets = IpSubnets::new();
    subnets.add(IpSubnet::V4(Ipv4Subnet::new(Ipv4Addr::new(1, 2, 3, 0), 24)));
    let mut sim = Builder::new().ip_subnets(subnets).build();
    sim.node("alice").build_client(async {
        let _ = UdpSocket::bind(":::0").await?;

        Ok(())
    });
    sim.run().expect("paniced at subtask");
}

#[test]
fn bind_addr_in_use() -> Result {
    let mut sim = Builder::new().build();

    let (release, wait) = oneshot::channel();
    sim.client("server", async move {
        let listener = UdpSocket::bind(("0.0.0.0", 80)).await?;
        let result = UdpSocket::bind(("0.0.0.0", 80)).await;
        assert_error_kind(result, ErrorKind::AddrInUse);

        release.send(()).expect("Receiver closed");
        listener.recv_from(&mut [0]).await?;

        Ok(())
    });
    sim.client("client", async move {
        wait.await.expect("Sender dropped");
        let socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
        socket.send_to(&[0], ("server", 80)).await?;
        Ok(())
    });

    sim.run()
}

fn run_localhost_test(subnet: IpSubnet, bind_addr: SocketAddr, connect_addr: SocketAddr) -> Result {
    let mut sim = Builder::new()
        .ip_subnets(IpSubnets::from_iter([subnet]))
        .build();
    let expected = [0, 1, 7, 3, 8];
    sim.client("client", async move {
        let socket = UdpSocket::bind(bind_addr).await?;

        tokio::spawn(async move {
            let mut buf = [0; 5];
            let (_, peer) = socket.recv_from(&mut buf).await.unwrap();

            assert_eq!(expected, buf);
            assert_eq!(peer.ip(), connect_addr.ip());
            assert_eq!(socket.local_addr().unwrap().ip(), bind_addr.ip());

            socket.send_to(&expected, peer).await.unwrap();
        });

        let mut buf = [0; 5];
        let bind_addr = SocketAddr::new(bind_addr.ip(), 0);
        let socket = UdpSocket::bind(bind_addr).await?;
        socket.send_to(&expected, connect_addr).await?;
        let (_, peer) = socket.recv_from(&mut buf).await?;

        assert_eq!(expected, buf);
        assert_eq!(peer.ip(), connect_addr.ip());

        Ok(())
    });
    sim.run()
}

#[test]
fn loopback_to_wildcard_v4() -> Result {
    let bind_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 1234));
    run_localhost_test(IpSubnet::V4(Ipv4Subnet::default()), bind_addr, connect_addr)
}

#[test]
fn loopback_to_localhost_v4() -> Result {
    let bind_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 1234));
    run_localhost_test(IpSubnet::V4(Ipv4Subnet::default()), bind_addr, connect_addr)
}

#[test]
fn loopback_to_wildcard_v6() -> Result {
    let bind_addr = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv6Addr::LOCALHOST, 1234));
    run_localhost_test(IpSubnet::V6(Ipv6Subnet::default()), bind_addr, connect_addr)
}

#[test]
fn loopback_to_localhost_v6() -> Result {
    let bind_addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv6Addr::LOCALHOST, 1234));
    run_localhost_test(IpSubnet::V6(Ipv6Subnet::default()), bind_addr, connect_addr)
}

#[test]
fn remote_to_localhost_dropped() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let bind_addr = UdpSocket::bind((Ipv4Addr::LOCALHOST, 1234)).await?;
        let mut buf = [0; 4];

        let result = timeout(Duration::from_secs(1), bind_addr.recv_from(&mut buf)).await;
        assert!(result.is_err());
        Ok(())
    });

    sim.client("client", async move {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 1234)).await?;
        socket.send_to(&[0], ("server", 1234)).await?;
        Ok(())
    });

    sim.run()
}

/// Since localhost is special cased to not route through the topology, this
/// test validates that the world still steps forward even if a client ping
/// pongs back and forth over localhost.
#[test]
fn localhost_ping_pong() -> Result {
    let mut sim = Builder::new().build();
    sim.client("client", async move {
        let server = SocketAddr::from((Ipv4Addr::LOCALHOST, 1234));
        let socket = UdpSocket::bind(server).await?;

        tokio::spawn(async move {
            let mut buffer = [0; 16];
            let (_, peer) = socket.recv_from(&mut buffer).await.unwrap();

            let buffer = turmoil::elapsed().as_nanos().to_be_bytes();
            socket.send_to(&buffer, peer).await.unwrap();
        });

        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let start = turmoil::elapsed().as_nanos();
        socket.send_to(&start.to_be_bytes(), server).await?;

        let mut buffer = [0; 16];
        socket.recv_from(&mut buffer).await?;
        assert_ne!(start, u128::from_be_bytes(buffer));

        Ok(())
    });
    sim.run()
}

#[test]
fn socket_capacity() -> Result {
    let mut sim = Builder::new()
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(1))
        .udp_capacity(1)
        .build();

    let (tx, rx) = oneshot::channel();

    sim.client("server", async move {
        let s = bind().await?;

        _ = rx.await;
        recv_ping(&s).await?;
        assert!(timeout(Duration::from_secs(1), recv_ping(&s))
            .await
            .is_err());

        Ok(())
    });

    sim.client("client", async move {
        let s = bind().await?;

        send_ping(&s).await?;
        send_ping(&s).await?; // dropped
        _ = tx.send(());

        Ok(())
    });

    sim.run()
}

#[test]
fn socket_to_nonexistent_node() -> Result {
    let mut sim = Builder::new().build();
    sim.client("client", async move {
        assert_eq!(lookup("client")[0], Ipv4Addr::new(192, 168, 0, 1));
        let sock = UdpSocket::bind("0.0.0.0:0").await?;
        let send = sock.send_to(b"Hello world!", "192.168.0.2:80").await;
        assert!(
            send.is_err(),
            "Send operation should have failed, since node does not exist"
        );

        let err = send.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ConnectionRefused);
        assert_eq!(err.to_string(), "Connection refused");

        Ok(())
    });
    sim.run()
}
