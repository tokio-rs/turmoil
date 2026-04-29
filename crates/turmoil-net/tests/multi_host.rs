//! Inter-host delivery smoke tests.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil_net::fixture::ClientServer;
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream, UdpSocket};

#[test]
fn udp_across_two_hosts() {
    ClientServer::new()
        .server("server", async move {
            let s = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            let mut buf = [0u8; 16];
            let (n, from) = s.recv_from(&mut buf).await.unwrap();
            s.send_to(&buf[..n], from).await.unwrap();
        })
        .run("client", async move {
            let c = UdpSocket::bind("0.0.0.0:0").await.unwrap();
            c.send_to(b"hi", "server:9000").await.unwrap();
            let mut buf = [0u8; 16];
            let (n, _) = c.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"hi");
        });
}

#[test]
fn tcp_across_two_hosts() {
    ClientServer::new()
        .server("server", async move {
            let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 5];
            sock.read_exact(&mut buf).await.unwrap();
            sock.write_all(&buf).await.unwrap();
        })
        .run("client", async move {
            let mut c = TcpStream::connect("server:9000").await.unwrap();
            c.write_all(b"hello").await.unwrap();
            let mut buf = [0u8; 5];
            c.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello");
        });
}
