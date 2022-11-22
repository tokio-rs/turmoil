#[cfg(doctest)]
mod readme;

mod builder;

use std::net::IpAddr;

pub use builder::Builder;

mod config;
use config::Config;

mod dns;
use dns::Dns;
pub use dns::{ToIpAddr, ToSocketAddrs};

mod envelope;
use envelope::Envelope;

mod error;
pub use error::Result;

mod host;
pub use host::elapsed;
use host::Host;

pub mod net;

mod role;
use role::Role;

mod rt;
use rt::Rt;

mod sim;
pub use sim::Sim;

mod top;
use top::Topology;

mod world;
use world::World;

const TRACING_TARGET: &str = "turmoil";

/// Lookup an ip address by host name.
///
/// Must be called from within a Turmoil simulation.
pub fn lookup(addr: impl ToIpAddr) -> IpAddr {
    World::current(|world| world.lookup(addr))
}

/// Hold messages two hosts, until [`release`] is called.
///
/// Must be called from within a Turmoil simulation.
pub fn hold(a: impl ToIpAddr, b: impl ToIpAddr) {
    World::current(|world| {
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.hold(a, b);
    })
}

/// The opposite of [`hold`]. All held messages are immediately delivered.
///
/// Must be called from within a Turmoil simulation.
pub fn release(a: impl ToIpAddr, b: impl ToIpAddr) {
    World::current(|world| {
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.release(a, b);
    })
}

/// Partition two hosts, resulting in all messages sent between them to be
/// dropped.
///
/// Must be called from within a Turmoil simulation.
pub fn partition(a: impl ToIpAddr, b: impl ToIpAddr) {
    World::current(|world| {
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.partition(a, b);
    })
}

/// Repair the connection between two hosts, resulting in messages to be
/// delivered.
///
/// Must be called from within a Turmoil simulation.
pub fn repair(a: impl ToIpAddr, b: impl ToIpAddr) {
    World::current(|world| {
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.repair(a, b);
    })
}
