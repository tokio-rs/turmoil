//! Inter-host delivery smoke tests.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil_net::fixture::ClientServer;
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream, UdpSocket};

#[tokio::test]
async fn udp_across_two_hosts() {
    let a: std::net::IpAddr = "10.0.0.1".parse().unwrap();
    let b: std::net::IpAddr = "10.0.0.2".parse().unwrap();

    ClientServer::new()
        .server([a], async move {
            let s = UdpSocket::bind("10.0.0.1:9000").await.unwrap();
            let mut buf = [0u8; 16];
            let (n, from) = s.recv_from(&mut buf).await.unwrap();
            s.send_to(&buf[..n], from).await.unwrap();
        })
        .run([b], async move {
            let c = UdpSocket::bind("10.0.0.2:0").await.unwrap();
            c.send_to(b"hi", "10.0.0.1:9000").await.unwrap();
            let mut buf = [0u8; 16];
            let (n, from) = c.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"hi");
            assert_eq!(from.ip(), a);
        })
        .await;
}

#[tokio::test]
async fn tcp_across_two_hosts() {
    let a: std::net::IpAddr = "10.0.0.1".parse().unwrap();
    let b: std::net::IpAddr = "10.0.0.2".parse().unwrap();

    ClientServer::new()
        .server([a], async move {
            let listener = TcpListener::bind("10.0.0.1:9000").await.unwrap();
            let (mut sock, peer) = listener.accept().await.unwrap();
            assert_eq!(peer.ip(), b);
            let mut buf = [0u8; 5];
            sock.read_exact(&mut buf).await.unwrap();
            sock.write_all(&buf).await.unwrap();
        })
        .run([b], async move {
            let mut c = TcpStream::connect("10.0.0.1:9000").await.unwrap();
            c.write_all(b"hello").await.unwrap();
            let mut buf = [0u8; 5];
            c.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello");
            assert_eq!(c.peer_addr().unwrap().ip(), a);
        })
        .await;
}
