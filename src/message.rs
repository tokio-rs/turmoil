use std::fmt;
use std::io::Write;

pub trait Message: fmt::Debug + Send + 'static {
    /// Serialize the message to JSON.
    ///
    /// This feature is used when logging a Turmoil execution.
    fn write_json(&self, dst: &mut dyn Write);
}
