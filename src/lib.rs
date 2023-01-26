#[cfg(doctest)]
mod readme;

mod builder;

use std::net::IpAddr;

pub use builder::Builder;

mod config;
use config::Config;

mod dns;
use dns::Dns;
pub use dns::{ToIpAddr, ToIpAddrs, ToSocketAddrs};

mod envelope;
use envelope::Envelope;
pub use envelope::{Datagram, Protocol, Segment};

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
pub use top::{LinkIter, LinksIter, SentRef};

mod world;
use world::World;

const TRACING_TARGET: &str = "turmoil";

/// Utility method for performing a function on all hosts in `a` against all
/// hosts in `b`.
pub(crate) fn for_pairs(a: &Vec<IpAddr>, b: &Vec<IpAddr>, mut f: impl FnMut(IpAddr, IpAddr)) {
    for first in a {
        for second in b {
            f(*first, *second)
        }
    }
}

/// Lookup an ip address by host name.
///
/// Must be called from within a Turmoil simulation.
pub fn lookup(addr: impl ToIpAddr) -> IpAddr {
    World::current(|world| world.lookup(addr))
}

/// Lookup an IP address by host name. Use regex to match a number of hosts.
///
/// Must be called from within a Turmoil simulation.
pub fn lookup_many(addr: impl ToIpAddrs) -> Vec<IpAddr> {
    World::current(|world| world.lookup_many(addr))
}

/// Hold messages between two hosts, or sets of hosts, until [`release`] is
/// called.
///
/// Must be called from within a Turmoil simulation.
pub fn hold(a: impl ToIpAddrs, b: impl ToIpAddrs) {
    World::current(|world| {
        let a = world.lookup_many(a);
        let b = world.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            world.hold(a, b);
        });
    })
}

/// The opposite of [`hold`]. All held messages are immediately delivered.
///
/// Must be called from within a Turmoil simulation.
pub fn release(a: impl ToIpAddrs, b: impl ToIpAddrs) {
    World::current(|world| {
        let a = world.lookup_many(a);
        let b = world.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            world.release(a, b);
        });
    })
}

/// Partition two hosts, or sets of hosts, resulting in all messages sent
/// between them to be dropped.
///
/// Must be called from within a Turmoil simulation.
pub fn partition(a: impl ToIpAddrs, b: impl ToIpAddrs) {
    World::current(|world| {
        let a = world.lookup_many(a);
        let b = world.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            world.partition(a, b);
        });
    })
}

/// Repair the connection between two hosts, or sets of hosts, resulting in
/// messages to be delivered.
///
/// Must be called from within a Turmoil simulation.
pub fn repair(a: impl ToIpAddrs, b: impl ToIpAddrs) {
    World::current(|world| {
        let a = world.lookup_many(a);
        let b = world.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            world.repair(a, b);
        });
    })
}
