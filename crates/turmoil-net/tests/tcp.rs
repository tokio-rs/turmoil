use std::io::ErrorKind;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil_net::fixture;
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};
use turmoil_net::KernelConfig;

#[tokio::test]
async fn tcp_accept_completes_handshake() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7100").await.unwrap();
        let (client, (server, server_peer)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7100"), listener.accept(),).unwrap();

        assert_eq!(client.peer_addr().unwrap().port(), 7100);
        assert!(client.local_addr().unwrap().ip().is_loopback());
        assert_eq!(server_peer, client.local_addr().unwrap());
        assert_eq!(server.peer_addr().unwrap(), client.local_addr().unwrap());
    })
    .await;
}

#[tokio::test]
async fn tcp_roundtrip() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7400").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7400"), listener.accept(),).unwrap();

        let mut buf = [0u8; 5];
        tokio::try_join!(client.write_all(b"hello"), server.read_exact(&mut buf)).unwrap();
        assert_eq!(&buf, b"hello");
    })
    .await;
}

#[tokio::test]
async fn tcp_read_wakes_on_data() {
    // Parked read registers a waker; a later write must wake it.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7500").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7500"), listener.accept(),).unwrap();

        let reader = tokio::spawn(async move {
            let mut buf = [0u8; 3];
            server.read_exact(&mut buf).await.unwrap();
            buf
        });

        client.write_all(b"hey").await.unwrap();
        assert_eq!(&reader.await.unwrap(), b"hey");
    })
    .await;
}

#[tokio::test]
async fn tcp_write_backpressure() {
    // Send buffer cap is 64 KiB; recv buffer cap is 64 KiB. Writing
    // 256 KiB without the server ever reading must block once both
    // are full. Draining the server releases the writer.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7600").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7600"), listener.accept(),).unwrap();

        const N: usize = 256 * 1024;
        let mut writer = tokio::spawn(async move {
            client.write_all(&vec![0u8; N]).await.unwrap();
        });

        // With no one reading, write_all can't complete.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut writer)
                .await
                .is_err(),
            "write_all completed without drain"
        );

        // Drain the server side; writer catches up and finishes.
        let mut buf = vec![0u8; N];
        server.read_exact(&mut buf).await.unwrap();
        writer.await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn tcp_segments_respect_mss() {
    // Loopback MTU is 65536; MSS = 65536 - 20 (IP) - 20 (TCP) = 65496.
    // A 200 KiB write must produce multiple segments. We can't observe
    // the wire but we can confirm the byte stream arrives intact.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7700").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7700"), listener.accept(),).unwrap();

        let payload = (0..50_000u32)
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        let mut got = vec![0u8; 200_000];
        tokio::try_join!(client.write_all(&payload), server.read_exact(&mut got),).unwrap();
        assert_eq!(got, payload);
    })
    .await;
}

#[tokio::test]
async fn tcp_graceful_close_signals_eof() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7800").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7800"), listener.accept(),).unwrap();

        let mut got = Vec::new();
        tokio::try_join!(
            async {
                client.write_all(b"bye").await?;
                client.shutdown().await
            },
            server.read_to_end(&mut got),
        )
        .unwrap();
        assert_eq!(got, b"bye");
    })
    .await;
}

#[tokio::test]
async fn tcp_accept_backlog_drops_excess_syns() {
    // Backlog=2 means only 2 handshaked-but-unaccepted connections sit
    // on the listener's ready queue. A 3rd connect's SYN is dropped by
    // the backlog cap; we don't model SYN retransmit yet, so the client
    // stays parked forever. After the server accepts one, a fresh
    // connect fits in the freed slot.
    fixture::lo_with_config(KernelConfig::default().default_backlog(2), async {
        let listener = TcpListener::bind("127.0.0.1:8100").await.unwrap();

        // Fill the backlog with two handshaked connections. The
        // kernel completes the handshake regardless of accept.
        let _a = TcpStream::connect("127.0.0.1:8100").await.unwrap();
        let _b = TcpStream::connect("127.0.0.1:8100").await.unwrap();

        // Third connect: SYN dropped, client stuck in SynSent.
        // Observe via timeout — the timer also lets the stepper run.
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                TcpStream::connect("127.0.0.1:8100"),
            )
            .await
            .is_err(),
            "connect completed despite full backlog"
        );

        // Drain one slot. A fresh connect now fits and completes.
        let _ = listener.accept().await.unwrap();
        let _d = TcpStream::connect("127.0.0.1:8100").await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn tcp_connect_to_closed_port_is_refused() {
    fixture::lo(async {
        let err = match TcpStream::connect("127.0.0.1:9999").await {
            Ok(_) => panic!("connect unexpectedly succeeded"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), ErrorKind::ConnectionRefused);
    })
    .await;
}

#[tokio::test]
async fn tcp_half_close_send_then_peer_writes_back() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8200").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8200"), listener.accept(),).unwrap();

        // Client writes, shuts down its write side. Server reads + EOF,
        // then writes back. Client reads despite its write side being
        // closed — that's half-close.
        let mut client_read = Vec::new();
        let mut server_read = Vec::new();
        tokio::try_join!(
            async {
                client.write_all(b"hi").await?;
                client.shutdown().await?;
                client.read_to_end(&mut client_read).await.map(|_| ())
            },
            async {
                server.read_to_end(&mut server_read).await?;
                server.write_all(b"back").await?;
                server.shutdown().await
            },
        )
        .unwrap();
        assert_eq!(server_read, b"hi");
        assert_eq!(client_read, b"back");
    })
    .await;
}

