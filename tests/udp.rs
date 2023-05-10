use std::{
    matches,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    rc::Rc,
    sync::{atomic::AtomicUsize, atomic::Ordering},
    time::Duration,
};
use tokio::time::timeout;
use turmoil::{lookup, net, Builder, IpVersion, Result};

const PORT: u16 = 1738;

async fn bind() -> std::result::Result<net::UdpSocket, std::io::Error> {
    bind_to_v4(PORT).await
}

async fn bind_to_v4(port: u16) -> std::result::Result<net::UdpSocket, std::io::Error> {
    net::UdpSocket::bind((IpAddr::from(Ipv4Addr::UNSPECIFIED), port)).await
}

async fn bind_to_v6(port: u16) -> std::result::Result<net::UdpSocket, std::io::Error> {
    net::UdpSocket::bind((IpAddr::from(Ipv6Addr::UNSPECIFIED), port)).await
}

async fn send_ping(sock: &net::UdpSocket) -> Result<()> {
    sock.send_to(b"ping", (lookup("server"), 1738)).await?;

    Ok(())
}

fn try_send_ping(sock: &net::UdpSocket) -> Result<()> {
    sock.try_send_to(b"ping", (lookup("server"), 1738))?;

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

        let server_addr = lookup("server");
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
        assert!(matches!(res, Err(_)));

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

        let server = (lookup("server"), 123);

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

// # IpVersion specific tests

#[test]
fn bind_ipv4_socket() -> Result {
    let mut sim = Builder::new().ip_version(IpVersion::V4).build();
    sim.client("client", async move {
        let sock = bind_to_v4(0).await?;
        assert!(sock.local_addr().unwrap().is_ipv4());
        Ok(())
    });
    sim.run()
}

#[test]
fn bind_ipv6_socket() -> Result {
    let mut sim = Builder::new().ip_version(IpVersion::V6).build();
    sim.client("client", async move {
        let sock = bind_to_v6(0).await?;
        assert!(sock.local_addr().unwrap().is_ipv6());
        Ok(())
    });
    sim.run()
}

#[test]
#[should_panic]
fn bind_ipv4_version_missmatch() {
    let mut sim = Builder::new().ip_version(IpVersion::V6).build();
    sim.client("client", async move {
        let _sock = bind_to_v4(0).await?;
        Ok(())
    });
    sim.run().unwrap()
}

#[test]
#[should_panic]
fn bind_ipv6_version_missmatch() {
    let mut sim = Builder::new().ip_version(IpVersion::V4).build();
    sim.client("client", async move {
        let _sock = bind_to_v6(0).await?;
        Ok(())
    });
    sim.run().unwrap()
}
