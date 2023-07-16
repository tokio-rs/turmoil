use rand_distr::Exp;
use std::time::{Duration, SystemTime};

#[derive(Clone)]
pub(crate) struct Config {
    /// How long the test should run for
    pub(crate) duration: Duration,

    /// How much simulated time should elapse each tick
    pub(crate) tick: Duration,

    /// When the simulation starts
    pub(crate) epoch: SystemTime,

    /// Max size of the tcp receive buffer
    pub(crate) tcp_capacity: usize,

    /// Max size of the udp receive buffer
    pub(crate) udp_capacity: usize,
}

/// Configures link behavior.
#[derive(Clone, Default)]
pub(crate) struct Link {
    /// Message latency between two hosts
    pub(crate) latency: Option<Latency>,

    /// How often sending a message works vs. the message getting dropped
    pub(crate) message_loss: Option<MessageLoss>,

    /// How often a sent message is duplicated. Only applies to UDP.
    pub(crate) message_duplication: Option<MessageDupliaction>,
}

/// Configure latency behavior between two hosts.
#[derive(Clone)]
pub(crate) struct Latency {
    /// Minimum latency
    pub(crate) min_message_latency: Duration,

    /// Maximum latency
    pub(crate) max_message_latency: Duration,

    /// Probability distribution of latency within the range above.
    pub(crate) latency_distribution: Exp<f64>,
}

/// Configure how often messages are lost
#[derive(Clone)]
pub(crate) struct MessageLoss {
    /// Probability of a link failing
    pub(crate) fail_rate: f64,

    /// Probability of a failed link returning
    pub(crate) repair_rate: f64,
}

/// Configure how often messages are duplicated
#[derive(Clone)]
pub(crate) struct MessageDupliaction {
    /// Probability of a link duplicating messages. Only applies to UDP.
    pub(crate) duplication_rate: f64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            duration: Duration::from_secs(10),
            tick: Duration::from_millis(1),
            epoch: SystemTime::now(),
            tcp_capacity: 64,
            udp_capacity: 64,
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

    pub(crate) fn message_duplication(&self) -> &MessageDupliaction {
        self.message_duplication
            .as_ref()
            .expect("`MessageDupliaction` missing")
    }

    pub(crate) fn message_duplication_mut(&mut self) -> &mut MessageDupliaction {
        self.message_duplication
            .as_mut()
            .expect("`MessageDupliaction` missing")
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

impl Default for MessageDupliaction {
    fn default() -> MessageDupliaction {
        MessageDupliaction {
            duplication_rate: 0.0,
        }
    }
}
