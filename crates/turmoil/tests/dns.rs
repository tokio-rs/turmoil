use std::net::Ipv4Addr;
use turmoil::*;

// This test is not in src/dns.rs since it initalizes a tracing subscriber
// which will persist, and effect other tests in the same file.
#[test]
fn tracing_subscriber_does_not_crash_direct_binds() -> Result {
    tracing_subscriber::fmt().init();
    let mut sim = Builder::new().build();
    sim.client(Ipv4Addr::new(192, 168, 42, 42), async move { Ok(()) });
    sim.run()
}
