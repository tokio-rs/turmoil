use std::{
    assert_eq, assert_ne,
    io::{self, ErrorKind, IoSliceMut},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    rc::Rc,
    time::Duration,
};

use std::future;
use test_case::test_case;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, Interest},
    sync::{oneshot, Notify},
    time::{sleep, timeout, Instant},
};
use turmoil::{
    lookup,
    net::{TcpListener, TcpStream},
    Builder, IpVersion, Result,
};

const PORT: u16 = 1738;

#[test]
fn try_read_vectored_scatter_and_partial_reads() -> Result {
    let mut sim = Builder::new().build();
    sim.client("client", async {
        let listener = TcpListener::bind(("127.0.0.1", PORT)).await?;
        let (mut client, (server, _)) =
            tokio::try_join!(TcpStream::connect(("127.0.0.1", PORT)), listener.accept())?;
        let mut first = [b'!'; 2];
        let mut second = [b'!'; 3];
        assert_error_kind(
            server.try_read_vectored(&mut [
                IoSliceMut::new(&mut first),
                IoSliceMut::new(&mut second),
            ]),
            ErrorKind::WouldBlock,
        );
        assert_eq!(&first, b"!!");
        assert_eq!(&second, b"!!!");

        client.write_all(b"abcdefgh").await?;
        assert_eq!(server.peek(&mut [0; 8]).await?, 8);
        assert_eq!(server.try_read_vectored(&mut [])?, 0);
        assert_eq!(
            server.try_read_vectored(&mut [IoSliceMut::new(&mut [])])?,
            0
        );
        assert_eq!(
            server.try_read_vectored(&mut [
                IoSliceMut::new(&mut []),
                IoSliceMut::new(&mut first),
                IoSliceMut::new(&mut []),
                IoSliceMut::new(&mut second),
                IoSliceMut::new(&mut []),
            ])?,
            5
        );
        assert_eq!(&first, b"ab");
        assert_eq!(&second, b"cde");

        first.fill(b'!');
        second.fill(b'!');
        let mut untouched = [b'!'; 2];
        assert_eq!(
            server.try_read_vectored(&mut [
                IoSliceMut::new(&mut first),
                IoSliceMut::new(&mut second),
                IoSliceMut::new(&mut untouched),
            ])?,
            3
        );
        assert_eq!(&first, b"fg");
        assert_eq!(&second, b"h!!");
        assert_eq!(&untouched, b"!!");
        assert_error_kind(
            server.try_read_vectored(&mut [IoSliceMut::new(&mut untouched)]),
            ErrorKind::WouldBlock,
        );

        client.shutdown().await?;
        assert_eq!(server.peek(&mut [0]).await?, 0);
        assert_eq!(
            server.try_read_vectored(&mut [IoSliceMut::new(&mut untouched)])?,
            0
        );
        assert_eq!(&untouched, b"!!");
        Ok(())
    });
    sim.run()
}

#[test]
fn try_read_vectored_crosses_segments_and_releases_capacity() -> Result {
    // Three-byte send and receive buffers.
    let mut sim = Builder::new()
        .tcp_capacity(3)
        .tick_duration(Duration::from_millis(1))
        .build();
    sim.client("client", async {
        let listener = TcpListener::bind(("127.0.0.1", PORT)).await?;
        let (client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect(("127.0.0.1", PORT)), listener.accept())?;
        assert_eq!(client.try_write(b"ab")?, 2);
        assert_eq!(client.try_write(b"cdef")?, 1);
        assert_error_kind(client.try_write(b"g"), ErrorKind::WouldBlock);
        // Loopback delivery runs on the next simulation tick. Both
        // segments land in the server's receive buffer, which frees the
        // send buffer for bytes the closed window holds back.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(client.try_write(b"def")?, 3);
        assert_error_kind(client.try_write(b"g"), ErrorKind::WouldBlock);
        let mut first = [0; 2];
        let mut second = [0; 2];
        let (read, write) = server.into_split();
        assert_eq!(
            read.try_read_vectored(&mut [
                IoSliceMut::new(&mut first),
                IoSliceMut::new(&mut second),
            ])?,
            3
        );
        assert_eq!(&first, b"ab");
        assert_eq!(&second[..1], b"c");
        // Reading reopens the window; the held bytes drain and free
        // space again.
        client.writable().await?;
        assert_eq!(client.try_write(b"g")?, 1);
        server = read.reunite(write)?;
        let mut rest = [0; 4];
        server.read_exact(&mut rest).await?;
        assert_eq!(&rest, b"defg");
        Ok(())
    });
    sim.run()
}

