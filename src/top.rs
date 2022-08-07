use indexmap::IndexMap;
use rand::{Rng, RngCore};
use rand_distr::{Distribution, Exp};
use std::net::SocketAddr;
use std::time::Duration;

/// Describes the network topology
#[derive(Default)]
pub(crate) struct Topology {
    latency: Latency,

    /// Specific configuration overrides between specific hosts.
    links: IndexMap<Pair, Link>,
}

#[derive(Debug, Hash, Eq, PartialEq)]
struct Pair(SocketAddr, SocketAddr);

pub(crate) enum Link {
    /// The link was explicitly partitioned.
    ExplicitPartition,

    /// The link was randomly partitioned.
    RandPartition,
}

pub(crate) struct Latency {
    /// Minimum latency
    min: Duration,

    /// Maximum latency
    max: Duration,

    /// Value distribution
    distribution: Exp<f64>,

    /// Probability of a link failing
    fail_rate: f64,

    /// Probability of a failed link returning
    repair_rate: f64,
}

impl Topology {
    pub(crate) fn new(fail_rate: f64, repair_rate: f64) -> Topology {
        Topology {
            latency: Latency {
                fail_rate,
                repair_rate,
                ..Latency::default()
            },
            links: IndexMap::new(),
        }
    }

    pub(crate) fn send_delay(
        &mut self,
        rand: &mut dyn RngCore,
        src: SocketAddr,
        dst: SocketAddr,
        client: bool,
    ) -> Option<Duration> {
        let pair = Pair::new(src, dst);

        match self.links.get(&pair) {
            None => {
                // Should the link be broken?
                if !client && self.latency.fail_rate > 0.0 {
                    if rand.gen_bool(self.latency.fail_rate) {
                        self.links.insert(pair, Link::RandPartition);
                        return None;
                    }
                }

                Some(self.latency.send_delay(rand))
            }
            Some(Link::ExplicitPartition) => {
                None
            }
            Some(Link::RandPartition) => {
                // Should the link be repaired?
                if !client && self.latency.repair_rate > 0.0 {
                    if rand.gen_bool(self.latency.repair_rate) {
                        self.links.remove(&pair);
                        return Some(self.latency.send_delay(rand));
                    }
                }

                None
            }
        }
    }

    pub(crate) fn partition(&mut self, a: SocketAddr, b: SocketAddr) {
        let pair = Pair::new(a, b);

        self.links.insert(pair, Link::ExplicitPartition);
    }

    pub(crate) fn repair(&mut self, a: SocketAddr, b: SocketAddr) {
        let pair = Pair::new(a, b);
        self.links.remove(&pair);
    }
}

impl Pair {
    fn new(a: SocketAddr, b: SocketAddr) -> Pair {
        assert_ne!(a, b);

        if a < b {
            Pair(a, b)
        } else {
            Pair(b, a)
        }
    }
}

impl Default for Latency {
    fn default() -> Latency {
        Latency {
            min: Duration::from_millis(0),
            max: Duration::from_millis(100),
            distribution: Exp::new(5.0).unwrap(),
            fail_rate: 0.0,
            repair_rate: 1.0,
        }
    }
}

impl Latency {
    fn send_delay(&self, rand: &mut dyn RngCore) -> Duration {
        let mult = self.distribution.sample(rand);
        let range = (self.max - self.min).as_millis() as f64;
        let delay = self.min + Duration::from_millis((range * mult) as _);
        std::cmp::min(delay, self.max)
    }
}
