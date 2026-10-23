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

    /// Filesystem configuration
    #[cfg(feature = "unstable-fs")]
    pub(crate) fs: FsConfig,
}

/// Configures link behavior.
#[derive(Clone, Default)]
pub(crate) struct Link {
    /// Message latency between two hosts
    pub(crate) latency: Option<Latency>,

    /// How often sending a message works vs. the message getting dropped
    pub(crate) message_loss: Option<MessageLoss>,

    /// How TCP resends the segments the network drops. Only the
    /// simulation-wide value applies.
    pub(crate) tcp_retransmit: Option<TcpRetransmit>,
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

/// How TCP resends the segments that a partition or message loss drops.
///
/// A lost segment is resent after a retransmission timeout (RTO) drawn from
/// `rto`. Every further loss of the same segment doubles its RTO, up to
/// `max_rto`. Once `retries` resends are lost as well, the sender waits one
/// more RTO and gives up: its end of the connection resets, and reads and
/// writes fail as they do after a RST. The peer learns of it from the RST its
/// next segment draws. SYN and FIN segments are resent like data. A RST is
/// never resent. UDP stays lossy.
///
/// Without it, a dropped segment is gone for good and its stream stalls
/// forever.
///
/// This models loss recovery only, not TCP's machinery. Limits:
/// - There are no ACKs, congestion window or keepalive. A segment is resent
///   only when the network drops it, and a connection that sends nothing
///   never notices a dead peer.
/// - A one-way partition loses only the segments sent across it. Segments
///   flowing the other way are still delivered, although their ACKs would be
///   lost on a real network and trigger resends there.
/// - Loss is bursty: a partition drops everything in flight, and the resends
///   it schedules come back together once their timers fire.
///
/// The defaults are Linux's:
/// - `rto`: 200ms (`TCP_RTO_MIN`, the floor an estimate from a fast link
///   settles on) to 1s (`TCP_TIMEOUT_INIT`, before any estimate)
/// - `max_rto`: 120s (`TCP_RTO_MAX`)
/// - `retries`: 15 (`net.ipv4.tcp_retries2`)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TcpRetransmit {
    /// The range a lost segment's first RTO is drawn from.
    pub rto: RangeInclusive<Duration>,

    /// The RTO stops doubling here.
    pub max_rto: Duration,

    /// How many resends of one segment may be lost before the connection
    /// resets.
    pub retries: u32,
}

impl Default for TcpRetransmit {
    fn default() -> Self {
        Self {
            rto: Duration::from_millis(200)..=Duration::from_secs(1),
            max_rto: Duration::from_secs(120),
            retries: 15,
        }
    }
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
