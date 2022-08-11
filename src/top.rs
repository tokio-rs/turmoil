use crate::config;

use indexmap::IndexMap;
use rand::{Rng, RngCore};
use rand_distr::{Distribution, Exp};
use std::net::SocketAddr;
use std::time::Duration;

/// Describes the network topology
#[derive(Default)]
pub(crate) struct Topology {
    config: config::Link,

    /// Specific configuration overrides between specific hosts.
    links: IndexMap<Pair, Link>,

    /// Current number of partitioned links
    num_partitioned: usize,
}

#[derive(Debug, Hash, Eq, PartialEq)]
struct Pair(SocketAddr, SocketAddr);

pub(crate) enum Link {
    /// The link is healthy
    Healthy,

    /// The link was explicitly partitioned.
    ExplicitPartition,

    /// The link was randomly partitioned.
    RandPartition,
}

impl Topology {
    pub(crate) fn new(config: config::Link) -> Topology {
        Topology {
            config,
            links: IndexMap::new(),
            num_partitioned: 0,
        }
    }

    /// Register a link between two hosts
    pub(crate) fn register(&mut self, a: SocketAddr, b: SocketAddr) {
        let pair = Pair::new(a, b);
        assert!(self.links.insert(pair, Link::Healthy).is_none());
    }

    pub(crate) fn set_max_message_latency(&mut self, value: Duration) {
        self.config.max_message_latency = value;
    }

    pub(crate) fn set_message_latency_curve(&mut self, value: f64) {
        self.config.distribution = Exp::new(value).unwrap();
    }

    pub(crate) fn send_delay(
        &mut self,
        rand: &mut dyn RngCore,
        src: SocketAddr,
        dst: SocketAddr,
    ) -> Option<Duration> {
        let pair = Pair::new(src, dst);

        match self.links[&pair] {
            Link::Healthy => {
                // Should the link be broken?
                if self.config.fail_rate > 0.0 {
                    if rand.gen_bool(self.config.fail_rate) {
                        self.num_partitioned += 1;
                        self.links.insert(pair, Link::RandPartition);
                        return None;
                    }
                }

                Some(send_delay(&self.config, rand))
            }
            Link::ExplicitPartition => None,
            Link::RandPartition => {
                // Should the link be repaired?
                if self.config.repair_rate > 0.0 {
                    if rand.gen_bool(self.config.repair_rate) {
                        self.num_partitioned -= 1;
                        self.links.remove(&pair);
                        return Some(send_delay(&self.config, rand));
                    }
                }

                None
            }
        }
    }

    pub(crate) fn partition(&mut self, a: SocketAddr, b: SocketAddr) {
        let pair = Pair::new(a, b);

        match self.links.insert(pair, Link::ExplicitPartition) {
            Some(Link::RandPartition | Link::ExplicitPartition) => {}
            _ => self.num_partitioned += 1,
        }
    }

    pub(crate) fn repair(&mut self, a: SocketAddr, b: SocketAddr) {
        let pair = Pair::new(a, b);
        match self.links.remove(&pair) {
            Some(Link::RandPartition | Link::ExplicitPartition) => self.num_partitioned += 1,
            _ => {}
        }
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

fn send_delay(config: &config::Link, rand: &mut dyn RngCore) -> Duration {
    let mult = config.distribution.sample(rand);
    let range = (config.max_message_latency - config.min_message_latency).as_millis() as f64;
    let delay = config.min_message_latency + Duration::from_millis((range * mult) as _);
    std::cmp::min(delay, config.max_message_latency)
}
