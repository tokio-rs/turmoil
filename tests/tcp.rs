use std::{
    assert_eq, assert_ne,
    io::{self, ErrorKind},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    rc::Rc,
    time::Duration,
};

use std::future;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{oneshot, Notify},
    time::timeout,
};
use turmoil::{
    lookup,
    net::{TcpListener, TcpStream},
    Builder, IpVersion, Result,
};

const PORT: u16 = 1738;

fn assert_error_kind<T>(res: io::Result<T>, kind: io::ErrorKind) {
    assert_eq!(res.err().map(|e| e.kind()), Some(kind));
}

async fn bind_to_v4(port: u16) -> std::result::Result<TcpListener, std::io::Error> {
    TcpListener::bind((IpAddr::from(Ipv4Addr::UNSPECIFIED), port)).await
}

async fn bind_to_v6(port: u16) -> std::result::Result<TcpListener, std::io::Error> {
    TcpListener::bind((IpAddr::from(Ipv6Addr::UNSPECIFIED), port)).await
}

async fn bind() -> std::result::Result<TcpListener, std::io::Error> {
    bind_to_v4(PORT).await
}

#[test]
fn network_partitions_during_connect() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = bind().await?;
        loop {
            let _ = listener.accept().await;
        }
    });

    sim.client("client", async {
        turmoil::partition("client", "server");

        assert_error_kind(
            TcpStream::connect(("server", PORT)).await,
            io::ErrorKind::ConnectionRefused,
        );

        turmoil::repair("client", "server");
        turmoil::hold("client", "server");

        assert!(
            timeout(Duration::from_secs(1), TcpStream::connect(("server", PORT)))
                .await
                .is_err()
        );

        Ok(())
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
fn ephemeral_port_does_not_leak_on_server_shutdown() -> Result {
    let mut sim = Builder::new()
        .simulation_duration(Duration::from_secs(60))
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(1))
        .build();

    sim.host("server", || async {
        let listener = bind().await?;
        loop {
            let (stream, _) = listener.accept().await?;
            let (_, mut write) = stream.into_split();
            write.shutdown().await?;
        }
    });

    sim.client("client", async {
        for _ in 49152..=(65535 + 1) {
            let mut stream = TcpStream::connect(("server", PORT)).await?;
            let _ = stream.read_u8().await;
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn ephemeral_port_does_not_leak_on_client_shutdown() -> Result {
    let mut sim = Builder::new()
        .simulation_duration(Duration::from_secs(60))
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(1))
        .build();

    sim.host("server", || async {
        let listener = bind().await?;
        loop {
            let (mut stream, _) = listener.accept().await?;
            let _ = stream.read_u8().await;
        }
    });

    sim.client("client", async {
        for _ in 49152..=(65535 + 1) {
            let stream = TcpStream::connect(("server", PORT)).await?;
            let (_, mut write) = stream.into_split();
            write.shutdown().await?;
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn client_hangup_on_connect() -> Result {
    let mut sim = Builder::new().build();

    let timeout_secs = 1;

    sim.client("server", async move {
        let listener = bind().await?;

        assert!(
            timeout(Duration::from_secs(timeout_secs * 2), listener.accept())
                .await
                .is_err()
        );

        Ok(())
    });

    sim.client("client", async move {
        turmoil::hold("client", "server");

        assert!(timeout(
            Duration::from_secs(timeout_secs),
            TcpStream::connect(("server", PORT))
        )
        .await
        .is_err());

        turmoil::release("client", "server");

        Ok(())
    });

    sim.run()
}

#[test]
fn hold_and_release_once_connected() -> Result {
    let mut sim = Builder::new().build();

    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        wait.notified().await;
        s.write_u8(1).await?;

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        turmoil::hold("server", "client");

        notify.notify_one();

        assert!(timeout(Duration::from_secs(1), s.read_u8()).await.is_err());

        turmoil::release("server", "client");

        assert_eq!(1, s.read_u8().await?);

        Ok(())
    });

    sim.run()
}

#[test]
fn network_partition_once_connected() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        assert!(timeout(Duration::from_secs(1), s.read_u8()).await.is_err());

        s.write_u8(1).await?;

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        turmoil::partition("server", "client");

        s.write_u8(1).await?;

        assert!(timeout(Duration::from_secs(1), s.read_u8()).await.is_err());

        Ok(())
    });

    sim.run()
}

#[test]
fn accept_front_of_line_blocking() -> Result {
    let wait = Rc::new(Notify::new());
    let notify = wait.clone();

    let mut sim = Builder::new().build();

    // We setup the simulation with hosts A, B, and C

    sim.host("B", || async {
        let listener = bind().await?;

        while let Ok((_, peer)) = listener.accept().await {
            tracing::debug!("peer {}", peer);
        }

        Ok(())
    });

    // Hold all traffic from A:B
    sim.client("A", async move {
        turmoil::hold("A", "B");

        assert!(
            timeout(Duration::from_secs(1), TcpStream::connect(("B", PORT)))
                .await
                .is_err()
        );
        notify.notify_one();

        Ok(())
    });

    // C:B should succeed, and should not be blocked behind the A:B, which is
    // not eligible for delivery
    sim.client("C", async move {
        wait.notified().await;

        let _ = TcpStream::connect(("B", PORT)).await?;

        Ok(())
    });

    sim.run()
}

#[test]
fn send_upon_accept() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = bind().await?;

        while let Ok((mut s, _)) = listener.accept().await {
            assert!(s.write_u8(9).await.is_ok());
        }

        Ok(())
    });

    sim.client("client", async {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        assert_eq!(9, s.read_u8().await?);

        Ok(())
    });

    sim.run()
}

