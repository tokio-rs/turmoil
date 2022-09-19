use crate::{version, Envelope, Message};

use indexmap::IndexMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

/// A host in the simulated network
pub(crate) struct Host {
    /// Host address
    pub(crate) addr: SocketAddr,

    /// Messages in-flight to the host. Some of these may still be "on the
    /// network".
    inbox: IndexMap<SocketAddr, VecDeque<Envelope>>,

    /// Signaled when a message becomes available to receive
    pub(crate) notify: Arc<Notify>,

    /// Current instant at the host
    pub(crate) now: Instant,

    epoch: Instant,

    /// Current host version. This is incremented each time a message is received.
    pub(crate) version: u64,
}

impl Host {
    pub(crate) fn new(addr: SocketAddr, now: Instant, notify: Arc<Notify>) -> Host {
        Host {
            addr,
            inbox: IndexMap::new(),
            notify,
            now,
            epoch: now,
            version: 0,
        }
    }

    /// Returns how long the host has been executing for in virtual time
    pub(crate) fn elapsed(&self) -> Duration {
        self.now - self.epoch
    }

    /// Returns a dot for the host at its current version
    pub(crate) fn dot(&self) -> version::Dot {
        version::Dot {
            host: self.addr,
            version: self.version,
        }
    }

    pub(crate) fn send(
        &mut self,
        src: version::Dot,
        delay: Option<Duration>,
        message: Box<dyn Message>,
    ) {
        let deliver_at = delay.map(|it| self.now + it);

        self.inbox.entry(src.host).or_default().push_back(Envelope {
            src,
            deliver_at,
            message,
        });

        self.notify.notify_one();
    }

    pub(crate) fn recv(&mut self) -> Option<Envelope> {
        let now = Instant::now();

        for deque in self.inbox.values_mut() {
            match deque.front() {
                Some(Envelope {
                    deliver_at: Some(time),
                    ..
                }) if *time <= now => {
                    self.version += 1;
                    return deque.pop_front();
                }
                _ => continue,
            }
        }

        None
    }

    pub(crate) fn recv_from(&mut self, peer: SocketAddr) -> Option<Envelope> {
        let now = Instant::now();
        let deque = self.inbox.entry(peer).or_default();

        match deque.front() {
            Some(Envelope {
                deliver_at: Some(time),
                ..
            }) if *time <= now => {
                self.version += 1;
                deque.pop_front()
            }
            _ => None,
        }
    }

    /// Releases all messages previously received from [`peer`]. These messages
    /// may be received immediately (on the next call to `[Host::recv]`).
    pub(crate) fn release(&mut self, peer: SocketAddr) {
        let now = Instant::now();
        let deque = self.inbox.entry(peer).or_default();

        for envelope in deque {
            if let Envelope {
                deliver_at: None, ..
            } = envelope
            {
                envelope.deliver_at = Some(now);
            }
        }

        self.notify.notify_one();
    }

    pub(crate) fn tick(&mut self, now: Instant) {
        self.now = now;

        for deque in self.inbox.values() {
            if let Some(Envelope {
                deliver_at: Some(time),
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
