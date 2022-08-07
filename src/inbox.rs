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
    messages: RefCell<IndexMap<SocketAddr, VecDeque<Message>>>,

    /// Notify that a message has been sent.
    notify: Notify,
}

struct Message {
    /// Who sent the message
    src: SocketAddr,

    /// When to deliver the message
    deliver_at: Instant,

    /// Message value
    value: Box<dyn Any>,
}

impl Sender {
    /// Send a message
    pub(crate) fn send(&self, src: SocketAddr, deliver_at: Instant, message: Box<dyn Any>) {
        self.inner
            .messages
            .borrow_mut()
            .entry(src)
            .or_default()
            .push_back(Message {
                src,
                deliver_at,
                value: message,
            });

        self.inner.notify.notify_one();
    }

    pub(crate) fn tick(&self, now: Instant) {
        let messages = self.inner.messages.borrow();

        for queue in messages.values() {
            if let Some(Message { deliver_at, .. }) = queue.front() {
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
    ) -> (Box<dyn Any>, SocketAddr) {
        loop {
            {
                let now = now();
                // Try reading a message
                let mut messages = self.inner.messages.borrow_mut();

                for per_host_messages in messages.values_mut() {
                    match per_host_messages.front() {
                        Some(Message { deliver_at, .. }) if *deliver_at <= now => {
                            let Message { value, src, .. } = per_host_messages.pop_front().unwrap();
                            return (value, src);
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
    ) -> Box<dyn Any> {
        loop {
            {
                let now = now();

                // Find a message matching the `src`
                let mut messages = self.inner.messages.borrow_mut();
                let messages = messages.entry(src).or_default();

                match messages.front() {
                    Some(Message { deliver_at, .. }) if *deliver_at <= now => {
                        let message = messages.pop_front().unwrap();
                        return message.value;
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
