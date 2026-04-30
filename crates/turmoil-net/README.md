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
    ClientServer::new()
        .server("server", async {
            let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            // ...
        })
        .run("client", async {
            let stream = TcpStream::connect("server:9000").await.unwrap();
            // ...
        });
}
```

Hostnames are resolved against an in-memory DNS — each new name gets an
IP in `192.168.0.0/16` on first sight. Pass literal `IpAddr`s if you
need a specific address.

Beyond these, reach for `Net`, `add_host`, `enter`, and drive `fabric.step()` on whatever cadence your test needs.

## Rules — fault injection

Every non-loopback packet leaving a host runs through an installed chain of `Rule`s. A rule returns a `Verdict`:

- `Pass` — defer to the next rule in the chain.
- `Deliver(Duration)` — deliver the packet, optionally after a delay. `Duration::ZERO` is immediate.
- `Drop` — discard silently.

Rules are evaluated in install order; the first non-`Pass` verdict wins. An empty chain behaves as `Deliver(0)` — no overhead, same path as pre-rules.

There are three install points, for three different lifetimes:

```rust
// (1) Permanent — lives for the whole Net. Use at setup time.
let mut net = Net::new();
net.rule(Latency::fixed(Duration::from_millis(10)));

// (2) RAII, scheduler-owned — vend a guard on EnterGuard.
//     Drop the guard to uninstall.
let guard = enter_guard.rule(MyRule::new());

// (3) RAII, task-owned — free fn, callable from any task.
//     Same RuleGuard shape.
let guard = turmoil_net::rule(|pkt: &Packet| {
    if pkt.dst == partition_target { Verdict::Drop } else { Verdict::Pass }
});
// ...test code that should see the partition...
drop(guard);  // partition lifts
```

Every rule impls the `Rule` trait (`fn on_packet(&mut self, &Packet) -> Verdict`), and there's a blanket impl for `FnMut(&Packet) -> Verdict` so ad-hoc closures work directly.

Fabric time is driven by the caller. `fabric.step(dt)` advances an internal clock by `dt` and delivers any packets whose scheduled arrival has come due. Pass `Duration::ZERO` to pump the fabric without advancing time — useful when you're stepping a multi-host topology and only want to count one "tick" total across hosts. Rules and their delays live in fabric time, not tokio time: a latency of 10ms means ten 1ms `step`s, regardless of what the async scheduler thinks is happening.

## Status

Experimental. TCP and UDP are functional. TCP retransmit is the next mechanism gap. Higher-level fault primitives — named partitions, reorder, loss distributions — are a `turmoil` concern and will land there, consuming the rule API this crate exposes.
