use crate::*;

use rand::{RngCore, SeedableRng};
use std::cell::RefCell;
use std::rc::Rc;

use std::time::Duration;

/// Configure the simulation
pub struct Builder {
    rng: Option<Box<dyn RngCore>>,

    config: Config,

    link_config: top::Latency,
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            rng: None,
            config: Config::default(),
            link_config: top::Latency::default(),
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
        self.link_config.min_message_latency = value;
        self
    }

    pub fn max_message_latency(&mut self, value: Duration) -> &mut Self {
        self.link_config.max_message_latency = value;
        self
    }

    pub fn fail_rate(&mut self, value: f64) -> &mut Self {
        self.link_config.fail_rate = value;
        self
    }

    pub fn repair_rate(&mut self, value: f64) -> &mut Self {
        self.link_config.repair_rate = value;
        self
    }

    pub fn build(&self) -> Sim {
        self.build_with_rng(Box::new(rand::rngs::SmallRng::from_entropy()))
    }

    pub fn build_with_rng(&self, rng: Box<dyn RngCore>) -> Sim {
        let topology = Topology::new(self.link_config.clone());

        Sim {
            inner: Rc::new(Inner {
                config: self.config.clone(),
                dns: Dns::new(),
                hosts: Default::default(),
                topology: RefCell::new(topology),
                rand: RefCell::new(rng),
            }),
        }
    }
}
