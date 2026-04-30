//! Per-host netstat snapshots.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil_net::fixture::ClientServer;
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream, UdpSocket};
use turmoil_net::{netstat, NetstatState, Proto};

#[test]
fn listener_shows_as_listen() {
    ClientServer::new()
        .server("server", async move {
            let _l = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            std::future::pending::<()>().await;
        })
        .run("client", async move {
            // Give the server a chance to bind.
            let _ = TcpStream::connect("server:9000").await.unwrap();
            let snap = netstat("server");
            let listen = snap
                .entries
                .iter()
                .find(|e| e.state == Some(NetstatState::Listen))
                .expect("listener present");
            assert_eq!(listen.proto, Proto::Tcp);
            assert_eq!(listen.local.port(), 9000);
            assert!(listen.peer.is_none());
        });
}

#[test]
fn established_connection_on_both_sides() {
    ClientServer::new()
        .server("server", async move {
            let l = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 1];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(b"y").await.unwrap();
            let mut buf = [0u8; 1];
            let _ = s.read(&mut buf).await;
        })
        .run("client", async move {
            let mut c = TcpStream::connect("server:9000").await.unwrap();
            c.write_all(b"x").await.unwrap();
            // Wait for the reply — guarantees the server side has
            // reached Established and sent at least one segment back.
            let mut buf = [0u8; 1];
            c.read_exact(&mut buf).await.unwrap();

            let server = netstat("server");
            let client = netstat("client");

            let est = |snap: &turmoil_net::Netstat| {
                snap.entries
                    .iter()
                    .filter(|e| e.state == Some(NetstatState::Established))
                    .count()
            };
            assert_eq!(est(&server), 1, "server has one established conn");
            assert_eq!(est(&client), 1, "client has one established conn");
        });
}

#[test]
fn udp_bound_socket_is_listed() {
    ClientServer::new()
        .server("server", async move {
            let _s = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            std::future::pending::<()>().await;
        })
        .run("client", async move {
            let snap = netstat("server");
            let udp = snap
                .entries
                .iter()
                .find(|e| e.proto == Proto::Udp)
                .expect("udp socket present");
            assert_eq!(udp.local.port(), 9000);
            assert!(udp.state.is_none());
        });
}

#[test]
fn by_ip_literal() {
    ClientServer::new()
        .server("server", async move {
            let _l = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            std::future::pending::<()>().await;
        })
        .run("client", async move {
            // "server" resolves to 192.168.0.1 — look it up by IP too.
            let snap = netstat("192.168.0.1".parse::<std::net::IpAddr>().unwrap());
            assert!(snap
                .entries
                .iter()
                .any(|e| e.state == Some(NetstatState::Listen)));
        });
}