#[test]
fn n_responses() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = bind().await?;

        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                while let Ok(how_many) = s.read_u8().await {
                    for i in 0..how_many {
                        let _ = s.write_u8(i).await;
                    }
                }
            });
        }

        Ok(())
    });

    sim.client("client", async {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        let how_many = 3;
        s.write_u8(how_many).await?;

        for i in 0..how_many {
            assert_eq!(i, s.read_u8().await?);
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn server_concurrency() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = bind().await?;

        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                while let Ok(how_many) = s.read_u8().await {
                    for i in 0..how_many {
                        let _ = s.write_u8(i).await;
                    }
                }
            });
        }

        Ok(())
    });

    let how_many = 3;

    for i in 0..how_many {
        sim.client(format!("client-{i}"), async move {
            let mut s = TcpStream::connect(("server", PORT)).await?;

            s.write_u8(how_many).await?;

            for i in 0..how_many {
                assert_eq!(i, s.read_u8().await?);
            }

            Ok(())
        });
    }

    sim.run()
}

#[test]
fn drop_listener() -> Result {
    let how_many_conns = 3;

    let wait = Rc::new(Notify::new());
    let notify = wait.clone();

    let mut sim = Builder::new().build();

    sim.host("server", || {
        let notify = notify.clone();

        async move {
            let listener = bind().await?;

            for _ in 0..how_many_conns {
                let (mut s, _) = listener.accept().await?;
                tokio::spawn(async move {
                    while let Ok(how_many) = s.read_u8().await {
                        for i in 0..how_many {
                            let _ = s.write_u8(i).await;
                        }
                    }
                });
            }

            drop(listener);
            notify.notify_one();

            future::pending().await
        }
    });

    sim.client("client", async move {
        let mut conns = vec![];

        for _ in 0..how_many_conns {
            let s = TcpStream::connect(("server", 1738)).await?;
            conns.push(s);
        }

        wait.notified().await;

        for mut s in conns {
            let how_many = 3;
            let _ = s.write_u8(how_many).await;

            for i in 0..how_many {
                assert_eq!(i, s.read_u8().await?);
            }
        }

        assert_error_kind(
            TcpStream::connect(("server", 1738)).await,
            io::ErrorKind::ConnectionRefused,
        );

        Ok(())
    });

    sim.run()
}

