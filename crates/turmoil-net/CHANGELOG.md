# 0.1.0 (May 4, 2026)

Initial release. `turmoil-net` is a deterministic simulated socket
stack: production code imports `tokio::net`, tests swap the import for
`turmoil_net::shim::tokio::net` and run against the simulated fabric.

### Added

- `shim::tokio::net` — drop-in replacements for `TcpStream`,
  `TcpListener`, `UdpSocket`, and `ToSocketAddrs`, matching `tokio::net`
  type-for-type and error-kind-for-error-kind.
- `Net` / `EnterGuard` — primitives for building custom test
  harnesses: `add_host`, `enter`, and the `egress_all` / `evaluate` /
  `deliver` trio for driving the fabric.
- `fixture::lo` and `fixture::ClientServer` — batteries-included
  fixtures for single-host and N-server/1-client topologies.
- `Rule` / `Verdict` / `rule()` — packet-level fault injection with
  `Pass` / `Deliver(delay)` / `Drop` verdicts. `Latency::fixed` ships
  as a built-in rule.
- `KernelConfig` — builder for per-host MTU, buffer caps, backlog, and
  retransmit thresholds.
- `Netstat` — Linux-style socket snapshot for inspecting connection
  state during a test failure.
- TCP with retransmit: count-based go-back-N for data segments and SYN
  handshake retries, surfacing `TimedOut` on exhaustion.
- UDP with MTU-bounded datagrams (`EMSGSIZE` on oversize).
