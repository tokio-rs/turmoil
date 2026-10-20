# turmoil-net

A deterministic networking substrate for testing async Rust.

## What it is

`turmoil-net` is a simulated socket stack. Production code imports `tokio::net`; tests flip the same import to `turmoil_net::shim::tokio::net` behind a `#[cfg]` and the same code runs against a simulated network you control.

```rust
#[cfg(not(test))]
use tokio::net::TcpStream;
#[cfg(test)]
use turmoil_net::shim::tokio::net::TcpStream;
```

## Why

Tests that touch real sockets are nondeterministic — timing, port assignment, and loopback scheduling all vary run to run.

A mocked `TcpStream` returns the bytes you prime it with; it doesn't model handshake, backpressure, FIN/RST, partial writes, or reordering under load.

`turmoil-net` runs a real protocol implementation (handshake, FIN, RST, MSS, backpressure) between real instances of your components, on a deterministic substrate you drive yourself. On top of that substrate, tests can drop packets, partition hosts, inject latency, reorder segments, or tear down connections mid-stream.

## Composability

The primitives are public. The `fixture` module ships `lo` for single-host tests and `ClientServer` for N-server/1-client topologies; both are thin wrappers. For custom topologies, per-host clock skew, or a different scheduling loop, build against the primitives directly.

The stack is clock-free: packet routing is synchronous, and time plus the pending-delivery queue live in the harness. The built-in tokio fixtures ship a reference scheduler; a custom harness supplies its own.

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

## UDP broadcast and multicast

`UdpSocket` supports IPv4 broadcast (`set_broadcast`) and IPv4/IPv6
multicast (`join_multicast_*`, `leave_multicast_*`, and
`set_multicast_loop_*`). Multicast loopback defaults to enabled; the
sending socket controls whether local members receive a copy.
`set_multicast_ttl_v4(0)` limits multicast to local members.

The fabric models one shared link plus each host's loopback. Broadcast
reaches IPv4 sockets on the destination port; multicast reaches sockets
that joined the group on that port. Wildcard binds can receive from
either scope; concrete binds receive only within their loopback or
link scope. Connected sockets also filter by peer. Membership ends
when a socket leaves or closes, and repeated joins are idempotent,
matching turmoil's existing API.

IPv4 membership accepts any address configured on the host, plus
implicit loopback addresses. `0.0.0.0` selects the socket's concrete
bind address, otherwise the first configured IPv4 address, falling
back to loopback when none is configured. Membership is tracked by
group and selected address: leaving one address preserves other
memberships, and multiple matching memberships still deliver one copy.

Loopback memberships receive only multicast sent over loopback.
Configured non-loopback addresses share the fabric's network link;
local copies of sends over that link go to its members. Joining a
group does not change the sending address. Bind a sender to a loopback
address to send multicast over loopback. Sends use the selected scope's
MTU, so loopback multicast can carry larger datagrams than the shared
link permits.

IPv6 index 0 uses the same address-selection rules for IPv6. Other
indexes remain unsupported because the kernel has no interface-index
registry. There is no subnet topology, so only `255.255.255.255` is
broadcast; other IPv4 addresses ending in `.255` remain unicast.

Rules see the original broadcast or multicast destination. Remote
delivery fans out after rule evaluation, so a drop or delay affects all
remote recipients. Local copies bypass rules, as unicast loopback does.

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
