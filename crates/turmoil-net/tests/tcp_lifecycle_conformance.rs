//! TCP lifecycle scenarios shared with native Tokio.
//!
//! To run on Linux sockets, compile this test with `--cfg parity_tokio`.
//! Native listeners bind only loopback and use ephemeral ports.

use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::sleep;

#[cfg(parity_tokio)]
use tokio::net::{TcpListener, TcpStream};
#[cfg(not(parity_tokio))]
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};

fn run(test: impl Future<Output = ()> + 'static) {
    let test = async {
        tokio::time::timeout(Duration::from_secs(5), test)
            .await
            .expect("TCP scenario timed out");
    };
    #[cfg(not(parity_tokio))]
    turmoil_net::fixture::lo(test);
    #[cfg(parity_tokio)]
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

#[test]
fn mismatched_reunite_returns_usable_halves() {
    run(async {
        for from_read in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (a, (mut server_a, _)) =
                tokio::try_join!(TcpStream::connect(address), listener.accept()).unwrap();
            let (b, (mut server_b, _)) =
                tokio::try_join!(TcpStream::connect(address), listener.accept()).unwrap();
            let (read_a, _write_a) = a.into_split();
            let (_read_b, write_b) = b.into_split();
            let error = if from_read {
                read_a.reunite(write_b)
            } else {
                write_b.reunite(read_a)
            }
            .unwrap_err();
            let (mut read_a, mut write_b) = (error.0, error.1);

            write_b.write_all(b"b").await.unwrap();
            assert_eq!(server_b.read_u8().await.unwrap(), b'b');
            server_a.write_all(b"a").await.unwrap();
            assert_eq!(read_a.read_u8().await.unwrap(), b'a');
        }
    });
}

#[test]
fn connect_polled_after_peer_fin_preserves_response() {
    run(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut connecting = Box::pin(TcpStream::connect(listener.local_addr().unwrap()));
        poll_fn(|cx| {
            assert!(connecting.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        // Complete the handshake and peer shutdown while leaving the
        // connect future unpolled, as other selected work can do.
        let (mut server, _) = listener.accept().await.unwrap();
        server.write_all(b"hello").await.unwrap();
        server.shutdown().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut client = connecting.await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"hello");
        // The peer's FIN closes only the receive direction.
        client.write_all(b"ack").await.unwrap();
        let mut ack = [0; 3];
        server.read_exact(&mut ack).await.unwrap();
        assert_eq!(&ack, b"ack");
    });
}

async fn pair() -> std::io::Result<(TcpListener, TcpStream, TcpStream)> {
    let listener = TcpListener::bind("127.0.0.1:1738").await?;
    let (client, (server, _)) =
        tokio::try_join!(TcpStream::connect("127.0.0.1:1738"), listener.accept())?;
    Ok((listener, client, server))
}

#[test]
fn listener_rebind_with_live_accepted_connection() {
    run(async {
        let result: std::io::Result<()> = async {
            let (listener, _client, _server) = pair().await?;
            drop(listener);
            let rebound = TcpListener::bind("127.0.0.1:1738").await;
            assert!(rebound.is_ok(), "listener rebind failed: {rebound:?}");
            Ok(())
        }
        .await;
        result.unwrap();
    });
}

#[test]
fn reset_preserves_already_received_bytes() {
    run(async {
        let result: std::io::Result<()> = async {
            let (_listener, mut client, mut server) = pair().await?;
            client.write_all(b"unread request").await?;
            server.write_all(b"response").await?;
            sleep(Duration::from_millis(10)).await;
            drop(server); // Unread request causes a reset.
            sleep(Duration::from_millis(10)).await;
            let mut buf = [0; 8];
            let read = client.read(&mut buf).await;
            assert!(matches!(read, Ok(8)), "buffered response lost: {read:?}");
            assert_eq!(&buf, b"response");
            Ok(())
        }
        .await;
        result.unwrap();
    });
}
