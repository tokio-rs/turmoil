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
turmoil = "0.1.0"
```

Next, create a test file and add a test:

```rust
use turmoil::{Builder, Io};

#[derive(Debug)]
enum Message {
    Echo(String),
}

impl turmoil::Message for Message {
    fn write_json(&self, _dst: &mut dyn std::io::Write) {
        todo!("not yet implemented")
    }
}

#[test]
fn simulation() {
    let mut sim = Builder::new().build();

    // register a client
    let client = sim.client("client");

    // register a host
    sim.register("server", |host: Io<Message>| async move {
        loop {
            let (msg, src) = host.recv().await;
            host.send(src, msg);
        }
    });

    // run the simulation
    sim.run_until(async move {
        client.send("server", Message::Echo("hello, server!".to_string()));

        let (msg, _) = client.recv().await;
        let echo = match msg {
            Message::Echo(e) => e,
        };
        assert_eq!("hello, server!", echo);
    });
}

```

## License

This project is licensed under the [MIT license](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in `turmoil` by you, shall be licensed as MIT,
without any additional terms or conditions.
