use crate::{version, Envelope};

use indexmap::IndexMap;
use std::any::Any;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

/// A host in the simulated network
pub(crate) struct Host {
    /// Messages in-flight to the host. Some of these may still be "on the
    /// network".
    inbox: IndexMap<SocketAddr, VecDeque<Envelope>>,

    /// Signaled when a message becomes available to receive
    notify: Arc<Notify>,

    /// Current instant at the host
    pub(crate) now: Instant,

    /// Current host version. This is incremented each time a message is received.
    pub(crate) version: u64,
}

impl Host {
    pub(crate) fn new(now: Instant, notify: Arc<Notify>) -> Host {
        Host {
            inbox: IndexMap::new(),
            notify,
            now,
            version: 0,
        }
    }

    pub(crate) fn send(&mut self, src: version::Dot, delay: Duration, message: Box<dyn Any>) {
        let deliver_at = self.now + delay;

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
                Some(Envelope { deliver_at, .. }) if *deliver_at <= now => {
                    self.version += 1;
                    return deque.pop_front();
                }
                _ => continue,
            }
        }

        None
    }

    pub(crate) fn recv_from(&mut self, src: SocketAddr) -> Option<Envelope> {
        let now = Instant::now();
        let deque = self.inbox.entry(src).or_default();

        match deque.front() {
            Some(Envelope { deliver_at, .. }) if *deliver_at <= now => {
                self.version += 1;
                deque.pop_front()
            }
            _ => None,
        }
    }

    pub(crate) fn tick(&mut self, now: Instant) {
        self.now = now;

        for deque in self.inbox.values() {
            if let Some(Envelope { deliver_at, .. }) = deque.front() {
                if *deliver_at <= now {
                    self.notify.notify_one();
                    return;
                }
            }
        }
    }
}
