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
}

#[derive(Debug, Hash, Eq, PartialEq)]
struct Pair(SocketAddr, SocketAddr);

struct Link {
    state: State,

    /// Optional, per-link configuration.
    config: config::Link,
}

enum State {
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
        }
    }

    /// Register a link between two hosts
    pub(crate) fn register(&mut self, a: SocketAddr, b: SocketAddr) {
        let pair = Pair::new(a, b);
        assert!(self.links.insert(pair, Link::new()).is_none());
    }

    pub(crate) fn set_max_message_latency(&mut self, value: Duration) {
        self.config.latency_mut().max_message_latency = value;
    }

    pub(crate) fn set_link_max_message_latency(
        &mut self,
        a: SocketAddr,
        b: SocketAddr,
        value: Duration,
    ) {
        self.links[&Pair::new(a, b)]
            .latency(self.config.latency())
            .max_message_latency = value;
    }

    pub(crate) fn set_message_latency_curve(&mut self, value: f64) {
        self.config.latency_mut().latency_distribution = Exp::new(value).unwrap();
    }

    pub(crate) fn set_fail_rate(&mut self, value: f64) {
        self.config.message_loss_mut().fail_rate = value;
    }

    pub(crate) fn set_link_fail_rate(&mut self, a: SocketAddr, b: SocketAddr, value: f64) {
        self.links[&Pair::new(a, b)]
            .message_los(&self.config.message_loss())
            .fail_rate = value;
    }

    pub(crate) fn send_delay(
        &mut self,
        rand: &mut dyn RngCore,
        src: SocketAddr,
        dst: SocketAddr,
    ) -> Option<Duration> {
        self.links[&Pair::new(src, dst)].send_delay(&self.config, rand)
    }

    pub(crate) fn partition(&mut self, a: SocketAddr, b: SocketAddr) {
        self.links[&Pair::new(a, b)].explicit_partition();
    }

    pub(crate) fn repair(&mut self, a: SocketAddr, b: SocketAddr) {
        self.links[&Pair::new(a, b)].explicit_repair();
    }
}

impl Link {
    fn new() -> Link {
        Link {
            state: State::Healthy,
            config: config::Link::default(),
        }
    }

    fn send_delay(
        &mut self,
        global_config: &config::Link,
        rand: &mut dyn RngCore,
    ) -> Option<Duration> {
        match self.state {
            State::Healthy => {
                // Should the link be broken?
                if self.rand_partition(global_config.message_loss(), rand) {
                    self.state = State::RandPartition;
                    return None;
                }

                Some(self.delay(global_config.latency(), rand))
            }
            State::ExplicitPartition => None,
            State::RandPartition => {
                // Should the link be repaired?
                if self.rand_repair(global_config.message_loss(), rand) {
                    self.state = State::Healthy;
                    return Some(self.delay(&global_config.latency(), rand));
                }

                None
            }
        }
    }

    fn explicit_partition(&mut self) {
        self.state = State::ExplicitPartition;
    }

    fn explicit_repair(&mut self) {
        self.state = State::Healthy;
    }

    /// Should the link be randomly partitioned
    fn rand_partition(&self, global: &config::MessageLoss, rand: &mut dyn RngCore) -> bool {
        let config = self.config.message_loss.as_ref().unwrap_or(global);
        let fail_rate = config.fail_rate;
        fail_rate > 0.0 && rand.gen_bool(fail_rate)
    }

    fn rand_repair(&self, global: &config::MessageLoss, rand: &mut dyn RngCore) -> bool {
        let config = self.config.message_loss.as_ref().unwrap_or(global);
        let repair_rate = config.repair_rate;
        repair_rate > 0.0 && rand.gen_bool(repair_rate)
    }

    fn delay(&self, global: &config::Latency, rand: &mut dyn RngCore) -> Duration {
        let config = self.config.latency.as_ref().unwrap_or(global);

        let mult = config.latency_distribution.sample(rand);
        let range = (config.max_message_latency - config.min_message_latency).as_millis() as f64;
        let delay = config.min_message_latency + Duration::from_millis((range * mult) as _);

        std::cmp::min(delay, config.max_message_latency)
    }

    fn latency(&mut self, global: &config::Latency) -> &mut config::Latency {
        self.config.latency.get_or_insert_with(|| global.clone())
    }

    fn message_los(&mut self, global: &config::MessageLoss) -> &mut config::MessageLoss {
        self.config
            .message_loss
            .get_or_insert_with(|| global.clone())
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
