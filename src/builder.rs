use std::{ops::RangeInclusive, time::SystemTime};

use rand::{rngs::SmallRng, SeedableRng};

use crate::*;

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
/// Turmoil uses a random number generator to determine the variation in random
/// factors that affect the simulation, like message latency, and failure
/// distributions. By default, this rng is randomly seeded. To make your tests
/// deterministic, you can provide your own seed through [`Builder::rng_seed`].
pub struct Builder {
    config: Config,
    ip_version: IpVersion,
    link: config::Link,
    rng_seed: Option<u64>,
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
            rng_seed: None,
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

    /// The Dynamic Ports, also known as the Private or Ephemeral Ports.
    /// See: <https://www.rfc-editor.org/rfc/rfc6335#section-6>
    pub fn ephemeral_ports(&mut self, value: RangeInclusive<u16>) -> &mut Self {
        self.config.ephemeral_ports = value;
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

    /// Enables the tokio I/O driver.
    pub fn enable_tokio_io(&mut self) -> &mut Self {
        self.config.enable_tokio_io = true;
        self
    }

    /// Enables running of nodes in random order. This allows exploration
    /// of extra state space in multi-node simulations where race conditions
    /// may arise based on message send/receive order.
    pub fn enable_random_order(&mut self) -> &mut Self {
        self.config.random_node_order = true;
        self
    }

    /// Seed for random numbers generated by turmoil.
    pub fn rng_seed(&mut self, value: u64) -> &mut Self {
        self.rng_seed = Some(value);
        self
    }

    /// Build a simulation with the settings from the builder.
    pub fn build<'a>(&self) -> Sim<'a> {
        if self.link.latency().max_message_latency < self.link.latency().min_message_latency {
            panic!("Maximum message latency must be greater than minimum.");
        }

        let rng = match self.rng_seed {
            Some(seed) => SmallRng::seed_from_u64(seed),
            None => SmallRng::from_os_rng(),
        };

        let world = World::new(
            self.link.clone(),
            Box::new(rng),
            self.ip_version.iter(),
            self.config.tick,
        );

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
