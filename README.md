# Turmoil

**This is very experimental**

Add hardship to your tests.

Turmoil is a framework for testing distributed systems. It provides
deterministic execution by running multiple concurrent hosts within a single
thread. It introduces "hardship" into the system via changes in the simulated
network and filesystem. Both can be controlled manually or with a seeded rng.

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
turmoil = "0.7"
```

See crate documentation for simulation setup instructions.

### Examples

- [/tests](https://github.com/tokio-rs/turmoil/tree/main/tests) for TCP, UDP, and filesystem.
- [`gRPC`](https://github.com/tokio-rs/turmoil/tree/main/examples/grpc) using
    `tonic` and `hyper`.
- [`axum`](https://github.com/tokio-rs/turmoil/tree/main/examples/axum)

### Filesystem Simulation (unstable)

*Requires the `unstable-fs` feature.*

```toml
[dev-dependencies]
turmoil = { version = "0.7", features = ["unstable-fs"] }
```

Turmoil provides simulated filesystem types for crash-consistency testing:

```rust,ignore
use turmoil::fs::shim::std::fs::OpenOptions;
use turmoil::fs::shim::std::os::unix::fs::FileExt;

let file = OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .open("/data/db")?;

file.write_all_at(b"data", 0)?;
file.sync_all()?;  // Data now durable, survives sim.crash()
```

Each host has isolated filesystem state. Use `Builder::fs_sync_probability()` to
configure random sync behavior for testing crash recovery.


## License

This project is licensed under the [MIT license](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in `turmoil` by you, shall be licensed as MIT,
without any additional terms or conditions.