#[test]
fn drop_listener_with_non_empty_queue() -> Result {
    let how_many_conns = 3;

    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    let tick = Duration::from_millis(10);
    let mut sim = Builder::new().tick_duration(tick).build();

    sim.host("server", || {
        let wait = wait.clone();

        async move {
            let listener = bind().await?;
            wait.notified().await;
            drop(listener);
            future::pending().await
        }
    });

    sim.client("client", async move {
        let mut conns = vec![];

        for _ in 0..how_many_conns {
            conns.push(tokio::task::spawn_local(TcpStream::connect((
                "server", PORT,
            ))));
        }

        // sleep for one iteration to land syns in the listener
        tokio::time::sleep(tick).await;
        notify.notify_one();

        for fut in conns {
            assert_error_kind(fut.await?, io::ErrorKind::ConnectionRefused);
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn recycle_server_socket_bind() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        for _ in 0..3 {
            let _ = TcpListener::bind((IpAddr::from(Ipv4Addr::UNSPECIFIED), 1234)).await?;
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn hangup() -> Result {
    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        let mut buf = [0; 8];
        assert!(matches!(s.read(&mut buf).await, Ok(0)));

        // Read again to ensure EOF
        assert!(matches!(s.read(&mut buf).await, Ok(0)));

        // Write until RST arrives
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;

            if let Err(e) = s.write_u8(1).await {
                assert_eq!(io::ErrorKind::BrokenPipe, e.kind());
                break;
            }
        }
        notify.notify_one();

        Ok(())
    });

    sim.client("client", async move {
        let s = TcpStream::connect(("server", PORT)).await?;

        drop(s);
        wait.notified().await;

        Ok(())
    });

    sim.run()
}

#[test]
fn shutdown_write() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        for i in 1..=3 {
            assert_eq!(i, s.read_u8().await?);
        }

        let mut buf = [0; 8];
        assert!(matches!(s.read(&mut buf).await, Ok(0)));

        for i in 0..3 {
            s.write_u8(i).await?;
        }

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        for i in 1..=3 {
            s.write_u8(i).await?;
        }

        s.shutdown().await?;

        assert_error_kind(s.shutdown().await, io::ErrorKind::NotConnected);
        assert_error_kind(s.write_u8(1).await, io::ErrorKind::BrokenPipe);

        // Ensure the other half is still open
        for i in 0..3 {
            assert_eq!(i, s.read_u8().await?);
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn read_with_empty_buffer() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let _ = listener.accept().await?;

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // no-op
        let mut buf = [0; 0];
        let n = s.read(&mut buf).await?;
        assert_eq!(0, n);

        Ok(())
    });

    sim.run()
}

#[test]
fn write_zero_bytes() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        assert_eq!(1, s.read_u8().await?);

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // no-op
        let buf = [0; 0];
        let _ = s.write(&buf).await?;

        // actual write to ensure server:read_u8 is not EOF
        s.write_u8(1).await?;

        Ok(())
    });

    sim.run()
}

#[test]
fn read_buf_smaller_than_msg() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        s.write_u64(1234).await?;
        s.write_u64(5678).await?;

        Ok(())
    });

    sim.client("client", async {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // read < message len
        let mut one_byte = [0; 1];
        assert_eq!(1, s.read(&mut one_byte).await?);

        // read the rest
        let mut rest = [0; 7];
        assert_eq!(7, s.read(&mut rest).await?);

        // one more to fallback to polling
        assert_eq!(5678, s.read_u64().await?);

        Ok(())
    });

    sim.run()
}

