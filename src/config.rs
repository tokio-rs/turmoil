pub(crate) struct Config {
    /// How often any given link should fail (on a per-message basis).
    fail_rate: f64,

    /// How often any given link should be repaired (on a per-message basis);
    repair_rate: f64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            fail_rate: 0.0,
            repair_rate: 0.0,
        }
    }
}
