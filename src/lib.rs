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

mod software;
use software::Software;

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

/// Returns `true` if logging is enabled
pub fn log_enabled() -> bool {
    World::current(|world| world.log.enabled() && world.current.is_some())
}

#[macro_export]
macro_rules! info {
    ( $($t:tt)* ) => {{
        if $crate::log_enabled() {
            let line = format!( $($t)* );
            $crate::log(false, &line);
        }
    }};
}

#[macro_export]
macro_rules! debug {
    () => {
        if $crate::log_enabled() {
            let line = format!( $($t)* );
            $crate::log(true, &line);
        }
    };
}

#[doc(hidden)]
pub fn log(debug: bool, line: &str) {
    World::current(|world| {
        if let Some(current) = world.current {
            let host = world.host(current);
            let dot = host.dot();
            let elapsed = host.elapsed();

            world.log.line(&world.dns, dot, elapsed, debug, line);
        }
    })
}