#[test]
fn split() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        assert_eq!(1, s.read_u8().await?);

        let mut buf = [0; 8];
        assert!(matches!(s.read(&mut buf).await, Ok(0)));

        s.write_u8(1).await?;

        // accept to establish s1, s2, s3 below
        listener.accept().await?;
        listener.accept().await?;
        listener.accept().await?;

        Ok(())
    });

    sim.client("client", async move {
        let s = TcpStream::connect(("server", PORT)).await?;

        let (mut r, mut w) = s.into_split();

        let h = tokio::spawn(async move { w.write_u8(1).await });

        assert_eq!(1, r.read_u8().await?);
        h.await??;

        let s1 = TcpStream::connect(("server", PORT)).await?;
        let s2 = TcpStream::connect(("server", PORT)).await?;
        let s3 = TcpStream::connect(("server", PORT)).await?;

        let (r1, w1) = s1.into_split();
        let (r2, w2) = s2.into_split();

        assert!(r1.reunite(w2).is_err());
        assert!(w1.reunite(r2).is_err());

        let (r3, w3) = s3.into_split();
        r3.reunite(w3)?;

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

#[test]
fn non_zero_bind() -> Result {
    let mut sim = Builder::new().ip_version(IpVersion::V4).build();
    sim.client("client", async move {
        let sock = TcpListener::bind("1.1.1.1:1").await;

        let Err(err) = sock else {
            panic!("bind should have failed")
        };
        assert_eq!(err.to_string(), "1.1.1.1:1 is not supported");
        Ok(())
    });
    sim.run()
}

#[test]
fn ipv6_connectivity() -> Result {
    let mut sim = Builder::new().ip_version(IpVersion::V6).build();
    sim.client("server", async move {
        let list = TcpListener::bind(("::", 80)).await.unwrap();
        let _stream = list.accept().await.unwrap();
        Ok(())
    });
    sim.client("client", async move {
        let stream = TcpStream::connect("server:80").await.unwrap();
        let _ = stream;
        Ok(())
    });

    sim.run()
}

#[test]
fn bind_addr_in_use() -> Result {
    let mut sim = Builder::new().build();

    let (release, wait) = oneshot::channel();
    sim.client("server", async move {
        let listener = TcpListener::bind(("0.0.0.0", 80)).await?;
        let result = TcpListener::bind(("0.0.0.0", 80)).await;
        assert_error_kind(result, io::ErrorKind::AddrInUse);

        release.send(()).expect("Receiver closed");
        listener.accept().await?;

        Ok(())
    });
    sim.client("client", async move {
        wait.await.expect("Sender dropped");
        TcpStream::connect("server:80").await?;
        Ok(())
    });

    sim.run()
}

fn run_localhost_test(
    ip_version: IpVersion,
    bind_addr: SocketAddr,
    connect_addr: SocketAddr,
) -> Result {
    let mut sim = Builder::new().ip_version(ip_version).build();
    let expected = [0, 1, 7, 3, 8];
    sim.client("client", async move {
        let listener = TcpListener::bind(bind_addr).await?;

        tokio::spawn(async move {
            let (mut socket, socket_addr) = listener.accept().await.unwrap();
            socket.write_all(&expected).await.unwrap();

            assert_eq!(socket_addr.ip(), connect_addr.ip());
            assert_eq!(socket.local_addr().unwrap().ip(), connect_addr.ip());
            assert_eq!(socket.peer_addr().unwrap().ip(), connect_addr.ip());
        });

        let mut socket = TcpStream::connect(connect_addr).await?;
        let mut actual = [0; 5];
        socket.read_exact(&mut actual).await?;

        assert_eq!(expected, actual);
        assert_eq!(socket.local_addr()?.ip(), connect_addr.ip());
        assert_eq!(socket.peer_addr()?.ip(), connect_addr.ip());

        Ok(())
    });
    sim.run()
}

#[test]
fn loopback_to_wildcard_v4() -> Result {
    let bind_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 1234));
    run_localhost_test(IpVersion::V4, bind_addr, connect_addr)
}

#[test]
fn loopback_to_localhost_v4() -> Result {
    let bind_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 1234));
    run_localhost_test(IpVersion::V4, bind_addr, connect_addr)
}

#[test]
fn loopback_wildcard_public_v4() -> Result {
    let bind_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv4Addr::new(192, 168, 0, 1), 1234));
    run_localhost_test(IpVersion::V4, bind_addr, connect_addr)
}

#[test]
fn loopback_localhost_public_v4() -> Result {
    let bind_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv4Addr::new(192, 168, 0, 1), 1234));
    let mut sim = Builder::new().ip_version(IpVersion::V4).build();
    sim.client("client", async move {
        let listener = TcpListener::bind(bind_addr).await?;

        tokio::spawn(async move {
            let (mut socket, socket_addr) = listener.accept().await.unwrap();
            socket.write_all(&[0, 1, 3, 7, 8]).await.unwrap();

            assert_eq!(socket_addr.ip(), connect_addr.ip());
            assert_eq!(socket.local_addr().unwrap().ip(), connect_addr.ip());
            assert_eq!(socket.peer_addr().unwrap().ip(), connect_addr.ip());
        });

        let res = TcpStream::connect(connect_addr).await;
        assert_error_kind(res, io::ErrorKind::ConnectionRefused);

        Ok(())
    });
    sim.run()
}

