use std::io::ErrorKind;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};
use turmoil_net::{step, KernelConfig, Net};

/// Drive the simulated stack forward by interleaving yields (so task
/// wakers get a chance to fire) and `step()` (which flushes `egress`).
/// Eight iterations is plenty for any test here — the longest path
/// (connect → write → ACK → read) takes ~4 steps.
async fn step_n(iters: usize) {
    for _ in 0..iters {
        tokio::task::yield_now().await;
        step();
    }
}

#[tokio::test]
async fn tcp_accept_completes_handshake() {
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:7100").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:7100"));

    step_n(4).await;

    let client = connect.await.unwrap().unwrap();
    let (server, server_peer) = accept.await.unwrap().unwrap();

    assert_eq!(client.peer_addr().unwrap().port(), 7100);
    assert!(client.local_addr().unwrap().ip().is_loopback());
    assert_eq!(server_peer, client.local_addr().unwrap());
    assert_eq!(server.peer_addr().unwrap(), client.local_addr().unwrap());
}

#[tokio::test]
async fn tcp_roundtrip() {
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:7400").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:7400"));
    step_n(8).await;

    let mut client = connect.await.unwrap().unwrap();
    let (mut server, _) = accept.await.unwrap().unwrap();

    // Write from client, read from server.
    let writer = tokio::spawn(async move {
        client.write_all(b"hello").await.unwrap();
        client
    });
    let reader = tokio::spawn(async move {
        let mut buf = [0u8; 5];
        server.read_exact(&mut buf).await.unwrap();
        buf
    });
    step_n(8).await;

    let _client = writer.await.unwrap();
    let buf = reader.await.unwrap();
    assert_eq!(&buf, b"hello");
}

#[tokio::test]
async fn tcp_read_wakes_on_data() {
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:7500").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:7500"));
    step_n(8).await;
    let mut client = connect.await.unwrap().unwrap();
    let (mut server, _) = accept.await.unwrap().unwrap();

    // Park a reader before any data arrives; it must register a waker
    // and yield.
    let reader = tokio::spawn(async move {
        let mut buf = [0u8; 3];
        server.read_exact(&mut buf).await.unwrap();
        buf
    });
    tokio::task::yield_now().await;

    client.write_all(b"hey").await.unwrap();
    step_n(8).await;

    let buf = reader.await.unwrap();
    assert_eq!(&buf, b"hey");
}

#[tokio::test]
async fn tcp_write_backpressure() {
    // Fill the peer's receive buffer + our own send buffer without
    // ever draining. Further writes must yield `Pending` — i.e. the
    // poll-style `write` returns Pending rather than accepting more
    // bytes. Use `tokio::time::timeout` because a stuck `write_all`
    // would otherwise hang the test.
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:7600").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:7600"));
    step_n(8).await;
    let mut client = connect.await.unwrap().unwrap();
    let (_server, _) = accept.await.unwrap().unwrap();

    // Send buffer cap is 64 KiB; recv buffer cap is 64 KiB. Writing
    // 256 KiB without the server ever reading must block once both
    // are full.
    let writer = tokio::spawn(async move {
        let big = vec![0u8; 256 * 1024];
        client.write_all(&big).await
    });
    step_n(16).await;

    // Writer should still be in flight — server never drained.
    assert!(!writer.is_finished(), "write_all completed without drain");
    writer.abort();
}

#[tokio::test]
async fn tcp_segments_respect_mss() {
    // Loopback MTU is 65536; MSS = 65536 - 20 (IP) - 20 (TCP) = 65496.
    // A single 200 KiB write must produce multiple segments, not one
    // giant packet. We can't directly observe the wire, but we can
    // confirm the byte stream arrives intact on the other side.
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:7700").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:7700"));
    step_n(8).await;
    let mut client = connect.await.unwrap().unwrap();
    let (mut server, _) = accept.await.unwrap().unwrap();

    let payload = (0..50_000u32)
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let expected = payload.clone();
    let writer = tokio::spawn(async move {
        client.write_all(&payload).await.unwrap();
        client
    });
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; 200_000];
        server.read_exact(&mut buf).await.unwrap();
        buf
    });
    step_n(32).await;

    let _client = writer.await.unwrap();
    let got = reader.await.unwrap();
    assert_eq!(got, expected);
}

#[tokio::test]
async fn tcp_graceful_close_signals_eof() {
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:7800").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:7800"));
    step_n(8).await;
    let mut client = connect.await.unwrap().unwrap();
    let (mut server, _) = accept.await.unwrap().unwrap();

    // Client writes then shuts down its write side. Server must read
    // the payload, then see EOF (`read` returning `Ok(0)`).
    let writer = tokio::spawn(async move {
        client.write_all(b"bye").await.unwrap();
        client.shutdown().await.unwrap();
        client
    });
    let reader = tokio::spawn(async move {
        let mut buf = Vec::new();
        // read_to_end drains until EOF.
        tokio::io::AsyncReadExt::read_to_end(&mut server, &mut buf)
            .await
            .unwrap();
        buf
    });
    step_n(16).await;

    let _client = writer.await.unwrap();
    let got = reader.await.unwrap();
    assert_eq!(got, b"bye");
}

