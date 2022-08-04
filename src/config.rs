use std::time::Duration;

#[derive(Clone)]
pub(crate) struct Config {
    /// How long the test should run for
    pub(crate) duration: Duration,

    /// How much simulated time should elapse each tick
    pub(crate) tick: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            duration: Duration::from_secs(10),
            tick: Duration::from_millis(1),
        }
    }
}
