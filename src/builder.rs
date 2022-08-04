use crate::{Dns, Sim};

use rand::{RngCore, SeedableRng};
use std::cell::RefCell;
use std::rc::Rc;

/// Configure the simulation
pub struct Builder {
    rng: Option<Box<dyn RngCore>>,

    /// How often any given link should fail (on a per-message basis).
    fail_rate: f64,

    /// How often any given link should be repaired (on a per-message basis);
    repair_rate: f64,
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            rng: None,
            fail_rate: 0.0,
            repair_rate: 0.0,
        }
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

    pub fn build<T: 'static>(&mut self) -> Sim<T> {
        use crate::{Inner, Topology};

        let topology = Topology::new(self.fail_rate, self.repair_rate);

        let rand = self
            .rng
            .take()
            .unwrap_or_else(|| Box::new(rand::rngs::SmallRng::from_entropy()));

        Sim {
            inner: Rc::new(Inner {
                hosts: Default::default(),
                topology: RefCell::new(topology),
                rand: RefCell::new(rand),
            }),
            dns: Dns::new(),
        }
    }
}
