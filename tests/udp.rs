use std::{
    matches,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    rc::Rc,
    sync::{atomic::AtomicUsize, atomic::Ordering},
    time::Duration,
};
use tokio::time::timeout;
use turmoil::{lookup, net, Builder, Result};

const PORT: u16 = 1738;

async fn bind() -> std::result::Result<net::UdpSocket, std::io::Error> {
    bind_to(PORT).await
}

async fn bind_to(port: u16) -> std::result::Result<net::UdpSocket, std::io::Error> {
    net::UdpSocket::bind((IpAddr::from(Ipv4Addr::UNSPECIFIED), port)).await
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
            let sock = bind_to(PORT + i).await?;

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