#[test]
fn loopback_to_wildcard_v6() -> Result {
    let bind_addr = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv6Addr::LOCALHOST, 1234));
    run_localhost_test(IpVersion::V6, bind_addr, connect_addr)
}

#[test]
fn loopback_to_localhost_v6() -> Result {
    let bind_addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv6Addr::LOCALHOST, 1234));
    run_localhost_test(IpVersion::V6, bind_addr, connect_addr)
}

#[test]
fn loopback_wildcard_public_v6() -> Result {
    let bind_addr = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1), 1234));
    run_localhost_test(IpVersion::V6, bind_addr, connect_addr)
}

#[test]
fn loopback_localhost_public_v6() -> Result {
    let bind_addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 1234);
    let connect_addr = SocketAddr::from((Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1), 1234));
    let mut sim = Builder::new().ip_version(IpVersion::V6).build();
    sim.client("client", async move {
        let listener = TcpListener::bind(bind_addr).await?;

        tokio::spawn(async move {
            let (mut socket, socket_addr) = listener.accept().await.unwrap();
            socket.write_all(&[0, 1, 3, 7, 8]).await.unwrap();

            assert_eq!(socket_addr.ip(), connect_addr.ip());
            assert_eq!(socket.local_addr().unwrap().ip(), connect_addr.ip());
            assert_eq!(socket.peer_addr().unwrap().ip(), connect_addr.ip());
        });

        let res = TcpStream::connect(connect_addr).await;
        assert_error_kind(res, io::ErrorKind::ConnectionRefused);

        Ok(())
    });
    sim.run()
}

#[test]
fn remote_to_localhost_refused() -> Result {
    let mut sim = Builder::new().build();

    let (stop, wait) = oneshot::channel();
    sim.client("server", async move {
        let bind_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 1234);
        let _listener = TcpListener::bind(bind_addr).await?;
        wait.await.unwrap();
        Ok(())
    });

    sim.client("client", async move {
        let result = TcpStream::connect(("server", 1234)).await;
        assert_error_kind(result, io::ErrorKind::ConnectionRefused);
        stop.send(()).unwrap();
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
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 1234)).await?;

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();

            let payload = turmoil::elapsed().as_nanos();
            socket.write_u128(payload).await.unwrap();
            let response = socket.read_u128().await.unwrap();
            assert_ne!(payload, response);
        });

        let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, 1234)).await?;
        let response = socket.read_u128().await?;
        let now = turmoil::elapsed().as_nanos();
        assert_ne!(response, now);

        Ok(())
    });
    sim.run()
}

#[test]
fn remote_dropped() -> Result {
    let mut sim = Builder::new().build();

    sim.client("client", async move {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 1234)).await?;
        tokio::spawn(async move {
            loop {
                let (_, _) = listener.accept().await.unwrap();
            }
        });

        let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, 1234)).await?;

        let mut buf = [0; 8];
        assert!(matches!(socket.read(&mut buf).await, Ok(0)));

        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;

            if let Err(e) = socket.write_u8(1).await {
                assert_eq!(io::ErrorKind::BrokenPipe, e.kind());
                break;
            }
        }

        Ok(())
    });

    sim.run()
}

#[test]
#[should_panic(expected = "192.168.0.1:80 server socket buffer full")]
fn socket_capacity() {
    let mut sim = Builder::new()
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(1))
        .tcp_capacity(1)
        .build();

    sim.host("server", || async {
        let l = TcpListener::bind(("0.0.0.0", 80)).await?;

        loop {
            _ = l.accept().await?;
        }
    });

    sim.client("client1", async move {
        _ = TcpStream::connect("server:80").await?;
        Ok(())
    });

    sim.client("client2", async move {
        _ = TcpStream::connect("server:80").await?;
        Ok(())
    });

    _ = sim.run();
}

#[test]
fn socket_to_nonexistent_node() -> Result {
    let mut sim = Builder::new().build();
    sim.client("client", async move {
        assert_eq!(lookup("client"), Ipv4Addr::new(192, 168, 0, 1));
        let sock = TcpStream::connect("192.168.0.2:80").await;
        assert!(
            sock.is_err(),
            "Send operation should have failed, since node does not exist"
        );

        let err = sock.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ConnectionRefused);
        assert_eq!(err.to_string(), "Connection refused");

        Ok(())
    });
    sim.run()
}
