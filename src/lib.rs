//! Turmoil is a framework for testing distributed systems. It provides
//! deterministic execution by running multiple concurrent hosts within a single
//! thread. It introduces "hardship" into the system via changes in the
//! simulated network. The network can be controlled manually or with a seeded
//! rng.
//!
//! # Hosts and Software
//!
//! A turmoil simulation is comprised of one or more hosts. Hosts run software,
//! which is represented as a `Future`.
//!
//! Test code is also executed on a special host, called a client. This allows
//! for an entry point into the simulated system. Client hosts have all the same
//! capabilities as normal hosts, such as networking support.
//!
//! ```
//! use turmoil;
//!
//! let mut sim = turmoil::Builder::new().build();
//!
//! // register a host
//! sim.host("host", || async {
//!
//!     // host software goes here
//!
//!     Ok(())
//! });
//!
//! // define test code
//! sim.client("test", async {
//!
//!     // we can interact with other hosts from here
//!
//!     Ok(())
//! });
//!
//! // run the simulation and handle the result
//! _ = sim.run();
//!
//! ```
//!
//! # Networking
//!
//! Simulated networking types that mirror `tokio::net` are included in the
//! `turmoil::net` module.
//!
//! Turmoil is not yet oppinionated on how to structure your application code to
//! swap in simulated types under test. More on this coming soon...
//!
//! # Network Manipulation
//!
//! The simulation has the following network manipulation capabilities:
//!
//! * [`partition`], which introduces a network partition between hosts
//! * [`repair`], which repairs a network partition between hosts
//! * [`hold`], which holds all "in flight" messages between hosts. Messages are
//!   available for introspection using [`Sim`]'s `links` method.
//! * [`release`], which releases all "in flight" messages between hosts
//!
//! # Tracing
//!
//! The `tracing` crate is used to emit important events during the lifetime of
//! a simulation.
//!
//! This can be configured using `RUST_LOG=turmoil=info`.
//!
//! # Feature flags
//!
//! * `regex`: Enables regex host resolution through `ToIpAddrs`
//!
//! ## tokio_unstable
//!
//! Turmoil uses [unhandled_panic] to forward host panics as test failures. See
//! [unstable features] to opt in.
//!
//! [unhandled_panic]:
//!     https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html#method.unhandled_panic
//! [unstable features]: https://docs.rs/tokio/latest/tokio/#unstable-features

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

mod ip;
pub use ip::IpVersion;

pub mod net;

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

/// Lookup an IP address by host name.
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
