use crate::{lookup, message, Message, ToSocketAddr, World};

use std::net::SocketAddr;

/// Send a message to `dst`.
///
/// Must be called from a host within a Turmoil simulation.
pub fn send<M: Message>(dst: impl ToSocketAddr, message: M) {
    World::current(|world| {
        let dst = world.lookup(dst);
        world.embark(dst, Box::new(message));
    });
}

/// Receive a message.
///
/// Must be called from a host within a Turmoil simulation.
pub async fn recv<M: Message>() -> (M, SocketAddr) {
    loop {
        let (maybe_envelope, notify) = World::current(|world| world.recv());

        if let Some(envelope) = maybe_envelope {
            let message = message::downcast::<M>(envelope.message);
            return (message, envelope.src.host);
        }

        notify.notified().await;
    }
}

/// Receive a message from `peer`.
///
/// Must be called from a host within a Turmoil simulation.
pub async fn recv_from<M: Message>(peer: impl ToSocketAddr) -> M {
    let peer = lookup(peer);

    loop {
        let (maybe_envelope, notify) = World::current(|world| world.recv_from(peer));

        if let Some(envelope) = maybe_envelope {
            return message::downcast::<M>(envelope.message);
        }

        notify.notified().await;
    }
}
