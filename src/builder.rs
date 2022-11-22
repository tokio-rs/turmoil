use crate::*;

use rand::{RngCore, SeedableRng};
use std::time::Duration;

/// Configure the simulation
pub struct Builder {
    rng: Option<Box<dyn RngCore>>,

    config: Config,

    link: config::Link,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            rng: None,
            config: Config::default(),
            link: config::Link {
                latency: Some(config::Latency::default()),
                message_loss: Some(config::MessageLoss::default()),
            },
        }
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
        self
    }

    pub fn max_message_latency(&mut self, value: Duration) -> &mut Self {
        self.link
            .latency
            .as_mut()
            .expect("`MessageLoss` missing")
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

    pub fn build<'a>(&self) -> Sim<'a> {
        self.build_with_rng(Box::new(rand::rngs::SmallRng::from_entropy()))
    }

    pub fn build_with_rng<'a>(&self, rng: Box<dyn RngCore>) -> Sim<'a> {
        let world = World::new(self.link.clone(), rng);
        Sim::new(self.config.clone(), world)
    }
}
