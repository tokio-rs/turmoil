use crate::*;

use rand::{RngCore, SeedableRng};
use std::time::{Duration, SystemTime};

/// Configure the simulation
pub struct Builder {
    rng: Option<Box<dyn RngCore>>,

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
            rng: None,
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

    /// Set the random number generator used to fuzz
    pub fn rng(&mut self, rng: impl RngCore + 'static) -> &mut Self {
        self.rng = Some(Box::new(rng));
        self
    }

    pub fn min_message_latency(&mut self, value: Duration) -> &mut Self {
        self.link
            .latency
            .as_mut()
            .expect("`Latency` missing")
            .min_message_latency = value;
        // If the min latency is greater than the max latency, update 
        // the max latency.
        if self.link
            .latency
            .as_mut()
            .expect("`Latency` missing")
            .max_message_latency < value {
                self.link
                    .latency
                    .as_mut()
                    .expect("`Latency` missing")
                    .max_message_latency = value.clone()
            }
        self
    }

    pub fn max_message_latency(&mut self, value: Duration) -> &mut Self {
        if self.link
            .latency
            .as_mut()
            .expect("`Latency` missing")
            .min_message_latency > value {
                panic!("Max message latency must be greater than minimum.")
            };
        self.link
            .latency
            .as_mut()
            .expect("`Latency` missing")
            .max_message_latency = value;
        self
    }

    pub fn fail_rate(&mut self, value: f64) -> &mut Self {
        self.link.message_loss_mut().fail_rate = value;
        self
    }

    pub fn repair_rate(&mut self, value: f64) -> &mut Self {
        self.link.message_loss_mut().repair_rate = value;
        self
    }

    pub fn tcp_capacity(&mut self, value: usize) -> &mut Self {
        self.config.tcp_capacity = value;
        self
    }

    pub fn udp_capacity(&mut self, value: usize) -> &mut Self {
        self.config.udp_capacity = value;
        self
    }

    pub fn build<'a>(&self) -> Sim<'a> {
        self.build_with_rng(Box::new(rand::rngs::SmallRng::from_entropy()))
    }

    pub fn build_with_rng<'a>(&self, rng: Box<dyn RngCore>) -> Sim<'a> {
        let world = World::new(self.link.clone(), rng, self.ip_version.iter());
        Sim::new(self.config.clone(), world)
    }
}
