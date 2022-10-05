use crate::{version, Message};

use tokio::time::Instant;

#[derive(Debug)]
pub(crate) struct Envelope {
    /// Who sent the message
    pub(crate) src: version::Dot,

    /// When (or if) to deliver the message
    pub(crate) instructions: DeliveryInstructions,

    /// Message value
    pub(crate) message: Box<dyn Message>,
}

#[derive(Debug)]
pub(crate) enum DeliveryInstructions {
    ExplicitlyHeld,
    DeliverAt(Instant),
}
