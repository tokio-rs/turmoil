use crate::{version, Envelope, Message};

use indexmap::IndexMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

/// A host in the simulated network.
pub(crate) enum Host {
    /// A lightweight client
    Client(Inner),

    /// A fully simulated host
    Simulated(Inner),
}

impl Host {
    pub(crate) fn client(addr: SocketAddr, now: Instant, notify: Arc<Notify>) -> Self {
        Self::Client(Inner::new(addr, now, notify))
    }

    pub(crate) fn simulated(addr: SocketAddr, now: Instant, notify: Arc<Notify>) -> Self {
        Self::Simulated(Inner::new(addr, now, notify))
    }

    pub(crate) fn now(&self) -> Instant {
        self.host().now
    }

    /// Returns how long the host has been executing for in virtual time
    pub(crate) fn elapsed(&self) -> Duration {
        let inner = self.host();
        inner.now - inner.epoch
    }

    /// Returns a dot for the host at its current version
    pub(crate) fn dot(&self) -> version::Dot {
        let inner = self.host();

        version::Dot {
            host: inner.addr,
            version: inner.version,
        }
    }

    pub(crate) fn send(&mut self, src: version::Dot, delay: Duration, message: Box<dyn Message>) {
        let inner = self.host_mut();
        let deliver_at = inner.now + delay;

        inner
            .inbox
            .entry(src.host)
            .or_default()
            .push_back(Envelope {
                src,
                deliver_at,
                message,
            });

        inner.notify.notify_one();
    }

    pub(crate) fn recv(&mut self) -> Option<Envelope> {
        let inner = self.host_mut();
        let now = Instant::now();

        for deque in inner.inbox.values_mut() {
            match deque.front() {
                Some(Envelope { deliver_at, .. }) if *deliver_at <= now => {
                    inner.version += 1;
                    return deque.pop_front();
                }
                _ => continue,
            }
        }

        None
    }

    pub(crate) fn recv_from(&mut self, peer: SocketAddr) -> Option<Envelope> {
        let inner = self.host_mut();
        let now = Instant::now();
        let deque = inner.inbox.entry(peer).or_default();

        match deque.front() {
            Some(Envelope { deliver_at, .. }) if *deliver_at <= now => {
                inner.version += 1;
                deque.pop_front()
            }
            _ => None,
        }
    }

    pub(crate) fn tick(&mut self, now: Instant) {
        let inner = self.host_mut();
        inner.now = now;

        for deque in inner.inbox.values() {
            if let Some(Envelope { deliver_at, .. }) = deque.front() {
                if *deliver_at <= now {
                    inner.notify.notify_one();
                    return;
                }
            }
        }
    }

    fn host(&self) -> &Inner {
        match self {
            Host::Client(i) => i,
            Host::Simulated(i) => i,
        }
    }

    fn host_mut(&mut self) -> &mut Inner {
        match self {
            Host::Client(i) => i,
            Host::Simulated(i) => i,
        }
    }
}

/// A host in the simulated network
pub(crate) struct Inner {
    /// Host address
    addr: SocketAddr,

    /// Messages in-flight to the host. Some of these may still be "on the
    /// network".
    inbox: IndexMap<SocketAddr, VecDeque<Envelope>>,

    /// Signaled when a message becomes available to receive
    notify: Arc<Notify>,

    /// Current instant at the host
    now: Instant,

    epoch: Instant,

    /// Current host version. This is incremented each time a message is received.
    version: u64,
}

impl Inner {
    fn new(addr: SocketAddr, now: Instant, notify: Arc<Notify>) -> Self {
        Self {
            addr,
            inbox: IndexMap::new(),
            notify,
            now,
            epoch: now,
            version: 0,
        }
    }
}
