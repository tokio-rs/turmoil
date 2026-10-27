# turmoil-net

Deterministic TCP and UDP simulation for async Rust tests.

Use `turmoil_net::shim::tokio::net` in tests that need simulated sockets
with Tokio-compatible APIs:

```rust
#[cfg(not(test))]
use tokio::net::TcpStream;
#[cfg(test)]
use turmoil_net::shim::tokio::net::TcpStream;
```

The stack models TCP handshakes, retransmission, flow control, and connection
teardown, plus UDP unicast, broadcast, and multicast. Tests control packet
delivery, loss, and latency.

## Why

Bugs caused by packet loss, slow peers, and broken connections can be hard
to reproduce with real sockets.

`turmoil-net` lets your components communicate through a simulated protocol
stack while tests control packet delivery. You can exercise retries,
backpressure, and recovery, then repeat the same network conditions when
debugging a failure.

## Usage

Use `fixture::lo` for a single host:

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

Use `fixture::ClientServer` for multiple hosts. It runs until the client
finishes, then stops the servers:

```rust
use turmoil_net::fixture::ClientServer;
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn tcp_across_hosts() {
    ClientServer::new()
        .server("server", async {
            let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"hi").await.unwrap();
        })
        .run("client", async {
            let mut stream = TcpStream::connect("server:9000").await.unwrap();
            let mut buf = [0; 2];
            stream.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hi");
        });
}
```

Registering a hostname assigns it an IP in `192.168.0.0/16`. Socket lookups
resolve registered names; they do not allocate addresses. Use literal IPs
when a test needs specific addresses.

## Custom harnesses

`Net` manages hosts and synchronous packet routing. The harness owns time,
scheduling, and delayed delivery:

- `Net::enter` installs the network while host code runs;
  `EnterGuard::exit` removes it while preserving state.
- `Net::egress` drains one host's outbound packets.
- `Net::evaluate` applies packet rules.
- `Net::receivers` and `Net::deliver_to` allow faults per receiver.
- `Net::reset_host` discards a host's sockets without sending FIN or RST.
- `Net::set_resolver` supplies custom hostname resolution.

## Fault injection

Harnesses evaluate rules for outbound packets in installation order.
The first verdict other than `Pass` wins:

- `Pass`: continue to the next rule.
- `Deliver(Duration)`: deliver after the given delay.
- `Drop`: discard the packet.

If every rule passes, `evaluate` returns `Pass`. Fixtures deliver immediately;
Turmoil applies its configured link behavior.

`Net::rule` installs a rule for the network's lifetime. `EnterGuard::rule`
and the free `rule` function return a guard that removes the rule on drop.
Closures implementing `FnMut(&Packet) -> Verdict` can be used directly:

```rust
use std::net::IpAddr;
use turmoil_net::{Net, Packet, Verdict};

let mut net = Net::new();
let target: IpAddr = "192.168.0.2".parse().unwrap();
net.rule(move |pkt: &Packet| {
    if pkt.dst == target {
        Verdict::Drop
    } else {
        Verdict::Pass
    }
});
```

Rules run before broadcast and multicast packets are copied to remote hosts.
Local copies and unicast loopback bypass rules.

## Interfaces and UDP

Each host has `lo` (index 1) and `eth0` (index 2) on a shared link.
`Net::interfaces` exposes their addresses and MTUs. Loopback addresses are
implicit; addresses passed to `Net::add_host` are aliases on `eth0`.

UDP supports IPv4 limited broadcast (`255.255.255.255`) and IPv4/IPv6
multicast. Membership is per group and interface. Wildcard and matching
group binds receive multicast; a unicast bind does not. Connected sockets
also filter by peer.

The default multicast route selects the shared link when it has an address
in that family, or `lo` in loopback-only fixtures. Source binding and group
membership do not select the output interface. An IPv6 destination scope ID
can select it explicitly.

## Limitations

- The shim has no `socket2` or `from_std` adapter. Multicast output-interface,
  multicast-all, and pre-bind reuse options are not exposed.
- There are no subnet prefixes, additional links, or configurable routing
  tables. Directed broadcast is not supported.
- UDP datagrams larger than the selected interface MTU are rejected with
  `EMSGSIZE`; fragmentation is not yet supported.
- Jumbograms, checksums, ICMP, and path-MTU discovery are not modeled.