#[test]
fn ready_then_try_read_vectored() -> Result {
    // Each four-byte write fits the server's receive buffer, leaving room
    // in the window for the FIN.
    let mut sim = Builder::new().tcp_capacity(5).build();
    sim.client("client", async {
        let listener = TcpListener::bind(("127.0.0.1", PORT)).await?;
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect(("127.0.0.1", PORT)), listener.accept())?;

        client.write_all(b"abcd").await?;
        let ready = server.ready(Interest::READABLE).await?;
        assert!(ready.is_readable());
        assert!(!ready.is_read_closed());
        let mut first = [0; 2];
        let mut second = [0; 2];
        assert_eq!(
            server.try_read_vectored(&mut [
                IoSliceMut::new(&mut first),
                IoSliceMut::new(&mut second),
            ])?,
            4
        );
        assert_eq!(&first, b"ab");
        assert_eq!(&second, b"cd");
        assert_error_kind(
            server.try_read_vectored(&mut [IoSliceMut::new(&mut first)]),
            ErrorKind::WouldBlock,
        );
        assert!(
            timeout(Duration::from_millis(10), server.ready(Interest::READABLE))
                .await
                .is_err()
        );

        // Readiness must release capacity.
        client.write_all(b"efgh").await?;
        client.shutdown().await?;
        sleep(Duration::from_secs(1)).await;
        assert!(server.ready(Interest::READABLE).await?.is_read_closed());
        let (read, write) = server.into_split();
        assert_eq!(
            read.try_read_vectored(&mut [
                IoSliceMut::new(&mut first),
                IoSliceMut::new(&mut second),
            ])?,
            4
        );
        assert_eq!(&first, b"ef");
        assert_eq!(&second, b"gh");
        // Detect EOF directly, without an async read or peek observing it first.
        assert_eq!(
            read.try_read_vectored(&mut [IoSliceMut::new(&mut first)])?,
            0
        );
        server = read.reunite(write)?;
        assert!(server.ready(Interest::READABLE).await?.is_read_closed());
        assert_eq!(server.read(&mut first).await?, 0);
        Ok(())
    });
    sim.run()
}

#[test]
fn try_read_vectored_reports_reset() -> Result {
    let mut sim = Builder::new().build();
    sim.client("client", async {
        let listener = TcpListener::bind(("127.0.0.1", PORT)).await?;
        let (client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect(("127.0.0.1", PORT)), listener.accept())?;
        server.write_all(b"unread").await?;
        client.peek(&mut [0]).await?;
        drop(client);
        assert_error_kind(server.peek(&mut [0]).await, ErrorKind::ConnectionReset);
        let mut buf = [b'!'; 2];
        assert_error_kind(
            server.try_read_vectored(&mut [IoSliceMut::new(&mut buf)]),
            ErrorKind::ConnectionReset,
        );
        assert_eq!(&buf, b"!!");
        Ok(())
    });
    sim.run()
}

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

        // SYNs vanish, so connect gives up once retransmits run out.
        assert_error_kind(
            TcpStream::connect(("server", PORT)).await,
            io::ErrorKind::TimedOut,
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
fn release_bind_on_drop() -> Result {
    let mut sim = Builder::new().build();

    sim.client("client", async {
        let sock = bind_to_v4(4555).await.unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;

        drop(sock);

        let _sock = bind_to_v4(4555).await.unwrap();

        Ok(())
    });

    sim.run()
}

#[test]
fn ephemeral_port_does_not_leak_on_server_shutdown() -> Result {
    // Each connection takes a three-way handshake plus the server's FIN.
    let mut sim = Builder::new()
        .simulation_duration(Duration::from_secs(120))
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

    let accepted = Rc::new(Notify::new());
    let notify = accepted.clone();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;
        notify.notify_one();

        assert!(timeout(Duration::from_secs(1), s.read_u8()).await.is_err());

        s.write_u8(1).await?;

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // `connect` returns before the server sees the final ACK; let the
        // handshake finish so the server can accept.
        accepted.notified().await;
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
fn accept_front_of_line_dropped() -> Result {
    let wait = Rc::new(Notify::new());
    let notify = wait.clone();

    let mut sim = Builder::new()
        .min_message_latency(Duration::ZERO)
        .max_message_latency(Duration::ZERO)
        .build();

    sim.host("server", move || {
        let wait = Rc::clone(&wait);
        async move {
            let listener = bind().await?;
            wait.notified().await;

            while let Ok((_, peer)) = listener.accept().await {
                tracing::debug!("peer {}", peer);
            }

            Ok(())
        }
    });

    sim.client("client", async move {
        // Queue up a number of abandoned connections at the server. The
        // handshakes complete into the accept backlog, so connect returns
        // before the server accepts.
        for _ in 0..5 {
            let connect = TcpStream::connect(("server", PORT));
            drop(timeout(Duration::from_secs(1), connect).await??);
        }

        // After allowing the server to accept, the next connection attempt
        // should succeed.
        notify.notify_one();
        let mut s = TcpStream::connect(("server", PORT)).await?;
        s.write_u8(1).await?;

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

        // Closing the listener resets every unaccepted connection. A
        // handshake still in flight is refused; one that completed into
        // the backlog connects, then sees the reset.
        for fut in conns {
            match fut.await? {
                Err(err) => assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused),
                Ok(mut s) => assert_error_kind(s.read_u8().await, io::ErrorKind::ConnectionReset),
            }
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
        // The server's side establishes when the final ACK lands.
        tokio::time::sleep(Duration::from_secs(1)).await;

        assert_eq!(1, turmoil::established_tcp_stream_count());
        assert_eq!(1, turmoil::established_tcp_stream_count_on("server"));

        drop(s);
        assert_eq!(0, turmoil::established_tcp_stream_count());
        assert_eq!(1, turmoil::established_tcp_stream_count_on("server")); // server sleeps in its loop

        wait.notified().await;
        assert_eq!(0, turmoil::established_tcp_stream_count_on("server"));

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

        // Shutting down again is a no-op, as on Linux.
        s.shutdown().await?;
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

// The FIN needs one byte of window, like any sequence number.
#[test_case(65; "tight receive buffer")]
#[test_case(128; "spare receive buffer")]
fn ready_reports_close_behind_unread_data(capacity: usize) -> Result {
    let mut sim = Builder::new().tcp_capacity(capacity).build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        s.write_all(&[42; 64]).await?;
        s.shutdown().await?;
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // Let the data and the FIN behind it both arrive.
        sleep(Duration::from_secs(1)).await;

        let ready = s.ready(Interest::READABLE).await?;
        assert!(ready.is_readable());
        assert!(ready.is_read_closed());

        // Readiness consumed nothing.
        let mut received = Vec::new();
        timeout(Duration::from_secs(1), s.read_to_end(&mut received)).await??;
        assert_eq!(vec![42; 64], received);

        let ready = s.ready(Interest::READABLE).await?;
        assert!(ready.is_read_closed());
        assert!(!ready.is_write_closed());

        Ok(())
    });

    sim.run()
}

#[test]
fn ready_with_unread_data_is_not_closed() -> Result {
    let mut sim = Builder::new().build();

    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        s.write_all(b"pipeline").await?;
        wait.notified().await;
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        let ready = s.ready(Interest::READABLE).await?;
        assert!(ready.is_readable());
        assert!(!ready.is_read_closed());

        let mut received = [0; 8];
        s.read_exact(&mut received).await?;
        assert_eq!(b"pipeline", &received);

        notify.notify_one();
        Ok(())
    });

    sim.run()
}

#[test]
fn ready_waits_for_data() -> Result {
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

        assert!(timeout(Duration::from_secs(1), s.ready(Interest::READABLE))
            .await
            .is_err());

        notify.notify_one();
        assert!(s.ready(Interest::READABLE).await?.is_readable());
        assert_eq!(1, s.read_u8().await?);

        Ok(())
    });

    sim.run()
}

