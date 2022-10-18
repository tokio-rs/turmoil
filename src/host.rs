use crate::envelope::DeliveryInstructions;
use crate::{Envelope, Message};

use indexmap::IndexMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::rc::Rc;
use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

/// A host in the simulated network.
///
/// Hosts support two networking modes:
/// - Datagram
/// - Stream
///
/// Both modes may be used by host software simultaneously.
pub(crate) struct Host {
    /// Host address
    pub(crate) addr: SocketAddr,

    /// Messages in-flight to the host. Some of these may still be "on the
    /// network".
    inbox: IndexMap<SocketAddr, VecDeque<Envelope>>,

    /// Signaled when a message becomes available to receive.
    pub(crate) notify: Rc<Notify>,

    /// Current instant at the host.
    pub(crate) now: Instant,

    _epoch: Instant,
}

impl Host {
    pub(crate) fn new(addr: SocketAddr, now: Instant, notify: Rc<Notify>) -> Host {
        Host {
            addr,
            inbox: IndexMap::new(),
            notify,
            now,
            _epoch: now,
        }
    }

    /// Returns how long the host has been executing for in virtual time
    pub(crate) fn _elapsed(&self) -> Duration {
        self.now - self._epoch
    }

    pub(crate) fn embark(
        &mut self,
        src: SocketAddr,
        delay: Option<Duration>,
        message: Box<dyn Message>,
    ) {
        let instructions = match delay {
            Some(d) => DeliveryInstructions::DeliverAt(self.now + d),
            None => DeliveryInstructions::ExplicitlyHeld,
        };

        self.inbox.entry(src).or_default().push_back(Envelope {
            src,
            instructions,
            message,
        });

        self.notify.notify_one();
    }

    pub(crate) fn recv(&mut self) -> (Option<Envelope>, Rc<Notify>) {
        let now = Instant::now();
        let notify = self.notify.clone();

        for deque in self.inbox.values_mut() {
            match deque.front() {
                Some(Envelope {
                    instructions: DeliveryInstructions::DeliverAt(time),
                    ..
                }) if *time <= now => {
                    return (deque.pop_front(), notify);
                }
                _ => continue,
            }
        }

        (None, notify)
    }

    pub(crate) fn recv_from(&mut self, peer: SocketAddr) -> (Option<Envelope>, Rc<Notify>) {
        let now = Instant::now();

        let deque = self.inbox.entry(peer).or_default();
        let notify = self.notify.clone();

        match deque.front() {
            Some(Envelope {
                instructions: DeliveryInstructions::DeliverAt(time),
                ..
            }) if *time <= now => {
                return (deque.pop_front(), notify);
            }
            _ => (None, notify),
        }
    }

    /// Releases all messages previously received from [`peer`]. These messages
    /// may be received immediately (on the next call to `[Host::recv]`).
    pub(crate) fn release(&mut self, peer: SocketAddr) {
        let now = Instant::now();

        for envelope in self.inbox.entry(peer).or_default() {
            if let Envelope {
                instructions: DeliveryInstructions::ExplicitlyHeld,
                ..
            } = envelope
            {
                envelope.instructions = DeliveryInstructions::DeliverAt(now);
            }
        }

        self.notify.notify_one();
    }

    pub(crate) fn tick(&mut self, now: Instant) {
        self.now = now;

        for deque in self.inbox.values() {
            if let Some(Envelope {
                instructions: DeliveryInstructions::DeliverAt(time),
                ..
            }) = deque.front()
            {
                if *time <= now {
                    self.notify.notify_one();
                    return;
                }
            }
        }
    }
}
