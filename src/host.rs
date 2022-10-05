use crate::envelope::DeliveryInstructions;
use crate::net::{Segment, SocketPair, StreamEnvelope, Syn};
use crate::{version, Envelope, Message};

use indexmap::IndexMap;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
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

    /// Optional accept queue; set if the host is bound. This simulates a server
    /// socket.
    listener: Option<Inbox<Envelope>>,

    /// In-flight messages for active connections. Some of these may still be
    /// "on the network".
    connections: IndexMap<SocketPair, Inbox<StreamEnvelope>>,

    /// Current instant at the host.
    pub(crate) now: Instant,

    epoch: Instant,

    /// Current host version. This is incremented each time a network operation
    /// occurs.
    pub(crate) version: u64,
}

/// A simple unbounded channel.
struct Inbox<T> {
    /// Queued items
    deque: VecDeque<T>,

    /// Signaled when an item is available to recv
    notify: Arc<Notify>,
}

impl Host {
    pub(crate) fn new(addr: SocketAddr, now: Instant, notify: Rc<Notify>) -> Host {
        Host {
            addr,
            inbox: IndexMap::new(),
            notify,
            listener: None,
            connections: IndexMap::new(),
            now,
            epoch: now,
            version: 0,
        }
    }

    /// Creates a new listener queue, which is bound to the host's `addr`.
    ///
    /// This is called by `Listener::bind()` and the returned `Notify` is used
    /// to signal when connections are available to accept.
    // TODO: Support binding to multiple ports
    pub(crate) fn bind(&mut self) -> io::Result<Arc<Notify>> {
        if self.listener.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                self.addr.to_string(),
            ));
        }

        let notify = Arc::new(Notify::new());

        self.listener.replace(Inbox {
            deque: VecDeque::new(),
            notify: notify.clone(),
        });
        self.bump_version();

        Ok(notify)
    }

    /// Unbind the host, dropping all pending connections.
    pub(crate) fn unbind(&mut self) {
        self.listener.take();
        self.bump_version();
    }

    /// Returns how long the host has been executing for in virtual time
    pub(crate) fn elapsed(&self) -> Duration {
        self.now - self.epoch
    }

    /// Bump the version for this host and return a dot.
    ///
    /// Called when a host establishes a new connection with a remote peer.
    pub(crate) fn bump(&mut self) -> version::Dot {
        self.bump_version();
        self.dot()
    }

    fn bump_version(&mut self) {
        self.version += 1;
    }

    /// Returns a dot for the host at its current version
    pub(crate) fn dot(&self) -> version::Dot {
        version::Dot {
            host: self.addr,
            version: self.version,
        }
    }

    pub(crate) fn accept(&mut self) -> Option<Envelope> {
        let now = Instant::now();
        let deque = &self.listener.as_ref()?.deque;

        // Iterate in order, skipping "held" envelopes, which is necessary to
        // avoid front-of-line blocking.
        for (index, envelope) in deque.iter().enumerate() {
            match envelope {
                Envelope {
                    instructions: DeliveryInstructions::DeliverAt(time),
                    ..
                } if *time <= now => {
                    self.bump_version();
                    let deque_mut = &mut self.listener.as_mut()?.deque;
                    return deque_mut.remove(index);
                }
                _ => continue,
            }
        }

        None
    }

    // If the host is not bound we simply do nothing, dropping the `Syn`. The
    // peer who initiated the connection is awaiting the receiver of the syn's
    // oneshot, which triggers a "connection refused" error.
    pub(crate) fn syn(&mut self, src: version::Dot, delay: Option<Duration>, syn: Syn) {
        if let Some(listener) = self.listener.as_mut() {
            let instructions = match delay {
                Some(d) => DeliveryInstructions::DeliverAt(self.now + d),
                None => DeliveryInstructions::ExplicitlyHeld,
            };

            listener.deque.push_back(Envelope {
                src,
                instructions,
                message: Box::new(syn),
            });

            listener.notify.notify_one();
        }
    }

    /// Setup a new connection for the `pair`.
    pub(crate) fn setup(&mut self, pair: SocketPair) {
        let inbox = Inbox {
            deque: VecDeque::new(),
            notify: Arc::new(Notify::new()),
        };

        let contains_pair = self.connections.insert(pair, inbox).is_some();

        assert!(!contains_pair, "{:?} is already registered", pair);
    }

    /// Receive notifications for `pair`'s connection.
    pub(crate) fn subscribe(&self, pair: SocketPair) -> Arc<Notify> {
        self.connections[&pair].notify.clone()
    }

    pub(crate) fn embark(
        &mut self,
        src: version::Dot,
        delay: Option<Duration>,
        message: Box<dyn Message>,
    ) {
        let instructions = match delay {
            Some(d) => DeliveryInstructions::DeliverAt(self.now + d),
            None => DeliveryInstructions::ExplicitlyHeld,
        };

        self.inbox.entry(src.host).or_default().push_back(Envelope {
            src,
            instructions,
            message,
        });

        self.notify.notify_one();
    }

    pub(crate) fn embark_on(
        &mut self,
        pair: SocketPair,
        delay: Option<Duration>,
        segment: Segment,
    ) {
        let inbox = self.connections.get_mut(&pair).expect("no connection");

        let instructions = match delay {
            Some(d) => DeliveryInstructions::DeliverAt(self.now + d),
            None => DeliveryInstructions::ExplicitlyHeld,
        };

        inbox.deque.push_back(StreamEnvelope {
            instructions,
            segment,
        });

        inbox.notify.notify_one();
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
                    let ret = (deque.pop_front(), notify);
                    self.bump_version();
                    return ret;
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
                let ret = (deque.pop_front(), notify);
                self.bump_version();
                ret
            }
            _ => (None, notify),
        }
    }

    pub(crate) fn recv_on(&mut self, pair: SocketPair) -> Option<Segment> {
        let now = Instant::now();

        let deque = self
            .connections
            .get_mut(&pair)
            .map(|i| &mut i.deque)
            .expect("no connection");

        match deque.front() {
            Some(StreamEnvelope {
                instructions: DeliveryInstructions::DeliverAt(time),
                ..
            }) if *time <= now => {
                let ret = deque.pop_front().map(|e| e.segment);
                self.bump_version();
                ret
            }
            _ => None,
        }
    }

    /// Releases all messages previously received from [`peer`]. These messages
    /// may be received immediately (on the next call to `[Host::recv]`).
    pub(crate) fn release(&mut self, peer: SocketAddr) {
        let now = Instant::now();

        if let Some(listener) = self.listener.as_mut() {
            for syn in listener.deque.iter_mut() {
                if let Envelope {
                    instructions: DeliveryInstructions::ExplicitlyHeld,
                    ..
                } = syn
                {
                    syn.instructions = DeliveryInstructions::DeliverAt(now);
                }
            }

            listener.notify.notify_one();
        }

        for (pair, inbox) in self.connections.iter_mut() {
            if pair.peer.host == peer {
                for envelope in &mut inbox.deque {
                    if let StreamEnvelope {
                        instructions: DeliveryInstructions::ExplicitlyHeld,
                        ..
                    } = envelope
                    {
                        envelope.instructions = DeliveryInstructions::DeliverAt(now);
                    }
                }

                inbox.notify.notify_one();
            }
        }

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

        if let Some(listener) = self.listener.as_ref() {
            for envelope in listener.deque.iter() {
                if let Envelope {
                    instructions: DeliveryInstructions::DeliverAt(time),
                    ..
                } = envelope
                {
                    if *time <= now {
                        listener.notify.notify_one();
                        break;
                    }
                }
            }
        }

        for inbox in self.connections.values() {
            if let Some(StreamEnvelope {
                instructions: DeliveryInstructions::DeliverAt(time),
                ..
            }) = inbox.deque.front()
            {
                if *time <= now {
                    inbox.notify.notify_one();
                }
            }
        }

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