#[test]
fn ready_reports_reset() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (s, _) = listener.accept().await?;

        // Closing with unread data resets the connection.
        sleep(Duration::from_secs(1)).await;
        drop(s);
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        s.write_all(b"unread").await?;

        // Blocks until the reset lands; only the requested interest is reported.
        let ready = s.ready(Interest::READABLE).await?;
        assert!(ready.is_read_closed(), "{ready:?}");
        assert!(!ready.is_write_closed(), "{ready:?}");

        let ready = s.ready(Interest::READABLE | Interest::WRITABLE).await?;
        assert!(ready.is_read_closed(), "{ready:?}");
        assert!(ready.is_write_closed(), "{ready:?}");

        Ok(())
    });

    sim.run()
}

#[test_case(b"unreadun"; "full buffers")]
#[test_case(b"un"; "spare buffer space")]
fn ready_writable_reports_reset(unread: &'static [u8]) -> Result {
    // Four-byte buffers: eight unread bytes fill the server's receive
    // buffer and our send buffer.
    let mut sim = Builder::new().tcp_capacity(4).build();

    sim.client("server", async {
        let listener = bind().await?;
        let (s, _) = listener.accept().await?;
        sleep(Duration::from_secs(1)).await;
        drop(s);
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        s.write_all(unread).await?;

        if unread.len() < 8 {
            sleep(Duration::from_secs(2)).await;
        }

        let start = Instant::now();
        let ready = timeout(Duration::from_secs(3), s.ready(Interest::WRITABLE)).await??;
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(ready.is_write_closed(), "{ready:?}");
        assert!(!ready.is_read_closed(), "{ready:?}");
        // As in tokio, readiness succeeds; the write reports the reset.
        s.writable().await?;
        assert_error_kind(s.try_write(b"x"), ErrorKind::ConnectionReset);
        Ok(())
    });

    sim.run()
}

