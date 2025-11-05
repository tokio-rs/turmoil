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
//! a simulation. To enable traces, your tests must install a
//! [`tracing-subscriber`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/).
//! The log level of turmoil can be configured using `RUST_LOG=turmoil=info`.
//!
//! It is possible to configure your tracing subscriber to log elapsed
//! simulation time instead of real time. See the grpc example.
//!
//! Turmoil can provide a full packet level trace of the events happening in a
//! simulation by passing `RUST_LOG=turmoil=trace`. This is really useful
//! when you are unable to identify why some unexpected behaviour is happening
//! and you need to know which packets are reaching where.
//!
//! To see this in effect, you can run the axum example with the following
//! command:
//!
//! ```bash
//! RUST_LOG=INFO,turmoil=TRACE cargo run -p axum-example
//! ```
//!
//! You can see the TCP packets being sent and delivered between the server
//! and the client:
//!
//! ```bash
//! ...
//! 2023-11-29T20:23:43.276745Z TRACE node{name="server"}: turmoil: Send src=192.168.0.1:9999 dst=192.168.0.2:49152 protocol=TCP [0x48, 0x54, 0x54, 0x50, 0x2F, 0x31, 0x2E, 0x31, 0x20, 0x32, 0x30, 0x30, 0x20, 0x4F, 0x4B, 0xD, 0xA, 0x63, 0x6F, 0x6E, 0x74, 0x65, 0x6E, 0x74, 0x2D, 0x74, 0x79, 0x70, 0x65, 0x3A, 0x20, 0x74, 0x65, 0x78, 0x74, 0x2F, 0x70, 0x6C, 0x61, 0x69, 0x6E, 0x3B, 0x20, 0x63, 0x68, 0x61, 0x72, 0x73, 0x65, 0x74, 0x3D, 0x75, 0x74, 0x66, 0x2D, 0x38, 0xD, 0xA, 0x63, 0x6F, 0x6E, 0x74, 0x65, 0x6E, 0x74, 0x2D, 0x6C, 0x65, 0x6E, 0x67, 0x74, 0x68, 0x3A, 0x20, 0x31, 0x30, 0xD, 0xA, 0x64, 0x61, 0x74, 0x65, 0x3A, 0x20, 0x57, 0x65, 0x64, 0x2C, 0x20, 0x32, 0x39, 0x20, 0x4E, 0x6F, 0x76, 0x20, 0x32, 0x30, 0x32, 0x33, 0x20, 0x32, 0x30, 0x3A, 0x32, 0x33, 0x3A, 0x34, 0x33, 0x20, 0x47, 0x4D, 0x54, 0xD, 0xA, 0xD, 0xA, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x66, 0x6F, 0x6F, 0x21]
//! 2023-11-29T20:23:43.276834Z DEBUG turmoil::sim: step 43
//! 2023-11-29T20:23:43.276907Z DEBUG turmoil::sim: step 44
//! 2023-11-29T20:23:43.276981Z DEBUG turmoil::sim: step 45
//! 2023-11-29T20:23:43.277039Z TRACE node{name="client"}: turmoil: Delivered src=192.168.0.1:9999 dst=192.168.0.2:49152 protocol=TCP [0x48, 0x54, 0x54, 0x50, 0x2F, 0x31, 0x2E, 0x31, 0x20, 0x32, 0x30, 0x30, 0x20, 0x4F, 0x4B, 0xD, 0xA, 0x63, 0x6F, 0x6E, 0x74, 0x65, 0x6E, 0x74, 0x2D, 0x74, 0x79, 0x70, 0x65, 0x3A, 0x20, 0x74, 0x65, 0x78, 0x74, 0x2F, 0x70, 0x6C, 0x61, 0x69, 0x6E, 0x3B, 0x20, 0x63, 0x68, 0x61, 0x72, 0x73, 0x65, 0x74, 0x3D, 0x75, 0x74, 0x66, 0x2D, 0x38, 0xD, 0xA, 0x63, 0x6F, 0x6E, 0x74, 0x65, 0x6E, 0x74, 0x2D, 0x6C, 0x65, 0x6E, 0x67, 0x74, 0x68, 0x3A, 0x20, 0x31, 0x30, 0xD, 0xA, 0x64, 0x61, 0x74, 0x65, 0x3A, 0x20, 0x57, 0x65, 0x64, 0x2C, 0x20, 0x32, 0x39, 0x20, 0x4E, 0x6F, 0x76, 0x20, 0x32, 0x30, 0x32, 0x33, 0x20, 0x32, 0x30, 0x3A, 0x32, 0x33, 0x3A, 0x34, 0x33, 0x20, 0x47, 0x4D, 0x54, 0xD, 0xA, 0xD, 0xA, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x66, 0x6F, 0x6F, 0x21]
//! 2023-11-29T20:23:43.277097Z TRACE node{name="client"}: turmoil: Recv src=192.168.0.1:9999 dst=192.168.0.2:49152 protocol=TCP [0x48, 0x54, 0x54, 0x50, 0x2F, 0x31, 0x2E, 0x31, 0x20, 0x32, 0x30, 0x30, 0x20, 0x4F, 0x4B, 0xD, 0xA, 0x63, 0x6F, 0x6E, 0x74, 0x65, 0x6E, 0x74, 0x2D, 0x74, 0x79, 0x70, 0x65, 0x3A, 0x20, 0x74, 0x65, 0x78, 0x74, 0x2F, 0x70, 0x6C, 0x61, 0x69, 0x6E, 0x3B, 0x20, 0x63, 0x68, 0x61, 0x72, 0x73, 0x65, 0x74, 0x3D, 0x75, 0x74, 0x66, 0x2D, 0x38, 0xD, 0xA, 0x63, 0x6F, 0x6E, 0x74, 0x65, 0x6E, 0x74, 0x2D, 0x6C, 0x65, 0x6E, 0x67, 0x74, 0x68, 0x3A, 0x20, 0x31, 0x30, 0xD, 0xA, 0x64, 0x61, 0x74, 0x65, 0x3A, 0x20, 0x57, 0x65, 0x64, 0x2C, 0x20, 0x32, 0x39, 0x20, 0x4E, 0x6F, 0x76, 0x20, 0x32, 0x30, 0x32, 0x33, 0x20, 0x32, 0x30, 0x3A, 0x32, 0x33, 0x3A, 0x34, 0x33, 0x20, 0x47, 0x4D, 0x54, 0xD, 0xA, 0xD, 0xA, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x66, 0x6F, 0x6F, 0x21]
//! 2023-11-29T20:23:43.277324Z  INFO client: axum_example: Got response: Response { status: 200, version: HTTP/1.1, headers: {"content-type": "text/plain; charset=utf-8", "content-length": "10", "date": "Wed, 29 Nov 2023 20:23:43 GMT"}, body: b"Hello foo!" }
//! ...
//! ```
//!
//! Here the server is sending a response, before it is delivered to, and
//! received by the client. Note that there are three steps to each packet
//! trace in turmoil. We see `Send` when a packet is sent from one address
//! to another. The packet is then `Delivered` to its destination, and when
//! the destination reads the packet it is `Recv`'d.
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

