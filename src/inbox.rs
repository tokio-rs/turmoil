use crate::{version, Log};

use indexmap::IndexMap;
use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::rc::Rc;
use tokio::sync::Notify;
use tokio::time::Instant;

pub(crate) struct Sender {
    inner: Rc<Inner>,
}

pub(crate) struct Receiver {
    inner: Rc<Inner>,
}

pub(crate) fn channel() -> (Sender, Receiver) {
    let inner = Rc::new(Inner {
        messages: Default::default(),
        notify: Default::default(),
    });

    let tx = Sender {
        inner: inner.clone(),
    };
    let rx = Receiver { inner };

    (tx, rx)
}

struct Inner {
    /// Received messages
    messages: RefCell<IndexMap<SocketAddr, VecDeque<Envelope>>>,

    /// Notify that a message has been sent.
    notify: Notify,
}

pub(crate) struct Envelope {
    /// Who sent the message
    pub(crate) src: version::Dot,

    /// When to deliver the message
    pub(crate) deliver_at: Instant,

    /// Message value
    pub(crate) message: Box<dyn Any>,
}

impl Sender {
    /// Send a message
    pub(crate) fn send(&self, envelope: Envelope) {
        self.inner
            .messages
            .borrow_mut()
            .entry(envelope.src.host)
            .or_default()
            .push_back(envelope);

        self.inner.notify.notify_one();
    }

    pub(crate) fn tick(&self, now: Instant) {
        let messages = self.inner.messages.borrow();

        for queue in messages.values() {
            if let Some(Envelope { deliver_at, .. }) = queue.front() {
                if *deliver_at <= now {
                    self.inner.notify.notify_one();
                    return;
                }
            }
        }
    }
}

impl Receiver {
    /// Receive a message
    pub(crate) async fn recv(
        &self,
        mut now: impl FnMut() -> Instant,
    ) -> Envelope {
        loop {
            {
                let now = now();
                // Try reading a message
                let mut messages = self.inner.messages.borrow_mut();

                for per_host_messages in messages.values_mut() {
                    match per_host_messages.front() {
                        Some(Envelope { deliver_at, .. }) if *deliver_at <= now => {
                            return per_host_messages.pop_front().unwrap();
                        }
                        _ => {
                            // Fall through to the notify
                        }
                    }
                }
            }

            self.inner.notify.notified().await;
        }
    }

    pub(crate) async fn recv_from(
        &self,
        src: SocketAddr,
        mut now: impl FnMut() -> Instant,
    ) -> Envelope {
        loop {
            {
                let now = now();

                // Find a message matching the `src`
                let mut messages = self.inner.messages.borrow_mut();
                let messages = messages.entry(src).or_default();

                match messages.front() {
                    Some(Envelope { deliver_at, .. }) if *deliver_at <= now => {
                        return messages.pop_front().unwrap();
                    }
                    _ => {
                        // Fall through to the notify
                    }
                }
            }

            self.inner.notify.notified().await;
        }
    }
}

impl Clone for Receiver {
    fn clone(&self) -> Receiver {
        Receiver {
            inner: self.inner.clone(),
        }
    }
}
