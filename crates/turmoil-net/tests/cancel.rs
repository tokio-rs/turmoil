//! Test Cancellation semantics.

use std::future::{poll_fn, Future};
use std::task::Poll;

use tokio::io::AsyncReadExt;
use turmoil_net::fixture::{self, ClientServer};
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};
use turmoil_net::{netstat, rule, Verdict};

#[test]
fn cancelled_connect_closes_socket() {
    // Drop every packet so the handshake parks in SynSent, then
    // cancel the connect future by dropping it. The preallocated fd
    // must be reaped via FdGuard — otherwise it leaks as a zombie.
    ClientServer::new()
        .server("server", async move {
            let _l = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            std::future::pending::<()>().await;
        })
        .run("client", async move {
            rule(|_: &_| Verdict::Drop).forget();

            // Poll connect once to allocate the fd + send SYN, then drop the
            // boxed future to trigger cancellation. We know the future is
            // Pending after one poll because every packet is dropped.
            let mut fut: std::pin::Pin<Box<dyn Future<Output = _>>> =
                Box::pin(TcpStream::connect("server:9000"));
            poll_fn(|cx| {
                assert!(fut.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(fut);

            let net_stat = netstat("client");
            assert!(
                net_stat.entries.is_empty(),
                "client socket table should be empty after cancelled connect, got:\n{net_stat}"
            );
        });
}

#[test]
fn dropping_listener_with_unaccepted_connections_rsts() {
    // Listener + client in the same LocalSet on lo. The handshake
    // completes but accept is never called, so the child sits in
    // listen.ready. Dropping the listener must RST it — observable
    // as ConnectionReset on the client's read.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:9000").await.unwrap();
        let mut client = TcpStream::connect("127.0.0.1:9000").await.unwrap();
        drop(listener);

        let mut buf = [0u8; 1];
        let err = client.read(&mut buf).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset);
    });
}
