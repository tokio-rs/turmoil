# turmoil-net

A deterministic networking substrate for testing async Rust.

## What it is

`turmoil-net` is a simulated socket stack — a POSIX-shaped kernel behind a tokio-compatible shim. The shim mirrors `tokio::net` exactly: same type names, same method signatures, same error semantics. Production code imports `tokio::net`; tests flip the same import to `turmoil_net::shim::tokio::net` behind a `#[cfg]` and the same code runs against a simulated network you control.

```rust
#[cfg(not(test))]
use tokio::net::TcpStream;
#[cfg(test)]
use turmoil_net::shim::tokio::net::TcpStream;
```

No wrapper types, no trait objects, no conditional method calls in the code under test.

It is not a full simulation runtime — it doesn't own your scheduler, intercept time, or replay your test from a seed. It's the networking piece. If you want the rest, the `turmoil` crate (one level up in this repo) layers on top once those stories are solid.

## Why

Tests that touch real sockets are nondeterministic: timing, port assignment, kernel retransmit, and loopback scheduling all vary run to run.

Mocks are the usual workaround, but they stop one layer too shallow. A mocked `TcpStream` returns the bytes you prime it with — it doesn't model what actually happens between two components on a network. No handshake, no backpressure when the peer stops reading, no FIN/RST when a connection tears down, no partial writes, no reorder under load. The bugs that matter in a distributed system live in those interactions, and mocks paper right over them.

`turmoil-net` runs the real protocol — handshake, FIN, RST, MSS, backpressure — between real instances of your components, on a deterministic substrate you drive yourself. Your client and server are the same code that runs in production, talking to each other through a simulated network that behaves like a network.

Because the network is simulated and deterministic, you can test the adversarial cases too — drop packets, partition hosts, inject latency, reorder segments, kill a connection mid-stream. These are the scenarios a distributed system exists to survive, and they're the scenarios that are painful-to-impossible to provoke with real sockets or stub with mocks.

## Composability

The primitives are exposed directly: `Net`, `EnterGuard`, `HostId`, `fabric.step()`. You build the fixture you need. The `fixture` module ships two common shapes — `lo` for single-host tests and `ClientServer` for N-server/1-client topologies — but they're built from the same primitives you can reach for yourself when you outgrow them. If you want custom host topology, per-host clock skew, or a bespoke scheduling loop, you write it against the same API the fixtures use.

## Using it

The quick path, for a single-host test:

```rust
use turmoil_net::fixture;
use turmoil_net::shim::tokio::net::UdpSocket;

#[test]
fn udp_loopback() {
    fixture::lo(async {
        let server = UdpSocket::bind("127.0.0.1:9000").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(b"hi", "127.0.0.1:9000").await.unwrap();

        let mut buf = [0u8; 16];
        let (n, _) = server.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hi");
    });
}
```

Two hosts, one test:

```rust
use turmoil_net::fixture::ClientServer;
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};

#[test]
fn tcp_across_hosts() {
    let server_ip: std::net::IpAddr = "10.0.0.1".parse().unwrap();
    let client_ip: std::net::IpAddr = "10.0.0.2".parse().unwrap();

    ClientServer::new()
        .server([server_ip], async {
            let listener = TcpListener::bind("10.0.0.1:9000").await.unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            // ...
        })
        .run([client_ip], async {
            let stream = TcpStream::connect("10.0.0.1:9000").await.unwrap();
            // ...
        });
}
```

Beyond these, reach for `Net`, `add_host`, `enter`, and drive `fabric.step()` on whatever cadence your test needs.

## Status

Experimental. TCP and UDP basics work end-to-end. Synthetic latency, packet loss, partition filters, and TCP retransmit are in progress.
