use rand_distr::Exp;
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct Config {
    /// How long the test should run for
    pub(crate) duration: Duration,

    /// How much simulated time should elapse each tick
    pub(crate) tick: Duration,
}

/// Configures link behavior.
#[derive(Clone)]
pub(crate) struct Link {
    /// Minimum latency
    pub(crate) min_message_latency: Duration,

    /// Maximum latency
    pub(crate) max_message_latency: Duration,

    /// Value distribution
    pub(crate) distribution: Exp<f64>,

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
        }
    }
}

impl Default for Link {
    fn default() -> Link {
        Link {
            min_message_latency: Duration::from_millis(0),
            max_message_latency: Duration::from_millis(100),
            distribution: Exp::new(5.0).unwrap(),
            fail_rate: 0.0,
            repair_rate: 1.0,
        }
    }
}
