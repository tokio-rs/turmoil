use crate::*;

use rand::{RngCore, SeedableRng};
use std::time::{Duration, SystemTime};

/// A builder that can be used to configure the simulation.
///
/// The Builder allows you to set a number of options when creating a turmoil
/// simulation, see the available methods for documentation of each of these
/// options.
///
/// ## Examples
///
/// You can use the builder to initialize a sim with default configuration:
///
/// ```
/// let sim = turmoil::Builder::new().build();
/// ```
///
/// If you want to vary factors of the simulation, you can use the
/// respective Builder methods:
///
/// ```
/// use std::time::{Duration, SystemTime};
///
/// let sim = turmoil::Builder::new()
///     .simulation_duration(Duration::from_secs(60))
///     .epoch(SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(946684800)).unwrap())
///     .fail_rate(0.05) // 5% failure rate
///     .build();
/// ```
///
/// If you create a builder with a set of options you can then repeatedly
/// call `build` to get a sim with the same settings:
///
/// ```
/// use std::time::Duration;
///
/// // Create a persistent builder
/// let mut builder = turmoil::Builder::new();
///
/// // Apply a chain of options to that builder
/// builder.simulation_duration(Duration::from_secs(45))
///     .fail_rate(0.05);
///
/// let sim_one = builder.build();
/// let sim_two = builder.build();
/// ````
///
/// ## Entropy and randomness
///
/// By default, the builder will use its own `rng` to determine the variation
/// in random factors that affect the simulation, like message latency, and
/// failure distributions. To make your tests deterministic, you can use your
/// own seeded `rng` provider when building the simulation through
/// `build_with_rng`.
///
/// For example:
///
/// ```
/// use rand::rngs::SmallRng;
/// use rand::SeedableRng;
///
/// let rng = SmallRng::seed_from_u64(0);
/// let sim = turmoil::Builder::new().build_with_rng(Box::new(rng));
/// ```
pub struct Builder {
    config: Config,

    ip_version: IpVersion,

    link: config::Link,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
            ip_version: IpVersion::default(),
            link: config::Link {
                latency: Some(config::Latency::default()),
                message_loss: Some(config::MessageLoss::default()),
            },
        }
    }

    /// When the simulation starts.
    pub fn epoch(&mut self, value: SystemTime) -> &mut Self {
        self.config.epoch = value;
        self
    }

    /// How long the test should run for in simulated time
    pub fn simulation_duration(&mut self, value: Duration) -> &mut Self {
        self.config.duration = value;
        self
    }

    /// How much simulated time should elapse each tick.
    pub fn tick_duration(&mut self, value: Duration) -> &mut Self {
        self.config.tick = value;
        self
    }

    /// Which kind of network should be simulated.
    pub fn ip_version(&mut self, value: IpVersion) -> &mut Self {
        self.ip_version = value;
        self
    }

    /// The minimum latency that a message will take to transfer over a
    /// link on the network.
    pub fn min_message_latency(&mut self, value: Duration) -> &mut Self {
        self.link.latency_mut().min_message_latency = value;
        self
    }

    /// The maximum latency that a message will take to transfer over a
    /// link on the network.
    pub fn max_message_latency(&mut self, value: Duration) -> &mut Self {
        self.link.latency_mut().max_message_latency = value;
        self
    }

    /// The failure rate of messages on the network. For TCP
    /// this will break connections as currently there are
    /// no re-send capabilities. With UDP this is useful for
    /// testing network flakiness.
    pub fn fail_rate(&mut self, value: f64) -> &mut Self {
        self.link.message_loss_mut().fail_rate = value;
        self
    }

    /// The repair rate of messages on the network. This is how
    /// frequently a link is repaired after breaking.
    pub fn repair_rate(&mut self, value: f64) -> &mut Self {
        self.link.message_loss_mut().repair_rate = value;
        self
    }

    /// Capacity of a host's TCP buffer in the sim.
    pub fn tcp_capacity(&mut self, value: usize) -> &mut Self {
        self.config.tcp_capacity = value;
        self
    }

    /// Capacity of host's UDP buffer in the sim.
    pub fn udp_capacity(&mut self, value: usize) -> &mut Self {
        self.config.udp_capacity = value;
        self
    }

    /// Build a simulation with the settings from the builder.
    ///
    /// This will use default rng with entropy from the device running.
    pub fn build<'a>(&self) -> Sim<'a> {
        self.build_with_rng(Box::new(rand::rngs::SmallRng::from_entropy()))
    }

    /// Build a sim with a provided `rng`.
    ///
    /// This allows setting the random number generator used to fuzz
    pub fn build_with_rng<'a>(&self, rng: Box<dyn RngCore>) -> Sim<'a> {
        if self.link.latency().max_message_latency < self.link.latency().min_message_latency {
            panic!("Maximum message latency must be greater than minimum.");
        }

        let world = World::new(self.link.clone(), rng, self.ip_version.iter());
        Sim::new(self.config.clone(), world)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::Builder;

    #[test]
    #[should_panic]
    fn invalid_latency() {
        let _sim = Builder::new()
            .min_message_latency(Duration::from_millis(100))
            .max_message_latency(Duration::from_millis(50))
            .build();
    }
}
