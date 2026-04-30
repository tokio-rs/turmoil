//! Rule installation and fabric behavior under rules.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil_net::fixture::ClientServer;
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream, UdpSocket};
use turmoil_net::{rule, Latency, Verdict};

#[test]
fn latency_delays_delivery() {
    // 50ms one-way latency × 2 directions = 100ms RTT floor. The
    // fixture's tokio clock is 1:1 with fabric time, so elapsed must
    // be at least the full RTT. Without the rule the same flow
    // completes in a handful of ticks — the lower bound proves the
    // rule was honored, not just that the packet eventually arrived.
    let latency = Duration::from_millis(50);
    ClientServer::new()
        .server("server", async move {
            let s = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
            let mut buf = [0u8; 16];
            let (n, from) = s.recv_from(&mut buf).await.unwrap();
            s.send_to(&buf[..n], from).await.unwrap();
        })
        .run("client", async move {
            rule(Latency::fixed(latency)).forget();
            let c = UdpSocket::bind("0.0.0.0:0").await.unwrap();
            let start = tokio::time::Instant::now();
            c.send_to(b"hi", "server:9000").await.unwrap();
            let mut buf = [0u8; 16];
            let (n, _) = c.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"hi");
            let elapsed = start.elapsed();
            assert!(
                elapsed >= 2 * latency,
                "expected ≥ {:?} for one full RTT, got {:?}",
                2 * latency,
                elapsed,
            );
        });
}

#[test]
fn drop_rule_blocks_all_traffic() {
    // With everything dropped, the TCP handshake can never complete.
    ClientServer::new()
        .server("server", async move {
            let _l = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            std::future::pending::<()>().await;
        })
        .run("client", async move {
            rule(|_: &_| Verdict::Drop).forget();

            let result =
                tokio::time::timeout(Duration::from_secs(1), TcpStream::connect("server:9000"))
                    .await;
            assert!(
                result.is_err(),
                "connect should hang when every packet is dropped"
            );
        });
}

#[test]
fn rule_guard_uninstalls_on_drop() {
    // Install, drop the guard, and observe that traffic flows again.
    ClientServer::new()
        .server("server", async move {
            let l = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = [0u8; 16];
                    if let Ok(n) = s.read(&mut buf).await {
                        if n > 0 {
                            let _ = s.write_all(&buf[..n]).await;
                        }
                    }
                });
            }
        })
        .run("client", async move {
            {
                let _g = rule(|_: &_| Verdict::Drop);
                let result = tokio::time::timeout(
                    Duration::from_millis(100),
                    TcpStream::connect("server:9000"),
                )
                .await;
                assert!(result.is_err(), "drop rule should block connect");
            }

            let mut c = TcpStream::connect("server:9000").await.unwrap();
            c.write_all(b"ok").await.unwrap();
            let mut buf = [0u8; 2];
            c.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"ok");
        });
}

#[test]
fn pass_falls_through_to_next_rule() {
    // First rule always passes, second drops — end state is "dropped".
    ClientServer::new()
        .server("server", async move {
            let _l = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            std::future::pending::<()>().await;
        })
        .run("client", async move {
            rule(|_: &_| Verdict::Pass).forget();
            rule(|_: &_| Verdict::Drop).forget();

            let result = tokio::time::timeout(
                Duration::from_millis(100),
                TcpStream::connect("server:9000"),
            )
            .await;
            assert!(result.is_err(), "second rule's Drop should apply");
        });
}

#[test]
fn first_non_pass_wins() {
    // First rule drops, second never runs. A non-Drop second rule
    // demonstrates the short-circuit.
    ClientServer::new()
        .server("server", async move {
            let _l = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            std::future::pending::<()>().await;
        })
        .run("client", async move {
            rule(|_: &_| Verdict::Drop).forget();
            rule(|_: &_| Verdict::Deliver(Duration::ZERO)).forget();

            let result = tokio::time::timeout(
                Duration::from_millis(100),
                TcpStream::connect("server:9000"),
            )
            .await;
            assert!(result.is_err(), "first Drop wins");
        });
}