#[test]
fn write_blocked_on_flow_control_fails_on_reset() -> Result {
    let mut sim = Builder::new().tcp_capacity(3).build();

    sim.client("server", async {
        let listener = bind().await?;
        let (s, _) = listener.accept().await?;
        sleep(Duration::from_secs(1)).await;
        drop(s);
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        // Fill the server's receive buffer and our send buffer.
        s.write_all(b"onetwo").await?;

        // No space remains, so only the reset can wake this write.
        let start = Instant::now();
        let res = timeout(Duration::from_secs(3), s.write_all(b"six")).await?;
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_error_kind(res, ErrorKind::ConnectionReset);
        Ok(())
    });

    sim.run()
}

#[test]
fn empty_write_does_not_wait_for_credits() -> Result {
    let mut sim = Builder::new().tcp_capacity(1).build();

    sim.host("server", || async {
        let listener = bind().await?;
        let (_s, _) = listener.accept().await?;
        future::pending().await
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        // Fill the server's receive buffer and our send buffer.
        s.write_all(b"xy").await?;

        // The server never reads, so space never frees up.
        assert_eq!(0, timeout(Duration::from_secs(1), s.write(b"")).await??);

        let (_, mut w) = s.into_split();
        assert_eq!(0, timeout(Duration::from_secs(1), w.write(b"")).await??);
        Ok(())
    });

    sim.run()
}

#[test]
fn ready_error_interest_waits_for_reset() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        let listener = bind().await?;
        let (s, _) = listener.accept().await?;
        sleep(Duration::from_secs(1)).await;
        drop(s);
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        s.write_all(b"unread").await?;

        // A healthy connection has no error to report.
        let healthy = timeout(Duration::from_millis(500), s.ready(Interest::ERROR)).await;
        assert!(healthy.is_err());

        let ready = timeout(Duration::from_secs(3), s.ready(Interest::ERROR)).await??;
        assert!(ready.is_error(), "{ready:?}");
        assert!(!ready.is_readable(), "{ready:?}");
        Ok(())
    });

    sim.run()
}

#[derive(Clone, Copy)]
enum ObserveEof {
    Read,
    Peek,
    Ready,
}

#[test_case(ObserveEof::Read; "after read")]
#[test_case(ObserveEof::Peek; "after peek")]
#[test_case(ObserveEof::Ready; "after readiness")]
fn ready_reports_reset_after_eof(observe: ObserveEof) -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;
        s.shutdown().await?;
        assert_eq!(1, s.peek(&mut [0; 1]).await?);
        drop(s);
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        match observe {
            ObserveEof::Read => assert_eq!(0, s.read(&mut [0; 1]).await?),
            ObserveEof::Peek => assert_eq!(0, s.peek(&mut [0; 1]).await?),
            ObserveEof::Ready => assert!(s.ready(Interest::READABLE).await?.is_read_closed()),
        }

        s.write_all(b"unread").await?;
        sleep(Duration::from_secs(1)).await;

        let ready = s.ready(Interest::READABLE | Interest::WRITABLE).await?;
        assert!(ready.is_read_closed(), "{ready:?}");
        assert!(ready.is_write_closed(), "{ready:?}");
        Ok(())
    });

    sim.run()
}

#[test]
fn ready_writable() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let _ = listener.accept().await?;
        Ok(())
    });

    sim.client("client", async move {
        let s = TcpStream::connect(("server", PORT)).await?;

        let ready = s.ready(Interest::WRITABLE).await?;
        assert!(ready.is_writable());
        assert!(!ready.is_readable());

        Ok(())
    });

    sim.run()
}

#[test]
fn readable_then_try_read_vectored() -> Result {
    let mut sim = Builder::new().build();

    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;
        wait.notified().await;
        s.write_all(b"ab").await?;
        s.shutdown().await?;
        Ok(())
    });

    sim.client("client", async move {
        // Readiness and peeks take `&self`, as in tokio.
        let s = TcpStream::connect(("server", PORT)).await?;
        let mut buf = [0; 2];

        assert!(timeout(Duration::from_secs(1), s.readable()).await.is_err());
        notify.notify_one();

        s.readable().await?;
        assert_eq!(2, s.peek(&mut buf).await?);
        assert_eq!(2, s.try_read_vectored(&mut [IoSliceMut::new(&mut buf)])?);
        assert_eq!(b"ab", &buf);

        // EOF is readable, and the read that follows reports it.
        s.readable().await?;
        assert_eq!(0, s.try_read_vectored(&mut [IoSliceMut::new(&mut buf)])?);
        Ok(())
    });

    sim.run()
}

#[test]
fn readable_completes_on_reset() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        let listener = bind().await?;
        let (s, _) = listener.accept().await?;
        sleep(Duration::from_secs(1)).await;
        drop(s);
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        s.write_all(b"unread").await?;

        // As in tokio, readiness reports no error; the read that follows does.
        timeout(Duration::from_secs(3), s.readable()).await??;
        assert_error_kind(
            s.try_read_vectored(&mut [IoSliceMut::new(&mut [0; 1])]),
            ErrorKind::ConnectionReset,
        );
        Ok(())
    });

    sim.run()
}

