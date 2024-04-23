use rand_distr::Exp;
use std::{
    ops::RangeInclusive,
    time::{Duration, SystemTime},
};

/// Configuration for a simulation.
///
/// This configuration allows you to define different aspects of how the
/// simulation should run, such as the duration, or the tick rate.
///
/// ## Default values
///
/// The config provides a default of:
/// - duration: 10 seconds
/// - tick: 1ms
/// - epoch: the current system time
/// - ephemeral_ports: 49152..=65535
/// - tcp_capacity: 64
/// - udp_capacity: 64
#[derive(Clone)]
pub(crate) struct Config {
    /// How long the test should run for in simulated time.
    pub(crate) duration: Duration,

    /// How much simulated time should elapse each tick
    pub(crate) tick: Duration,

    /// When the simulation starts
    pub(crate) epoch: SystemTime,

    pub(crate) ephemeral_ports: RangeInclusive<u16>,

    /// Max size of the tcp receive buffer
    pub(crate) tcp_capacity: usize,

    /// Max size of the udp receive buffer
    pub(crate) udp_capacity: usize,

    /// Enables tokio IO driver
    pub(crate) enable_tokio_io: bool,

    /// Enables running of host/client code in random order at each
    /// simulation step
    pub(crate) random_node_order: bool,
}

/// Configures link behavior.
#[derive(Clone, Default)]
pub(crate) struct Link {
    /// Message latency between two hosts
    pub(crate) latency: Option<Latency>,

    /// How often sending a message works vs. the message getting dropped
    pub(crate) message_loss: Option<MessageLoss>,
}

/// Configure latency behavior between two hosts.
///
/// Provides default values of:
/// - min_message_latency: 0ms
/// - max_message_latency: 100ms
/// - latency_distribution: Exp(5)
#[derive(Clone)]
pub(crate) struct Latency {
    /// Minimum latency
    pub(crate) min_message_latency: Duration,

    /// Maximum latency
    pub(crate) max_message_latency: Duration,

    /// Probability distribution of latency within the range above.
    pub(crate) latency_distribution: Exp<f64>,
}

/// Configure how often messages are lost.
///
/// Provides default values of 0% failure rate, and 100% repair rate.
#[derive(Clone)]
pub(crate) struct MessageLoss {
    /// Probability of a link failing
    pub(crate) fail_rate: f64,

    /// Probability of a failed link returning
    pub(crate) repair_rate: f64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            duration: Duration::from_secs(10),
            tick: Duration::from_millis(1),
            epoch: SystemTime::now(),
            ephemeral_ports: 49152..=65535,
            tcp_capacity: 64,
            udp_capacity: 64,
            enable_tokio_io: false,
            random_node_order: false,
        }
    }
}

impl Link {
    pub(crate) fn latency(&self) -> &Latency {
        self.latency.as_ref().expect("`Latency` missing")
    }

    pub(crate) fn latency_mut(&mut self) -> &mut Latency {
        self.latency.as_mut().expect("`Latency` missing")
    }

    pub(crate) fn message_loss(&self) -> &MessageLoss {
        self.message_loss.as_ref().expect("`MessageLoss` missing")
    }

    pub(crate) fn message_loss_mut(&mut self) -> &mut MessageLoss {
        self.message_loss.as_mut().expect("`MessageLoss` missing")
    }
}

impl Default for Latency {
    fn default() -> Latency {
        Latency {
            min_message_latency: Duration::from_millis(0),
            max_message_latency: Duration::from_millis(100),
            latency_distribution: Exp::new(5.0).unwrap(),
        }
    }
}

impl Default for MessageLoss {
    fn default() -> MessageLoss {
        MessageLoss {
            fail_rate: 0.0,
            repair_rate: 1.0,
        }
    }
}
