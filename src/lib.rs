mod builder;

pub use builder::Builder;

mod config;
use config::Config;

mod dns;
use dns::Dns;
pub use dns::ToSocketAddr;

mod envelope;
use envelope::Envelope;

mod host;
use host::Host;

mod io;
pub use io::Io;

mod log;
use log::Log;

mod message;
pub use message::Message;

mod rt;
use rt::Rt;

mod sim;
pub use sim::Sim;

mod top;
use top::Topology;

mod version;

mod world;
use world::World;

/// Partition two hosts, resulting in all messages sent between them to be
/// dropped.
///
/// Must be called from within a Turmoil simulation.
pub fn partition(a: impl ToSocketAddr, b: impl ToSocketAddr) {
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
pub fn repair(a: impl ToSocketAddr, b: impl ToSocketAddr) {
    World::current(|world| {
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.repair(a, b);
    })
}
