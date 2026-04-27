use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};
use turmoil_net::{step, Net};

#[tokio::test]
async fn tcp_accept_completes_handshake() {
    let _guard = Net::new().enter();

    let listener = TcpListener::bind("127.0.0.1:7100").await.unwrap();
    let accept = tokio::spawn(async move { listener.accept().await });
    let connect = tokio::spawn(TcpStream::connect("127.0.0.1:7100"));

    // Drive the handshake: each `step()` drains egress and folds
    // loopback back through `deliver`; yields let the connect/accept
    // tasks observe their wakers.
    for _ in 0..8 {
        tokio::task::yield_now().await;
        step();
    }

    let client = connect.await.unwrap().unwrap();
    let (server, server_peer) = accept.await.unwrap().unwrap();

    assert_eq!(client.peer_addr().unwrap().port(), 7100);
    assert!(client.local_addr().unwrap().ip().is_loopback());
    assert_eq!(server_peer, client.local_addr().unwrap());
    assert_eq!(server.peer_addr().unwrap(), client.local_addr().unwrap());
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
