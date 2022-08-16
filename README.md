# Turmoil

**This is very experimental**

Add hardship to your tests.

Turmoil is a framework for developing and testing distributed systems. It
provides deterministic execution by running multiple concurrent hosts within
a single thread. It introduces "hardship" into the system via changes in the
simulated network. The network can be controlled manually or with a seeded rng.

[![Crates.io](https://img.shields.io/crates/v/turmoil.svg)](https://crates.io/crates/turmoil)
[![Documentation](https://docs.rs/turmoil/badge.svg)][docs]

[docs]: https://docs.rs/turmoil

## Quickstart

Add this to your `Cargo.toml`.

```toml
[dev-dependencies]
turmoil = "0.2"
```

Next, create a test file and add a test:

```rust
use turmoil::{Builder, Io};

#[derive(Debug)]
struct Echo(String);

impl turmoil::Message for Echo {
    fn write_json(&self, _dst: &mut dyn std::io::Write) {
        unimplemented!()
    }
}

#[test]
fn simulation() {
    let mut sim = Builder::new().build();

    // register a host
    sim.register("server", |host: Io<Echo>| async move {
        loop {
            let (msg, src) = host.recv().await;
            host.send(src, msg);
        }
    });

    // register a client (this is the test code)
    sim.client("client", |host: Io<Echo>| async move {
        host.send("server", Echo("hello, server!".to_string()));

        let (echo, _) = host.recv().await;
        assert_eq!("hello, server!", echo.0);
    });

    // run the simulation
    sim.run();
}
```

## License

This project is licensed under the [MIT license](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in `turmoil` by you, shall be licensed as MIT,
without any additional terms or conditions.