#[tokio::test]
async fn tcp_drop_with_unread_bytes_sends_rst() {
    // Server sends bytes to client but client drops without reading.
    // Client's drop emits RST; server observes ConnectionReset on a
    // subsequent write.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8300").await.unwrap();
        let (client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8300"), listener.accept(),).unwrap();

        server.write_all(b"unread").await.unwrap();
        // Wait until the bytes actually reach the client's recv_buf —
        // on_close checks recv_buf to decide RST vs. clean close, so
        // we need them there before drop. peek observes without
        // consuming.
        let mut p = [0u8; 6];
        client.peek(&mut p).await.unwrap();
        drop(client);

        // The first write after the peer's drop may succeed (RST
        // hasn't landed yet); keep writing until one surfaces the
        // reset. Each iteration awaits, so the stepper runs and
        // carries the RST across. Bounded so a regression surfaces
        // as a timeout rather than an infinite loop.
        let observed = tokio::time::timeout(std::time::Duration::from_millis(500), async {
            loop {
                if let Err(e) = server.write_all(b"x").await {
                    break e.kind();
                }
            }
        })
        .await
        .expect("never observed RST");
        assert_eq!(observed, ErrorKind::ConnectionReset);
    })
    .await;
}

#[tokio::test]
async fn tcp_inbound_rst_wakes_parked_read() {
    // Park a reader, then have the peer drop with unread bytes on
    // *its* side — that triggers an abortive close (RST), which our
    // parked reader must observe as ConnectionReset.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8400").await.unwrap();
        let (client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8400"), listener.accept(),).unwrap();

        // Server writes to client — those bytes must land in the
        // client's recv_buf before we drop, otherwise on_close sees
        // an empty recv_buf and elects a clean close. peek blocks on
        // the actual state transition.
        server.write_all(b"unread").await.unwrap();
        let mut p = [0u8; 6];
        client.peek(&mut p).await.unwrap();

        // Park server's read, then drop the client. The RST emitted
        // by client's abortive close wakes the parked read.
        let read = tokio::spawn(async move {
            let mut buf = [0u8; 4];
            server.read(&mut buf).await
        });
        drop(client);

        let err = read.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ConnectionReset);
    })
    .await;
}

#[tokio::test]
async fn tcp_stream_options_roundtrip() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8600").await.unwrap();
        let (client, _) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8600"), listener.accept(),).unwrap();

        assert_eq!(client.ttl().unwrap(), 64);
        client.set_ttl(16).unwrap();
        assert_eq!(client.ttl().unwrap(), 16);

        assert!(!client.nodelay().unwrap());
        client.set_nodelay(true).unwrap();
        assert!(client.nodelay().unwrap());
    })
    .await;
}

#[tokio::test]
async fn tcp_try_read_would_block_on_empty() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8700").await.unwrap();
        let (client, _) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8700"), listener.accept(),).unwrap();

        let mut buf = [0u8; 8];
        assert_eq!(
            client.try_read(&mut buf).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    })
    .await;
}

#[tokio::test]
async fn tcp_peek_leaves_data_for_read() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8800").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8800"), listener.accept(),).unwrap();

        client.write_all(b"PING").await.unwrap();

        let mut p = [0u8; 4];
        let n = server.peek(&mut p).await.unwrap();
        assert_eq!(&p[..n], b"PING");

        // Same bytes still there for a real read.
        let mut r = [0u8; 4];
        server.read_exact(&mut r).await.unwrap();
        assert_eq!(&r, b"PING");
    })
    .await;
}

#[tokio::test]
async fn tcp_listener_ttl_roundtrips() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7300").await.unwrap();
        assert_eq!(listener.ttl().unwrap(), 64);
        listener.set_ttl(16).unwrap();
        assert_eq!(listener.ttl().unwrap(), 16);
        assert_eq!(
            listener.set_ttl(256).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    })
    .await;
}
