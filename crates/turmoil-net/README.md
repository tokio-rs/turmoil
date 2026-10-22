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

## Interfaces and UDP broadcast/multicast

Each host has two explicit interfaces: `lo` (index 1) and `eth0` (index 2)
on the fabric's shared link. `Net::interfaces(host)` exposes their identities,
addresses, and MTUs. Loopback addresses are implicit; addresses passed to
`Net::add_host` are aliases on `eth0`, not separate interfaces. There are no
subnet prefixes, additional links, or configurable routing tables yet.

`UdpSocket` supports IPv4 broadcast (`set_broadcast`) and IPv4/IPv6
multicast (`join_multicast_*`, `leave_multicast_*`, and
`set_multicast_loop_*`). Membership is keyed by group and interface:
joining through another address on the same interface returns `AddrInUse`,
and leaving through either alias removes that membership. Joining through
an unknown interface returns Linux `ENODEV`.

An unspecified join interface selects the shared link when it has an address
in that family. Loopback-only fixtures have an implicit multicast route to
`lo`. This selection is independent of the socket's bind address. IPv6 joins
accept indices 1 and 2 as well as default index 0. IPv6 leave with index 0
removes the most recently joined interface for that group, matching Linux.

Bind addresses filter packet destinations. Wildcard binds and matching
multicast-group binds can receive multicast; an interface's unicast address
is not a multicast receive filter. Binding `255.255.255.255` filters for limited
broadcasts and preserves that address in `local_addr()`. Connected
sockets also filter by peer. Linux's default multicast-all behavior permits
a matching socket to receive groups joined elsewhere on the same host and
incoming interface. Closing the last member removes that host membership.
UDP receiver selection uses indexed destination/port lookups and host
membership counts maintained on join, leave, and close. It visits matching
receivers without scanning unrelated sockets or allocating a target list.

Multicast output uses the default route independently of source binding or
membership. An IPv6 destination scope ID can select the output interface.
On the shared link, the sending socket's multicast-loop flag controls local
copies; TTL 0 suppresses remote delivery. IPv4 output through `lo` still
arrives when the multicast-loop flag is disabled. Loopback source addresses
do not turn a shared-link route into a loopback route.

The kernel also supports multicast output-interface and multicast-all options,
and UDP reuse options before bind. The Tokio shim has no socket2/from-std
adapter yet, so it does not expose those additional socket configuration APIs.

Only `255.255.255.255` is currently classified as broadcast. Directed broadcast
requires subnet configuration; an arbitrary address ending in `.255` remains
unicast. UDP still rejects payloads exceeding the selected interface's MTU:
IP fragmentation/reassembly and selectable PMTU policy remain unimplemented.

Rules see the original broadcast or multicast destination. Remote delivery
fans out after rule evaluation, so a drop or delay affects all remote
recipients. Local copies bypass rules, as unicast loopback does.

The socket scenarios in `tests/udp_cast_conformance.rs` run against both the
simulator and real Tokio on Linux. To run the Linux side in a private network
namespace, use `python3 crates/turmoil-net/tests/linux/udp_cast.py` from the
workspace root (requires unprivileged user/network namespaces and iproute2).
The manual UDP delivery scaling benchmark runs with
`cargo test -p turmoil-net --release udp_delivery_scaling -- --ignored --nocapture`.
It measures receiver selection and queueing, excluding socket setup and
simulation scheduling; timing results are not used as a CI pass/fail check.

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
