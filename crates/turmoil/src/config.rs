use rand_distr::Exp;
use std::{
    ops::RangeInclusive,
    time::{Duration, SystemTime},
};

#[cfg(feature = "unstable-fs")]
use crate::fs::FsConfig;

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
/// - tcp_capacity: 64 KiB send and receive buffers
/// - udp_capacity: 64 datagrams
#[derive(Clone)]
pub(crate) struct Config {
    /// How long the test should run for in simulated time.
    pub(crate) duration: Duration,

    /// How much simulated time should elapse each tick
    pub(crate) tick: Duration,

    /// When the simulation starts
    pub(crate) epoch: SystemTime,

    pub(crate) ephemeral_ports: RangeInclusive<u16>,

    /// Per-socket TCP send and receive buffer size, in bytes. `None`
    /// keeps the network stack's default.
    pub(crate) tcp_capacity: Option<usize>,

    /// Datagrams queued per UDP socket before arrivals are dropped
    pub(crate) udp_capacity: usize,

    /// Enables tokio IO driver
    pub(crate) enable_tokio_io: bool,

    /// Enables running of host/client code in random order at each
    /// simulation step
    pub(crate) random_node_order: bool,

    /// Filesystem configuration
    #[cfg(feature = "unstable-fs")]
    pub(crate) fs: FsConfig,
}

/// TCP retransmits after this long without an ACK, matching Linux's
/// initial RTO. Stretched for links slow enough to exceed it.
const MIN_RTO: Duration = Duration::from_secs(1);

impl Config {
    /// Per-host network stack settings.
    ///
    /// The stack retransmits after a number of egress passes rather
    /// than elapsed time; turmoil egresses once per tick, so the count
    /// is the RTO in ticks.
    pub(crate) fn kernel(&self, max_latency: Duration) -> turmoil_net::KernelConfig {
        let mut kernel = turmoil_net::KernelConfig::default()
            .ephemeral_ports(self.ephemeral_ports.clone())
            .udp_recv_queue_cap(self.udp_capacity)
            .retx_threshold(self.retx_threshold(max_latency))
            .fin_wait2_timeout(self.network_ticks(Duration::from_secs(60)))
            .time_wait_timeout(self.network_ticks(Duration::from_secs(120)))
            .fragment_timeout(self.network_ticks(Duration::from_secs(30)));
        if let Some(capacity) = self.tcp_capacity {
            kernel = kernel.send_buf_cap(capacity).recv_buf_cap(capacity);
        }
        kernel
    }

    pub(crate) fn retx_threshold(&self, max_latency: Duration) -> u32 {
        self.network_ticks(MIN_RTO.max(max_latency.saturating_mul(3)))
    }

    fn network_ticks(&self, duration: Duration) -> u32 {
        duration
            .as_nanos()
            .div_ceil(self.tick.as_nanos().max(1))
            .try_into()
            .unwrap_or(u32::MAX)
    }
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
            tcp_capacity: None,
            udp_capacity: 64,
            enable_tokio_io: false,
            random_node_order: false,
            #[cfg(feature = "unstable-fs")]
            fs: FsConfig::default(),
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