#[test]
fn poll_ready_fns_match_async_fns() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;
        s.write_u8(1).await?;
        s.shutdown().await?;
        Ok(())
    });

    sim.client("client", async move {
        let s = TcpStream::connect(("server", PORT)).await?;
        future::poll_fn(|cx| s.poll_write_ready(cx)).await?;
        future::poll_fn(|cx| s.poll_read_ready(cx)).await?;
        assert_eq!(1, s.try_read_vectored(&mut [IoSliceMut::new(&mut [0; 1])])?);
        future::poll_fn(|cx| s.poll_read_ready(cx)).await?;
        assert!(s.ready(Interest::READABLE).await?.is_read_closed());
        Ok(())
    });

    sim.run()
}

#[test]
fn concurrent_readable_and_writable_waiters_observe_reset() -> Result {
    let mut sim = Builder::new().tcp_capacity(1).build();

    sim.client("server", async {
        let listener = bind().await?;
        let (s, _) = listener.accept().await?;
        sleep(Duration::from_secs(1)).await;
        drop(s);
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        // Fill both buffers so only the reset can wake the write waiter.
        s.write_all(b"xy").await?;

        let s = std::sync::Arc::new(s);
        let w = s.clone();
        let writable = tokio::spawn(async move { w.writable().await });
        tokio::task::yield_now().await;
        // Polled after the write waiter; must not steal its reset wakeup.
        let r = s.clone();
        let readable = tokio::spawn(async move { r.readable().await });

        let (write, read) = timeout(Duration::from_secs(3), async {
            tokio::join!(writable, readable)
        })
        .await?;
        // As in tokio, readiness succeeds; I/O reports the reset.
        write??;
        read??;
        assert_error_kind(s.try_write(b"z"), ErrorKind::ConnectionReset);
        Ok(())
    });

    sim.run()
}

#[test]
fn two_readable_waiters_both_wake() -> Result {
    let mut sim = Builder::new().build();

    sim.client("client", async {
        let listener = bind().await?;
        let (reader, (mut writer, _)) =
            tokio::try_join!(TcpStream::connect(("127.0.0.1", PORT)), listener.accept())?;
        let reader = std::sync::Arc::new(reader);

        let r = reader.clone();
        let first = tokio::spawn(async move { r.readable().await });
        tokio::task::yield_now().await;
        let r = reader.clone();
        let second = tokio::spawn(async move { r.ready(Interest::READABLE).await });
        tokio::task::yield_now().await;

        writer.write_all(b"x").await?;

        let (first, second) = timeout(Duration::from_secs(1), async {
            tokio::join!(first, second)
        })
        .await?;
        first??;
        assert!(second??.is_readable());
        Ok(())
    });

    sim.run()
}

#[test]
fn two_writable_waiters_both_wake() -> Result {
    let mut sim = Builder::new().tcp_capacity(1).build();

    sim.client("client", async {
        let listener = bind().await?;
        let (mut writer, (mut reader, _)) =
            tokio::try_join!(TcpStream::connect(("127.0.0.1", PORT)), listener.accept())?;
        // Fill both buffers so both waiters block on the peer reading.
        writer.write_all(b"xy").await?;
        let writer = std::sync::Arc::new(writer);

        let w = writer.clone();
        let first = tokio::spawn(async move { w.writable().await });
        tokio::task::yield_now().await;
        let w = writer.clone();
        let second = tokio::spawn(async move { w.ready(Interest::WRITABLE).await });
        tokio::task::yield_now().await;

        assert_eq!(b'x', reader.read_u8().await?);

        let (first, second) = timeout(Duration::from_secs(1), async {
            tokio::join!(first, second)
        })
        .await?;
        first??;
        assert!(second??.is_writable());
        Ok(())
    });

    sim.run()
}

#[test]
fn peek_empty_buffer() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let _ = listener.accept().await?;
        Ok(())
    });

    sim.client("client", async move {
        let s = TcpStream::connect(("server", PORT)).await?;

        // no-op peek with empty buffer
        let mut buf = [0; 0];
        let n = s.peek(&mut buf).await?;
        assert_eq!(0, n);

        Ok(())
    });

    sim.run()
}

#[test]
fn peek_then_read() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        s.write_u64(1234).await?;
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // peek full message
        let mut peek_buf = [0; 8];
        assert_eq!(8, s.peek(&mut peek_buf).await?);
        assert_eq!(1234u64, u64::from_be_bytes(peek_buf));

        // peek again should see same data
        let mut peek_buf2 = [0; 8];
        assert_eq!(8, s.peek(&mut peek_buf2).await?);
        assert_eq!(1234u64, u64::from_be_bytes(peek_buf2));

        // read should consume the data
        assert_eq!(1234, s.read_u64().await?);
        let mut buf = [0; 8];
        assert!(matches!(s.read(&mut buf).await, Ok(0)));

        Ok(())
    });

    sim.run()
}

