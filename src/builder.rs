use crate::{Config, Dns, Sim};

use rand::{RngCore, SeedableRng};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// Configure the simulation
pub struct Builder {
    rng: Option<Box<dyn RngCore>>,

    config: Config,

    /// How often any given link should fail (on a per-message basis).
    fail_rate: f64,

    /// How often any given link should be repaired (on a per-message basis);
    repair_rate: f64,
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            rng: None,
            config: Config::default(),
            fail_rate: 0.0,
            repair_rate: 0.0,
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

    pub fn fail_rate(&mut self, value: f64) -> &mut Self {
        self.fail_rate = value;
        self
    }

    pub fn repair_rate(&mut self, value: f64) -> &mut Self {
        self.repair_rate = value;
        self
    }

    pub fn build<T: 'static>(&self) -> Sim<T> {
        self.build_with_rng(Box::new(rand::rngs::SmallRng::from_entropy()))
    }

    pub fn build_with_rng<T: 'static>(&self, rng: Box<dyn RngCore>) -> Sim<T> {
        use crate::{Inner, Topology};

        let topology = Topology::new(self.fail_rate, self.repair_rate);

        Sim {
            inner: Rc::new(Inner {
                config: self.config.clone(),
                hosts: Default::default(),
                topology: RefCell::new(topology),
                rand: RefCell::new(rng),
            }),
            dns: Dns::new(),
        }
    }
}
