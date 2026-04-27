use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};
use turmoil_net::{step, Net};

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
async fn tcp_listener_ttl_roundtrips() {
    use std::io::ErrorKind;

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
