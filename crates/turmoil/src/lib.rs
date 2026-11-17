//! Deterministic simulation testing for distributed systems in Rust.
//!
//! Each simulated host has its own Tokio runtime. Turmoil steps hosts in turn
//! and controls simulated time, network latency, packet loss, partitions, and
//! crashes. Tests configure network faults directly or use a seeded RNG.
//!
//! # Hosts and Software
//!
//! Each host runs a future. Client hosts run test code with the same networking
//! capabilities as service hosts. The simulation completes when all clients
//! finish.
//!
//! ```
//! let mut sim = turmoil::Builder::new().build();
//!
//! sim.host("host", || async {
//!     // Run the service.
//!     Ok(())
//! });
//!
//! sim.client("test", async {
//!     // Interact with the service and check its responses.
//!     Ok(())
//! });
//!
//! sim.run().unwrap();
//! ```
//!
//! # Networking
//!
//! [`net`] re-exports [`turmoil_net`]'s TCP and UDP sockets with Tokio-compatible
//! APIs. Import them in place of `tokio::net` when running under simulation.
//! Each host has its own stack; Turmoil schedules packets between hosts.
//! TCP models handshakes, retransmission, flow control, and connection teardown.
//! `connect` completes after the handshake, without waiting for `accept`.
//!
//! # Network Manipulation
//!
//! * [`partition`] drops packets between hosts; [`repair`] restores delivery.
//! * [`hold`] queues packets between hosts; [`release`] delivers held packets.
//! * [`Sim::links`] inspects packets in flight.
//!
//! TCP retransmits dropped segments and reports `TimedOut` when retries are
//! exhausted. [`Sim::crash`] discards a host's sockets without sending FIN or
//! RST. Peers detect the failure through unanswered retries or a reset after
//! the host restarts.
//!
//! Use [`turmoil_net::rule`] from host code for packet drops and delays.
//! Rules run as outbound packets are drained after each host tick.
//!
//! # Filesystem (unstable)
//!
//! *Requires the `unstable-fs` feature.*
//!
//! Simulated filesystem types that mirror `std::fs` and `std::os::unix::fs` are
//! included in the `fs::shim` module. This enables crash-consistency testing
//! for storage systems.
//!
//! ```ignore
//! use turmoil::fs::shim::std::fs::OpenOptions;
//! use std::os::unix::fs::FileExt;  // Real trait, works with our File
//!
//! # fn example() -> std::io::Result<()> {
//! let file = OpenOptions::new()
//!     .read(true)
//!     .write(true)
//!     .create(true)
//!     .open("/data/db")?;
//!
//! file.write_all_at(b"data", 0)?;
//! file.sync_all()?;  // Data now durable, survives crash
//! # Ok(())
//! # }
//! ```
//!
//! Each host has its own filesystem. Writes remain pending until synced
//! explicitly or by simulated background writeback. [`Sim::crash`] discards
//! pending writes; synced data survives [`Sim::bounce`].
//!
//! # Barriers (unstable)
//!
//! *Requires the `unstable-barriers` feature.*
//!
//! Barriers let tests suspend and resume code at a `trigger(event)` call.
//!
//! ```ignore
//! use turmoil::barriers::{Barrier, Reaction, trigger};
//!
//! // In source code (conditionally compiled)
//! trigger(MyEvent::PrepareAckReceived(tx_id)).await;
//!
//! // In test code
//! let mut barrier = Barrier::build(Reaction::Suspend, |e: &MyEvent| {
//!     matches!(e, MyEvent::PrepareAckReceived(_))
//! });
//!
//! // Run simulation until barrier triggers
//! let triggered = barrier.wait().await.unwrap();
//! // Source code is now suspended
//!
//! // Resume by dropping the handle
//! drop(triggered);
//! ```
//!
//! # Tracing
//!
//! Install a
//! [`tracing-subscriber`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/).
//! With an environment filter, `RUST_LOG=turmoil=trace` enables packet traces:
//!
//! ```bash
//! RUST_LOG=INFO,turmoil=TRACE cargo run -p axum-example
//! ```
//!
//! Packet events include `Send`, `Delivered`, `Hold`, and `Drop`.
//! See the grpc example for a subscriber that logs simulated elapsed time.
//!
//! # Feature flags
//!
//! * `regex`: Enables regex host resolution through `ToIpAddrs`
//! * `unstable-fs`: Simulated filesystem
//! * `unstable-io_uring`: Simulated io_uring with filesystem support
//! * `unstable-barriers`: Execution hooks for tests
//!
//! ## tokio_unstable
//!
//! Turmoil uses [unhandled_panic] to forward host panics as test failures. See
//! [unstable features] to opt in.
//!
//! [unhandled_panic]:
//!     https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html#method.unhandled_panic
//! [unstable features]: https://docs.rs/tokio/latest/tokio/#unstable-features

mod builder;

use std::{net::IpAddr, time::Duration};

pub use builder::Builder;

mod config;
use config::Config;

mod dns;
use dns::Dns;
pub use dns::{ToIpAddr, ToIpAddrs};
pub use net::ToSocketAddrs;
pub use turmoil_net::{IpFragment, Packet, TcpFlags, TcpSegment, Transport, UdpDatagram};

mod error;
pub use error::Result;

mod host;
use host::Host;

mod ip;
pub use ip::IpVersion;

#[cfg(feature = "unstable-fs")]
pub use turmoil_fs as fs;
#[cfg(feature = "unstable-fs")]
pub use turmoil_fs::FsConfig;

#[cfg(feature = "unstable-io_uring")]
pub use turmoil_io_uring as io_uring;

/// Hook fired by `turmoil-fs` on every silent-corruption event when
/// `unstable-barriers` is on. Installed on `FsState::on_corruption`
/// at host construction time.
#[cfg(all(feature = "unstable-fs", feature = "unstable-barriers"))]
fn fs_corruption_hook(event: &turmoil_fs::FsCorruption) {
    crate::barriers::trigger_noop(event.clone());
}

#[cfg(feature = "unstable-barriers")]
pub mod barriers;

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

/// Returns whether the caller is in a simulation context.
pub fn in_simulation() -> bool {
    World::try_current(|_| ()).is_ok()
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

/// The logical duration from [`UNIX_EPOCH`](std::time::UNIX_EPOCH) until now.
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
