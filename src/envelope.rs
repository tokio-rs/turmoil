use crate::{version, Message};

use tokio::time::Instant;

pub(crate) struct Envelope {
    /// Who sent the message
    pub(crate) src: version::Dot,

    /// When to deliver the message
    pub(crate) deliver_at: Instant,

    /// Message value
    pub(crate) message: Box<dyn Message>,
}
