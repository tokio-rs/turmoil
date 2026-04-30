use std::io::ErrorKind;

use turmoil_net::fixture;
use turmoil_net::shim::tokio::net::UdpSocket;

#[test]
fn udp_loopback_roundtrip() {
    fixture::lo(async {
        let server = UdpSocket::bind("127.0.0.1:7100").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        client.send_to(b"hello", "127.0.0.1:7100").await.unwrap();

        let mut buf = [0u8; 16];
        let (n, from) = server.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");
        assert!(from.ip().is_loopback());
    });
}

#[test]
fn udp_connect_send_recv() {
    fixture::lo(async {
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
    });
}

#[test]
fn udp_send_unconnected_fails() {
    fixture::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let err = s.send(b"x").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotConnected);
    });
}

#[test]
fn udp_oversized_datagram_rejected() {
    fixture::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // Loopback MTU is 65536; 65536 - 20 (IP) - 8 (UDP) = 65508 payload cap.
        // Mirror Linux: EMSGSIZE (errno 90), which surfaces as raw os error
        // since the matching `ErrorKind::FileTooLarge` is unstable.
        let too_big = vec![0u8; 65_509];
        let err = s.send_to(&too_big, "127.0.0.1:7000").await.unwrap_err();
        assert_eq!(err.raw_os_error(), Some(90));
    });
}

#[test]
fn udp_to_unbound_port_is_dropped() {
    // Send to a port with no receiver: no error, no delivery. The
    // datagram is silently dropped in the kernel's deliver step.
    fixture::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        s.send_to(b"x", "127.0.0.1:9999").await.unwrap();
    });
}

#[test]
fn udp_connected_drops_packets_from_non_peer() {
    fixture::lo(async {
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
    });
}

#[test]
fn udp_recv_wakes_when_packet_arrives() {
    // Park a recv; a later send_to from the client must wake it.
    fixture::lo(async {
        let server = UdpSocket::bind("127.0.0.1:6000").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let recv = tokio::spawn(async move {
            let mut buf = [0u8; 16];
            let (n, _) = server.recv_from(&mut buf).await.unwrap();
            buf[..n].to_vec()
        });

        client.send_to(b"hi", "127.0.0.1:6000").await.unwrap();
        assert_eq!(recv.await.unwrap(), b"hi");
    });
}

#[test]
fn udp_peer_addr() {
    fixture::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(s.peer_addr().unwrap_err().kind(), ErrorKind::NotConnected);

        s.connect("127.0.0.1:5000").await.unwrap();
        assert_eq!(s.peer_addr().unwrap().port(), 5000);
    });
}

#[test]
fn udp_peek_from_leaves_datagram() {
    fixture::lo(async {
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
    });
}

#[test]
fn udp_try_recv_would_block_when_empty() {
    fixture::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(
            s.try_recv_from(&mut buf).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    });
}

#[test]
fn udp_try_send_and_try_recv() {
    // The try_* syscalls are non-blocking; with no awaits between them
    // the stepper never runs, so the sent datagram would still be
    // queued on the sender's outbound. peek_from parks until data
    // lands in the server's recv queue, which forces a fabric tick —
    // after that, try_recv_from sees the same bytes.
    fixture::lo(async {
        let server = UdpSocket::bind("127.0.0.1:6200").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .try_send_to(b"hi", "127.0.0.1:6200".parse().unwrap())
            .unwrap();

        let mut peek = [0u8; 16];
        server.peek_from(&mut peek).await.unwrap();

        let mut buf = [0u8; 16];
        let (n, _) = server.try_recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hi");
    });
}

#[test]
fn udp_ttl_roundtrips() {
    fixture::lo(async {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(s.ttl().unwrap(), 64);
        s.set_ttl(32).unwrap();
        assert_eq!(s.ttl().unwrap(), 32);

        assert_eq!(s.set_ttl(256).unwrap_err().kind(), ErrorKind::InvalidInput);
    });
}

#[test]
fn udp_close_frees_port() {
    fixture::lo(async {
        let a = UdpSocket::bind("127.0.0.1:5500").await.unwrap();
        drop(a);
        // Same port now available.
        UdpSocket::bind("127.0.0.1:5500").await.unwrap();
    });
}