#[test]
fn peek_partial() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        s.write_all(&[0, 0, 1, 1]).await?;
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // peek with smaller buffer
        let mut peek_buf = [0; 2];
        assert_eq!(2, s.peek(&mut peek_buf).await?);
        assert_eq!([0, 0], peek_buf);

        // peek with larger buffer should still see all data
        let mut peek_buf2 = [0; 4];
        assert_eq!(4, s.peek(&mut peek_buf2).await?);
        assert_eq!([0, 0, 1, 1], peek_buf2);

        // read partial
        let mut read_buf = [0; 2];
        assert_eq!(2, s.read(&mut read_buf).await?);
        assert_eq!([0, 0], read_buf);

        // peek remaining
        let mut peek_buf3 = [0; 2];
        assert_eq!(2, s.peek(&mut peek_buf3).await?);
        assert_eq!([1, 1], peek_buf3);

        Ok(())
    });

    sim.run()
}

#[test]
fn peek_multiple_messages() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        s.write_u64(1234).await?;
        s.write_u64(5678).await?;
        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // peek first message
        let mut peek_buf = [0; 8];
        assert_eq!(8, s.peek(&mut peek_buf).await?);
        assert_eq!(1234u64, u64::from_be_bytes(peek_buf));

        // read first message
        assert_eq!(1234, s.read_u64().await?);

        // peek second message
        let mut peek_buf2 = [0; 8];
        assert_eq!(8, s.peek(&mut peek_buf2).await?);
        assert_eq!(5678u64, u64::from_be_bytes(peek_buf2));

        // read second message
        assert_eq!(5678, s.read_u64().await?);

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

// As on Linux, wildcard binds succeed in either family, but a host has
// no source address to reach a peer outside its network's family.
#[test]
fn ipv4_unreachable_on_ipv6_network() -> Result {
    let mut sim = Builder::new().ip_version(IpVersion::V6).build();
    sim.client("client", async move {
        let _sock = bind_to_v4(0).await?;
        assert_error_kind(
            TcpStream::connect("192.168.0.2:80").await,
            ErrorKind::AddrNotAvailable,
        );
        Ok(())
    });
    sim.run()
}

#[test]
fn ipv6_unreachable_on_ipv4_network() -> Result {
    let mut sim = Builder::new().ip_version(IpVersion::V4).build();
    sim.client("client", async move {
        let _sock = bind_to_v6(0).await?;
        assert_error_kind(
            TcpStream::connect("[fe80::2]:80").await,
            ErrorKind::AddrNotAvailable,
        );
        Ok(())
    });
    sim.run()
}

#[test]
fn non_zero_bind() -> Result {
    let mut sim = Builder::new().ip_version(IpVersion::V4).build();
    sim.client("client", async move {
        let sock = TcpListener::bind("1.1.1.1:1").await;

        assert_error_kind(sock, ErrorKind::AddrNotAvailable);
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

#[test]
fn loopback_delivery() -> Result {
    let mut sim = Builder::new()
        .tick_duration(Duration::from_millis(100))
        .build();

    sim.client("host", async {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 1738)).await?;

        tokio::spawn(async move {
            let (mut acc, _) = listener.accept().await.unwrap();

            tokio::time::sleep(Duration::from_millis(50)).await;
            _ = acc.write_u128(9).await;
        });

        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, 1738)).await?;
        assert_eq!(9, s.read_u128().await?);

        Ok(())
    });

    // syn
    assert!(!sim.step()?);
    // write
    assert!(!sim.step()?);
    // write delivered
    assert!(sim.step()?);

    Ok(())
}

fn run_localhost_test(
    ip_version: IpVersion,
    bind_addr: SocketAddr,
    connect_addr: SocketAddr,
) -> Result {
    let mut sim = Builder::new()
        .tick_duration(Duration::from_millis(100))
        .ip_version(ip_version)
        .build();
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
fn socket_to_nonexistent_node() -> Result {
    let mut sim = Builder::new().build();
    sim.client("client", async move {
        assert_eq!(lookup("client"), Ipv4Addr::new(192, 168, 0, 1));
        let sock = TcpStream::connect("192.168.0.2:80").await;

        assert_error_kind(sock, ErrorKind::ConnectionRefused);

        Ok(())
    });
    sim.run()
}

#[test]
fn exhaust_ephemeral_ports() -> Result {
    let mut sim = Builder::new()
        .tick_duration(Duration::from_millis(1))
        .ephemeral_ports(49152..=49162)
        .build();

    sim.client("localhost", async {
        let l = TcpListener::bind("127.0.0.1:1738").await?;
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    _ = s.read_u8().await;
                });
            }
        });

        let mut socks = vec![];
        let err = loop {
            match TcpStream::connect("127.0.0.1:1738").await {
                Ok(sock) => socks.push(sock),
                Err(err) => break err,
            }
        };
        assert_eq!(err.kind(), ErrorKind::AddrInUse);
        assert_eq!(socks.len(), 11);

        Ok(())
    });

    sim.run()
}

