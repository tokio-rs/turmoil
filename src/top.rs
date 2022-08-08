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

    /// Current number of partitioned links
    num_partitioned: usize,
}

#[derive(Debug, Hash, Eq, PartialEq)]
struct Pair(SocketAddr, SocketAddr);

pub(crate) enum Link {
    /// The link was explicitly partitioned.
    ExplicitPartition,

    /// The link was randomly partitioned.
    RandPartition,
}

// TODO: this should be renamed. It tracks more than just link latency now.
#[derive(Clone)]
pub(crate) struct Latency {
    /// Minimum latency
    pub(crate) min_message_latency: Duration,

    /// Maximum latency
    pub(crate) max_message_latency: Duration,

    /// Value distribution
    distribution: Exp<f64>,

    /// Probability of a link failing
    pub(crate) fail_rate: f64,

    /// Probability of a failed link returning
    pub(crate) repair_rate: f64,

    /// Max number of links to partition
    max_partitions: Option<usize>,
}

impl Topology {
    pub(crate) fn new(config: Latency) -> Topology {
        Topology {
            latency: config,
            links: IndexMap::new(),
            num_partitioned: 0,
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
                    if self.can_partition() && rand.gen_bool(self.latency.fail_rate) {
                        self.num_partitioned += 1;
                        self.links.insert(pair, Link::RandPartition);
                        return None;
                    }
                }

                Some(self.latency.send_delay(rand))
            }
            Some(Link::ExplicitPartition) => None,
            Some(Link::RandPartition) => {
                // Should the link be repaired?
                if !client && self.latency.repair_rate > 0.0 {
                    if rand.gen_bool(self.latency.repair_rate) {
                        self.num_partitioned -= 1;
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

    /// Returns `true` if a new link can be partitioned without exceeding the
    /// max partitions.
    fn can_partition(&self) -> bool {
        self.num_partitioned < self.latency.max_partitions.unwrap_or(usize::MAX)
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
            min_message_latency: Duration::from_millis(0),
            max_message_latency: Duration::from_millis(100),
            distribution: Exp::new(5.0).unwrap(),
            fail_rate: 0.0,
            repair_rate: 1.0,
            max_partitions: None,
        }
    }
}

impl Latency {
    fn send_delay(&self, rand: &mut dyn RngCore) -> Duration {
        let mult = self.distribution.sample(rand);
        let range = (self.max_message_latency - self.min_message_latency).as_millis() as f64;
        let delay = self.min_message_latency + Duration::from_millis((range * mult) as _);
        std::cmp::min(delay, self.max_message_latency)
    }
}
