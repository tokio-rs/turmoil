use crate::{version, Message};

use tokio::time::Instant;

pub(crate) struct Envelope {
    /// Who sent the message
    pub(crate) src: version::Dot,

    /// When to deliver the message. If `None`, the message is stuck without an ETA.
    pub(crate) deliver_at: Option<Instant>,

    /// Message value
    pub(crate) message: Box<dyn Message>,
}
