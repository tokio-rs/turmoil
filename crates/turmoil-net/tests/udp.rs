use std::io::ErrorKind;

use turmoil_net::shim::tokio::net::UdpSocket;
use turmoil_net::{add_address, step, Net};

#[tokio::test]
async fn udp_loopback_roundtrip() {
    let _guard = Net::new().enter();

    let server = UdpSocket::bind("127.0.0.1:7100").await.unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    client.send_to(b"hello", "127.0.0.1:7100").await.unwrap();
    step();

    let mut buf = [0u8; 16];
    let (n, from) = server.recv_from(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello");
    assert!(from.ip().is_loopback());
}

#[tokio::test]
async fn udp_connect_send_recv() {
    let _guard = Net::new().enter();

    let server = UdpSocket::bind("127.0.0.1:7200").await.unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.connect("127.0.0.1:7200").await.unwrap();

    client.send(b"ping").await.unwrap();
    step();

    let mut buf = [0u8; 16];
    let (n, client_addr) = server.recv_from(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"ping");

    server.connect(client_addr).await.unwrap();
    server.send(b"pong").await.unwrap();
    step();

    let mut buf2 = [0u8; 16];
    let n2 = client.recv(&mut buf2).await.unwrap();
    assert_eq!(&buf2[..n2], b"pong");
}

#[tokio::test]
async fn udp_broadcast_flag() {
    let _guard = Net::new().enter();
    add_address("10.0.0.1".parse().unwrap());

    let s = UdpSocket::bind("10.0.0.1:0").await.unwrap();
    assert!(!s.broadcast().unwrap());
    let err = s.send_to(b"x", "255.255.255.255:9000").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);

    s.set_broadcast(true).unwrap();
    s.send_to(b"x", "255.255.255.255:9000").await.unwrap();
}

#[tokio::test]
async fn udp_send_unconnected_fails() {
    let _guard = Net::new().enter();

    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let err = s.send(b"x").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotConnected);
}

#[tokio::test]
async fn udp_oversized_datagram_rejected() {
    let _guard = Net::new().enter();

    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // Loopback MTU is 65536; 65536 - 20 (IP) - 8 (UDP) = 65508 payload cap.
    let too_big = vec![0u8; 65_509];
    let err = s.send_to(&too_big, "127.0.0.1:7000").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
}

#[tokio::test]
async fn udp_to_unbound_port_is_dropped() {
    let _guard = Net::new().enter();

    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    s.send_to(b"x", "127.0.0.1:9999").await.unwrap();
    // No receiver bound at :9999 — deliver() drops silently, and the
    // loopback packet doesn't escape to the fabric either.
    assert_eq!(step(), 0);
}

#[tokio::test]
async fn udp_connected_drops_packets_from_non_peer() {
    let _guard = Net::new().enter();

    let server = UdpSocket::bind("127.0.0.1:5800").await.unwrap();
    server.connect("127.0.0.1:9000").await.unwrap();

    let intruder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    intruder.send_to(b"hi", "127.0.0.1:5800").await.unwrap();
    step();

    // recv_from would park forever — use a timeout to assert it never
    // completes in-band.
    let mut buf = [0u8; 16];
    let got = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        server.recv_from(&mut buf),
    )
    .await;
    assert!(
        got.is_err(),
        "expected timeout, intruder packet was delivered"
    );
}

#[tokio::test]
async fn udp_recv_wakes_when_packet_arrives() {
    let _guard = Net::new().enter();

    let server = UdpSocket::bind("127.0.0.1:6000").await.unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Park a recv task; it must register a waker and yield.
    let recv = tokio::spawn(async move {
        let mut buf = [0u8; 16];
        let (n, _) = server.recv_from(&mut buf).await.unwrap();
        buf[..n].to_vec()
    });
    tokio::task::yield_now().await;

    client.send_to(b"hi", "127.0.0.1:6000").await.unwrap();
    step();

    let got = recv.await.unwrap();
    assert_eq!(got, b"hi");
}

#[tokio::test]
async fn udp_peer_addr() {
    let _guard = Net::new().enter();

    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    assert_eq!(s.peer_addr().unwrap_err().kind(), ErrorKind::NotConnected);

    s.connect("127.0.0.1:5000").await.unwrap();
    assert_eq!(s.peer_addr().unwrap().port(), 5000);
}

#[tokio::test]
async fn udp_peek_from_leaves_datagram() {
    let _guard = Net::new().enter();

    let server = UdpSocket::bind("127.0.0.1:6100").await.unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(b"hello", "127.0.0.1:6100").await.unwrap();
    step();

    let mut buf = [0u8; 16];
    let (n, _) = server.peek_from(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello");

    // Same datagram still there for recv_from.
    let mut buf2 = [0u8; 16];
    let (n2, _) = server.recv_from(&mut buf2).await.unwrap();
    assert_eq!(&buf2[..n2], b"hello");
}

#[tokio::test]
async fn udp_try_recv_would_block_when_empty() {
    let _guard = Net::new().enter();

    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(
        s.try_recv_from(&mut buf).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn udp_try_send_and_try_recv() {
    let _guard = Net::new().enter();

    let server = UdpSocket::bind("127.0.0.1:6200").await.unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client
        .try_send_to(b"hi", "127.0.0.1:6200".parse().unwrap())
        .unwrap();
    step();

    let mut buf = [0u8; 16];
    let (n, _) = server.try_recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hi");
}

#[tokio::test]
async fn udp_ttl_roundtrips() {
    let _guard = Net::new().enter();

    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    assert_eq!(s.ttl().unwrap(), 64);
    s.set_ttl(32).unwrap();
    assert_eq!(s.ttl().unwrap(), 32);

    assert_eq!(s.set_ttl(256).unwrap_err().kind(), ErrorKind::InvalidInput);
}

#[tokio::test]
async fn udp_close_frees_port() {
    let _guard = Net::new().enter();

    let a = UdpSocket::bind("127.0.0.1:5500").await.unwrap();
    drop(a);
    // Same port now available.
    UdpSocket::bind("127.0.0.1:5500").await.unwrap();
}

#[tokio::test]
async fn bind_zero_avoids_ports_taken_on_other_ips() {
    // Linux: `:0` picks a port not in use at any IP for the same
    // (domain, ty). Regression for a prior bug where the allocator
    // only checked the (domain, ty, ip, port) tuple and would happily
    // return a port already bound on a different IP.
    //
    // The allocator cursor starts at 49152, so that's the first port
    // it would hand out. Squatting it on 10.0.0.1 should force the
    // subsequent `:0` bind to pick a different port rather than
    // colliding.
    let _guard = Net::new().enter();
    add_address("10.0.0.1".parse().unwrap());

    let squatter = UdpSocket::bind("10.0.0.1:49152").await.unwrap();
    let _ = squatter;

    let ephemeral = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    assert_ne!(ephemeral.local_addr().unwrap().port(), 49152);
}
