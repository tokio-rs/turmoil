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

[crates-badge]: https://img.shields.io/crates/v/turmoil.svg
[crates-url]: https://crates.io/crates/turmoil
[docs-badge]: https://docs.rs/turmoil/badge.svg
[docs-url]: https://docs.rs/turmoil
[actions-badge]: https://github.com/tokio-rs/turmoil/actions/workflows/rust.yml/badge.svg?branch=main
[actions-url]: https://github.com/tokio-rs/turmoil/actions?query=workflow%3ACI+branch%3Amain

## Quickstart

Add this to your `Cargo.toml`.

```toml
[dev-dependencies]
turmoil = "0.2"
```

Next, create a test file and add a test:

```rust
use std::net::{IpAddr, Ipv4Addr};

use turmoil::{lookup, net, Builder};

let port = 1234;
let unspecified = IpAddr::from(Ipv4Addr::UNSPECIFIED);

let mut sim = Builder::new().build();

// register a host
sim.host("server", || async move {
    let sock = net::UdpSocket::bind((unspecified, port)).await.unwrap();

    loop {
        let mut buf = vec![0; 12];
        let (_, origin) = sock.recv_from(&mut buf).await.unwrap();

        let _ = sock.send_to(&buf, origin).await;
    }
});

// register a client (this is the test code)
sim.client("client", async move {
    let sock = net::UdpSocket::bind((unspecified, port)).await?;

    let server_addr = lookup("server");
    sock.send_to(b"hello, world", (server_addr, port)).await?;

    let mut buf = vec![0; 12];
    let _ = sock.recv_from(&mut buf).await?;
    assert_eq!(b"hello, world", &buf[..]);

    Ok(())
});

// run the simulation
sim.run();
```

For more examples, check out the [tests](tests) directory.

## License

This project is licensed under the [MIT license](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in `turmoil` by you, shall be licensed as MIT,
without any additional terms or conditions.
