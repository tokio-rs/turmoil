# Turmoil

**This is very experimental**

Add hardship to your tests.

Turmoil is a framework for developing and testing distributed systems. It
provides deterministic execution by running multiple concurrent hosts within
a single thread. It introduces "hardship" into the system via changes in the
simulated network. The network can be controlled manually or with a seeded rng.

[![Crates.io][crates-badge]][crates-url]
[![Documentation][docs-badge]][docs-url]
[![Build Status][actions-badge]][actions-url]
[![Discord chat][discord-badge]][discord-url]

[crates-badge]: https://img.shields.io/crates/v/turmoil.svg
[crates-url]: https://crates.io/crates/turmoil
[docs-badge]: https://docs.rs/turmoil/badge.svg
[docs-url]: https://docs.rs/turmoil
[actions-badge]: https://github.com/tokio-rs/turmoil/actions/workflows/rust.yml/badge.svg?branch=main
[actions-url]: https://github.com/tokio-rs/turmoil/actions?query=workflow%3ACI+branch%3Amain
[discord-badge]: https://img.shields.io/discord/500028886025895936.svg?logo=discord&style=flat-square
[discord-url]: https://discord.com/channels/500028886025895936/628283075398467594

## Quickstart

Add this to your `Cargo.toml`.

```toml
[dev-dependencies]
turmoil = "0.3"
```

Next, create a test file and add a test:

```rust
let mut sim = turmoil::Builder::new().build();

// register a host
sim.host("server", || async move {
    // host software goes here

    Ok(())
});

// register a client
sim.client("client", async move {
    // dns lookup for "server"
    let addr = turmoil::lookup("server");

    // test code goes here

    Ok(())
});

// run the simulation
sim.run();
```

See `ping_pong` in [udp.rs](tests/udp.rs) for a networking example. For more
examples, check out the [tests](tests) directory.

### tokio_unstable

Turmoil uses [unhandled_panic] to foward host panics as test failures. See
[unstable features] to opt in.

[unhandled_panic]: https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html#method.unhandled_panic
[unstable features]: https://docs.rs/tokio/latest/tokio/#unstable-features

## License

This project is licensed under the [MIT license](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in `turmoil` by you, shall be licensed as MIT,
without any additional terms or conditions.
