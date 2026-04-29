use std::io::ErrorKind;

use turmoil_net::shim::tokio::net::UdpSocket;
use turmoil_net::Net;

#[tokio::test]
async fn udp_loopback_roundtrip() {
    Net::lo(async {
        let server = UdpSocket::bind("127.0.0.1:7100").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        client.send_to(b"hello", "127.0.0.1:7100").await.unwrap();

        let mut buf = [0u8; 16];
        let (n, from) = server.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");
        assert!(from.ip().is_loopback());
    })
    .await;
}

#[tokio::test]
async fn udp_connect_send_recv() {
    Net::lo(async {
        let server = UdpSocket::bind("127.0.0.1:7200").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect("127.0.0.1:7200").await.unwrap();

        client.send(b"ping").await.unwrap();

        let mut buf = [0u8; 16];
        let (n, client_addr) = server.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");

        server.connect(client_addr).await.unwrap();
        server.send(b"pong").await.unwrap();

        let mut buf2 = [0u8; 16];
        let n2 = client.recv(&mut buf2).await.unwrap();
        assert_eq!(&buf2[..n2], b"pong");
    })
    .await;
}

#[tokio::test]
async fn udp_send_unconnected_fails() {
    Net::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let err = s.send(b"x").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotConnected);
    })
    .await;
}

#[tokio::test]
async fn udp_oversized_datagram_rejected() {
    Net::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // Loopback MTU is 65536; 65536 - 20 (IP) - 8 (UDP) = 65508 payload cap.
        let too_big = vec![0u8; 65_509];
        let err = s.send_to(&too_big, "127.0.0.1:7000").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    })
    .await;
}

#[tokio::test]
async fn udp_to_unbound_port_is_dropped() {
    // Send to a port with no receiver: no error, no delivery. The
    // datagram is silently dropped in the kernel's deliver step.
    Net::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        s.send_to(b"x", "127.0.0.1:9999").await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn udp_connected_drops_packets_from_non_peer() {
    Net::lo(async {
        let server = UdpSocket::bind("127.0.0.1:5800").await.unwrap();
        server.connect("127.0.0.1:9000").await.unwrap();

        let intruder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        intruder.send_to(b"hi", "127.0.0.1:5800").await.unwrap();

        // recv_from would park forever — use a timeout to assert it
        // never completes in-band.
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
    })
    .await;
}

#[tokio::test]
async fn udp_recv_wakes_when_packet_arrives() {
    // Park a recv; a later send_to from the client must wake it.
    Net::lo(async {
        let server = UdpSocket::bind("127.0.0.1:6000").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let recv = tokio::spawn(async move {
            let mut buf = [0u8; 16];
            let (n, _) = server.recv_from(&mut buf).await.unwrap();
            buf[..n].to_vec()
        });

        client.send_to(b"hi", "127.0.0.1:6000").await.unwrap();
        assert_eq!(recv.await.unwrap(), b"hi");
    })
    .await;
}

#[tokio::test]
async fn udp_peer_addr() {
    Net::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(s.peer_addr().unwrap_err().kind(), ErrorKind::NotConnected);

        s.connect("127.0.0.1:5000").await.unwrap();
        assert_eq!(s.peer_addr().unwrap().port(), 5000);
    })
    .await;
}

#[tokio::test]
async fn udp_peek_from_leaves_datagram() {
    Net::lo(async {
        let server = UdpSocket::bind("127.0.0.1:6100").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(b"hello", "127.0.0.1:6100").await.unwrap();

        let mut buf = [0u8; 16];
        let (n, _) = server.peek_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");

        // Same datagram still there for recv_from.
        let mut buf2 = [0u8; 16];
        let (n2, _) = server.recv_from(&mut buf2).await.unwrap();
        assert_eq!(&buf2[..n2], b"hello");
    })
    .await;
}

#[tokio::test]
async fn udp_try_recv_would_block_when_empty() {
    Net::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(
            s.try_recv_from(&mut buf).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    })
    .await;
}

#[tokio::test]
async fn udp_try_send_and_try_recv() {
    // The try_* syscalls are non-blocking; with no awaits between them
    // the stepper never runs, so the sent datagram would still be
    // queued. One yield hands control to the stepper, which folds the
    // loopback send back into the server's recv queue.
    Net::lo(async {
        let server = UdpSocket::bind("127.0.0.1:6200").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .try_send_to(b"hi", "127.0.0.1:6200".parse().unwrap())
            .unwrap();
        tokio::task::yield_now().await;

        let mut buf = [0u8; 16];
        let (n, _) = server.try_recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hi");
    })
    .await;
}

#[tokio::test]
async fn udp_ttl_roundtrips() {
    Net::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(s.ttl().unwrap(), 64);
        s.set_ttl(32).unwrap();
        assert_eq!(s.ttl().unwrap(), 32);

        assert_eq!(s.set_ttl(256).unwrap_err().kind(), ErrorKind::InvalidInput);
    })
    .await;
}

#[tokio::test]
async fn udp_close_frees_port() {
    Net::lo(async {
        let a = UdpSocket::bind("127.0.0.1:5500").await.unwrap();
        drop(a);
        // Same port now available.
        UdpSocket::bind("127.0.0.1:5500").await.unwrap();
    })
    .await;
}
