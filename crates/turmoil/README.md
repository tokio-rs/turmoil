# turmoil

Deterministic simulation testing for distributed systems in Rust.

Each simulated host has its own Tokio runtime. Turmoil steps hosts in turn
and controls simulated time, network latency, packet loss, partitions, and
crashes. Each host uses [`turmoil-net`](../turmoil-net) for TCP and UDP.
The `unstable-fs` feature adds
a simulated filesystem through [`turmoil-fs`](../turmoil-fs).

```toml
[dev-dependencies]
turmoil = "0.7"
```

Use `Builder` to configure a simulation, `Sim::host` to register services,
and `Sim::client` to run test code. Import sockets from `turmoil::net`.
See the [crate documentation](https://docs.rs/turmoil) and
[examples](../../examples) for usage.

## Barriers (unstable)

*Requires the `unstable-barriers` feature.*

Barriers let tests suspend and resume code at a `trigger(event)` call.
See the `barriers` module for details.