use std::{net::IpAddr, time::Duration};

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
            // skip for the same host
            if first != second {
                f(*first, *second)
            }
        }
    }
}

/// Returns how long the currently executing host has been executing for in
/// virtual time.
///
/// Must be called from within a Turmoil simulation.
pub fn elapsed() -> Duration {
    World::current(|world| world.current_host().timer.elapsed())
}

/// Returns how long the simulation has been executing for in virtual time.
///
/// Will return None if the duration is not available, typically because
/// there is no currently executing host or world.
pub fn sim_elapsed() -> Option<Duration> {
    World::try_current(|world| {
        world
            .try_current_host()
            .map(|host| host.timer.sim_elapsed())
            .ok()
    })
    .ok()
    .flatten()
}

/// The logical duration from [`UNIX_EPOCH`] until now.
///
/// On creation the simulation picks a `SystemTime` and calculates the
/// duration since the epoch. Each `run()` invocation moves logical time
/// forward the configured tick duration.
///
/// Will return None if the duration is not available, typically because
/// there is no currently executing host or world.
pub fn since_epoch() -> Option<Duration> {
    World::try_current(|world| {
        world
            .try_current_host()
            .map(|host| host.timer.since_epoch())
            .ok()
    })
    .ok()
    .flatten()
}

/// Lookup an IP address by host name.
///
/// Must be called from within a Turmoil simulation.
pub fn lookup(addr: impl ToIpAddr) -> IpAddr {
    World::current(|world| world.lookup(addr))
}

/// Perform a reverse DNS lookup, returning the hostname if the entry exists.
///
/// Must be called from within a Turmoil simulation.
pub fn reverse_lookup(addr: IpAddr) -> Option<String> {
    World::current(|world| world.reverse_lookup(addr).map(|h| h.to_owned()))
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
    World::current(|world| world.hold_many(a, b))
}

/// The opposite of [`hold`]. All held messages are immediately delivered.
///
/// Must be called from within a Turmoil simulation.
pub fn release(a: impl ToIpAddrs, b: impl ToIpAddrs) {
    World::current(|world| world.release_many(a, b))
}

/// Partition two hosts, or sets of hosts, resulting in all messages sent
/// between them to be dropped.
///
/// Must be called from within a Turmoil simulation.
pub fn partition(a: impl ToIpAddrs, b: impl ToIpAddrs) {
    World::current(|world| world.partition_many(a, b))
}

/// Partition two hosts, or sets of hosts, in one direction.
///
/// Must be called from within a Turmoil simulation.
pub fn partition_oneway(from: impl ToIpAddrs, to: impl ToIpAddrs) {
    World::current(|world| world.partition_oneway_many(from, to))
}

/// Repair the connection between two hosts, or sets of hosts, resulting in
/// messages to be delivered.
///
/// Must be called from within a Turmoil simulation.
pub fn repair(a: impl ToIpAddrs, b: impl ToIpAddrs) {
    World::current(|world| world.repair_many(a, b))
}

/// Repair the connection between two hosts, or sets of hosts, in one direction.
///
/// Must be called from within a Turmoil simulation.
pub fn repair_oneway(from: impl ToIpAddrs, to: impl ToIpAddrs) {
    World::current(|world| world.repair_oneway_many(from, to))
}

/// Return the number of established tcp streams on the current host.
pub fn established_tcp_stream_count() -> usize {
    World::current(|world| world.est_tcp_streams())
}

/// Return the number of established tcp streams on the given host.
pub fn established_tcp_stream_count_on(addr: impl ToIpAddr) -> usize {
    World::current(|world| world.est_tcp_streams_on(addr))
}