#[test]
fn try_write() -> Result {
    let mut sim = Builder::new().build();
    sim.client("client", async move {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 1234)).await?;

        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();

            let written = socket.try_write(b"hello!").unwrap();
            assert_eq!(written, 6);
        });

        let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, 1234)).await?;
        let mut buf: [u8; 6] = [0; 6];
        socket.read_exact(&mut buf).await?;
        assert_eq!(&buf, b"hello!");

        Ok(())
    });

    sim.run()
}

#[test]
fn network_partition_drops_inflight_messages() {
    let mut sim = Builder::new()
        .min_message_latency(Duration::from_millis(10))
        .max_message_latency(Duration::from_millis(10))
        .build();

    sim.host("server", || async {
        let listener = bind().await?;
        let (mut sock, _) = listener.accept().await?;
        sock.write_u8(1).await?;
        turmoil::partition("server", "client");
        Ok(())
    });

    sim.client("client", async {
        let mut sock = TcpStream::connect(("server", PORT)).await?;
        let result = timeout(Duration::from_secs(1), sock.read_u8()).await;
        assert!(result.is_err());
        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn network_partition_oneway_drops_inflight_messages() {
    let mut sim = Builder::new()
        .min_message_latency(Duration::from_millis(10))
        .max_message_latency(Duration::from_millis(10))
        .build();

    sim.host("server", || async {
        let listener = bind().await?;
        let (mut sock, _) = listener.accept().await?;
        sock.write_u8(1).await?;
        turmoil::partition_oneway("server", "client");
        Ok(())
    });

    sim.client("client", async {
        let mut sock = TcpStream::connect(("server", PORT)).await?;
        let result = timeout(Duration::from_secs(1), sock.read_u8()).await;
        assert!(result.is_err());
        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn network_hold_holds_inflight_messages() {
    let mut sim = Builder::new()
        .min_message_latency(Duration::from_millis(10))
        .build();

    sim.host("server", || async {
        let listener = bind().await?;
        let (mut sock, _) = listener.accept().await?;
        sock.write_u8(1).await?;

        turmoil::hold("server", "client");
        tokio::time::sleep(Duration::from_secs(1)).await;
        turmoil::release("server", "client");

        Ok(())
    });

    sim.client("client", async {
        let mut sock = TcpStream::connect(("server", PORT)).await?;
        let result = timeout(Duration::from_millis(500), sock.read_u8()).await;
        assert!(result.is_err());
        sock.read_u8().await?;
        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn tcp_write_backpressure_on_hold() {
    let capacity = 4;

    let mut sim = Builder::new()
        .tcp_capacity(capacity)
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(1))
        .build();

    sim.host("server", || async {
        let listener = bind().await?;
        let (mut conn, _) = listener.accept().await?;
        let mut buf = [0u8; 1024];
        while conn.read(&mut buf).await? > 0 {}
        Ok(())
    });

    sim.client("client", async move {
        let mut stream = TcpStream::connect(("server", PORT)).await?;

        turmoil::hold("client", "server");

        for _ in 0..capacity {
            let n = stream.try_write(b"x")?;
            assert_eq!(n, 1);
        }

        // Buffer full — try_write returns WouldBlock
        let err = stream.try_write(b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        // Release the link — server reads, credits are restored
        turmoil::release("client", "server");

        // writable() resolves once credits are available
        stream.writable().await?;
        stream.write_all(b"y").await?;
        stream.shutdown().await?;

        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn tcp_receiver_buffer_full_backpressures() {
    let capacity = 4;

    let mut sim = Builder::new()
        .tcp_capacity(capacity)
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(1))
        .build();

    sim.host("server", || async {
        let listener = bind().await?;
        let (_conn, _) = listener.accept().await?;
        // Emulate a busy server, so it never reads
        future::pending().await
    });

    sim.client("client", async move {
        let stream = TcpStream::connect(("server", PORT)).await?;

        for _ in 0..capacity {
            let n = stream.try_write(b"x")?;
            assert_eq!(n, 1);
        }

        let err = stream.try_write(b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn tcp_write_succeeds_on_healthy_link() {
    let capacity: usize = 4;

    let mut sim = Builder::new()
        .tcp_capacity(capacity)
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(1))
        .build();

    sim.host("server", || async {
        let listener = bind().await?;
        let (mut conn, _) = listener.accept().await?;
        let mut buf = [0u8; 1024];
        while conn.read(&mut buf).await? > 0 {}
        Ok(())
    });

    sim.client("client", async move {
        let mut stream = TcpStream::connect(("server", PORT)).await?;

        // Write more than capacity on a healthy link — messages drain as they
        // are delivered, so the send buffer never fills completely.
        for i in 0u8..100 {
            stream.write_all(&[i]).await?;
        }

        stream.shutdown().await?;
        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn tcp_backpressure_with_oneway_partition() {
    let capacity = 4;

    let mut sim = Builder::new()
        .tcp_capacity(capacity)
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(1))
        .build();

    let accepted = Rc::new(Notify::new());
    let notify = accepted.clone();

    sim.host("server", move || {
        let notify = notify.clone();
        async move {
            let listener = bind().await?;
            let (mut conn, _) = listener.accept().await?;
            notify.notify_one();

            // Fits the client's receive buffer: with ACKs cut off, the server
            // can't learn that the window reopened.
            conn.write_all(b"hell").await?;

            future::pending().await
        }
    });

    sim.client("client", async move {
        let mut stream = TcpStream::connect(("server", PORT)).await?;
        // The handshake's final ACK must reach the server first.
        accepted.notified().await;

        // Partition client→server only; server→client remains open
        turmoil::partition_oneway("client", "server");

        // Client→server: exhaust credits then WouldBlock
        for _ in 0..capacity {
            let n = stream.try_write(b"x")?;
            assert_eq!(n, 1);
        }
        let err = stream.try_write(b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        // Server→client: reads succeed (healthy direction)
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await?;
        assert_eq!(&buf, b"hell");

        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn tcp_try_write_returns_would_block_when_full() {
    let capacity = 4;

    let mut sim = Builder::new()
        .tcp_capacity(capacity)
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(1))
        .build();

    sim.host("server", || async {
        let listener = bind().await?;
        let (_conn, _) = listener.accept().await?;
        future::pending().await
    });

    sim.client("client", async move {
        let stream = TcpStream::connect(("server", PORT)).await?;

        // Hold the link so messages cannot drain
        turmoil::hold("client", "server");

        // Fill the buffer — each try_write succeeds
        for _ in 0..capacity {
            let n = stream.try_write(b"x")?;
            assert_eq!(n, 1);
        }

        // Buffer is full — try_write returns WouldBlock
        let err = stream.try_write(b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn tcp_backpressure_on_loopback() {
    let capacity = 4;

    let mut sim = Builder::new().tcp_capacity(capacity).build();

    sim.client("host", async move {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, PORT)).await?;

        // accept must be spawned concurrently — loopback connect/accept
        // deadlocks if sequential (connect awaits SYN-ACK, accept never runs)
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let (server, _) = listener.accept().await.unwrap();
            tx.send(server).unwrap();
        });

        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, PORT)).await?;
        let mut server = rx.await.unwrap();

        // Fill the send buffer
        for _ in 0..capacity {
            let n = client.try_write(b"x")?;
            assert_eq!(n, 1);
        }

        // Buffer full — WouldBlock
        let err = client.try_write(b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        // Server reads — frees buffer space
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).await?;
        assert_eq!(&buf, b"xxxx");

        // Writer can resume
        client.writable().await?;
        client.write_all(b"y").await?;

        let mut buf = [0u8; 1];
        server.read_exact(&mut buf).await?;
        assert_eq!(&buf, b"y");

        Ok(())
    });

    sim.run().unwrap();
}

// https://github.com/tokio-rs/turmoil/issues/158
//
// Per RFC 9293 §3.10.4, if a receiver closes the connection while there is
// still unread data in the receive queue, the stack MUST send a RST rather
// than a graceful FIN so the peer learns data was lost.
#[test]
fn close_with_unread_data_sends_rst() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await?;
        let (mut s, _) = listener.accept().await?;

        // Send some data the client will never read.
        s.write_all(b"unread").await?;

        // Next I/O on the connection should observe a RST (ConnectionReset),
        // not a graceful EOF.
        let mut buf = [0; 8];
        let err = loop {
            match s.read(&mut buf).await {
                Ok(0) => panic!("expected RST, got graceful EOF"),
                Ok(_) => continue,
                Err(e) => break e,
            }
        };
        assert_eq!(io::ErrorKind::ConnectionReset, err.kind());

        Ok(())
    });

    sim.client("client", async move {
        let s = TcpStream::connect(("server", PORT)).await?;

        // Block until at least one byte has arrived in our receive queue,
        // then drop without consuming it. Peek stashes the bytes in the rx
        // buffer so they count as unread at drop time.
        let mut buf = [0; 1];
        assert!(s.peek(&mut buf).await? >= 1);

        drop(s);

        Ok(())
    });

    sim.run()
}