#[tokio::test]
async fn tcp_accept_backlog_drops_excess_syns() {
    // Backlog=2 means only 2 in-flight/ready connections are allowed.
    // A 3rd connect's SYN gets dropped and the client stays parked.
    let _guard = Net::with_config(KernelConfig::default().default_backlog(2)).enter();

    let listener = TcpListener::bind("127.0.0.1:8100").await.unwrap();

    // Fill the backlog with two connects; don't accept them.
    let a = tokio::spawn(TcpStream::connect("127.0.0.1:8100"));
    let b = tokio::spawn(TcpStream::connect("127.0.0.1:8100"));
    step_n(8).await;
    assert!(a.is_finished());
    assert!(b.is_finished());

    // Third connect: SYN should be dropped by accept_syn's capacity
    // check. The client sits in SynSent forever.
    let c = tokio::spawn(TcpStream::connect("127.0.0.1:8100"));
    step_n(8).await;
    assert!(!c.is_finished(), "connect completed despite full backlog");

    // Drain once — accepting frees a slot, the client retries, and c
    // completes on the next pump. We don't model SYN retries yet, so
    // just assert the new connect is *still* parked.
    let _ = listener.accept().await.unwrap();
    step_n(8).await;
    assert!(!c.is_finished(), "no SYN retry modeled yet");

    c.abort();
}

#[tokio::test]
async fn tcp_connect_to_closed_port_is_refused() {
    let _guard = Net::new().enter();

    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:9999"));
    step_n(4).await;
    let res = connect.await.unwrap();
    let err = match res {
        Ok(_) => panic!("connect unexpectedly succeeded"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), ErrorKind::ConnectionRefused);
}

#[tokio::test]
async fn tcp_half_close_send_then_peer_writes_back() {
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:8200").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:8200"));
    step_n(8).await;
    let mut client = connect.await.unwrap().unwrap();
    let (mut server, _) = accept.await.unwrap().unwrap();

    // Client writes, then shuts down its write side. Server reads the
    // bytes + EOF, then writes a reply. Client reads the reply despite
    // its own write side being closed — that's half-close.
    let client_task = tokio::spawn(async move {
        client.write_all(b"hi").await.unwrap();
        client.shutdown().await.unwrap();
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        buf
    });
    let server_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        server.read_to_end(&mut buf).await.unwrap();
        server.write_all(b"back").await.unwrap();
        server.shutdown().await.unwrap();
        buf
    });
    step_n(24).await;

    let server_read = server_task.await.unwrap();
    let client_read = client_task.await.unwrap();
    assert_eq!(server_read, b"hi");
    assert_eq!(client_read, b"back");
}

#[tokio::test]
async fn tcp_drop_with_unread_bytes_sends_rst() {
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:8300").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:8300"));
    step_n(8).await;
    let client = connect.await.unwrap().unwrap();
    let (server, _) = accept.await.unwrap().unwrap();

    // Server sends bytes to client but client drops without reading.
    // Client drop should emit RST; server's next write observes it.
    let mut server = server;
    server.write_all(b"unread").await.unwrap();
    step_n(8).await;

    drop(client);
    step_n(8).await;

    // Server's next write may succeed (RST arrives async) but a
    // subsequent one after another pump surfaces the reset. Use a
    // loop bounded to keep the test deterministic.
    let mut observed = None;
    for _ in 0..8 {
        match server.write_all(b"x").await {
            Ok(_) => step_n(2).await,
            Err(e) => {
                observed = Some(e.kind());
                break;
            }
        }
    }
    assert_eq!(observed, Some(ErrorKind::ConnectionReset));
}

#[tokio::test]
async fn tcp_inbound_rst_wakes_parked_read() {
    // Park a reader, then have the peer drop with unread bytes on
    // *its* side — that triggers an abortive close (RST), which our
    // parked reader must observe as ConnectionReset.
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:8400").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:8400"));
    step_n(8).await;
    let client = connect.await.unwrap().unwrap();
    let (mut server, _) = accept.await.unwrap().unwrap();

    // Server writes to client — those bytes sit unread in client's
    // recv_buf until the drop triggers RST.
    server.write_all(b"unread").await.unwrap();
    step_n(8).await;

    // Park a read on the server; it'll wake when the client's RST
    // tears the connection down.
    let read = tokio::spawn(async move {
        let mut buf = [0u8; 4];
        server.read(&mut buf).await
    });
    tokio::task::yield_now().await;

    drop(client);
    step_n(8).await;

    let err = read.await.unwrap().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConnectionReset);
}

#[tokio::test]
async fn tcp_listener_ttl_roundtrips() {

    let _guard = Net::new().enter();
    let listener = TcpListener::bind("127.0.0.1:7300").await.unwrap();
    assert_eq!(listener.ttl().unwrap(), 64);
    listener.set_ttl(16).unwrap();
    assert_eq!(listener.ttl().unwrap(), 16);
    assert_eq!(
        listener.set_ttl(256).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}
