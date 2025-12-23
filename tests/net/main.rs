//! Tests for `turmoil::net`.

mod tcp;
mod udp;

use std::net::SocketAddr;

use turmoil::{Builder, Result};

#[test]
fn lookup_host() -> Result {
    let mut sim = Builder::new().build();
    sim.host("host", || async { Ok(()) });

    sim.client("client", async {
        let result = turmoil::net::lookup_host("host:1234").await;
        let addrs: Vec<_> = result.unwrap().collect();
        let expected = SocketAddr::from((turmoil::lookup("host"), 1234));
        assert_eq!(addrs, [expected]);

        let result = turmoil::net::lookup_host("bogus:1234").await;
        assert!(result.is_err());

        let result = turmoil::net::lookup_host("port_missing").await;
        assert!(result.is_err());

        Ok(())
    });

    sim.run()
}
