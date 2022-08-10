use crate::{inbox, version, Dns, Topology, ToSocketAddr};
use crate::top::Latency;

use indexmap::IndexMap;
use rand::RngCore;
use scoped_tls::scoped_thread_local;
use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

/// Tracks all the state for the simulated world.
pub(crate) struct World {
    /// Tracks all individual hosts
    hosts: IndexMap<SocketAddr, Host>,

    /// Tracks how each host is connected to each other.
    topology: Topology,

    /// Maps hostnames to socket addresses.
    dns: Dns,

    /// If set, this is the current host being executed.
    current: Option<SocketAddr>,
    
    /// Random number generator used for all decisions. To make execution
    /// determinstic, reuse the same seed.
    rng: Box<dyn RngCore>,
}

scoped_thread_local!(static CURRENT: RefCell<World>);

impl World {
    /// Initialize a new world
    pub(crate) fn new(latency: Latency, rng: Box<dyn RngCore>) -> World {
        World {
            hosts: IndexMap::new(),
            topology: Topology::new(latency),
            dns: Dns::new(),
            current: None,
            rng,
        }
    }

    /// Get a mutable ref to the **current** world
    pub(crate) fn current<R>(f: impl FnOnce(&mut World) -> R) -> R {
        CURRENT.with(|current| {
            let mut current = current.borrow_mut();
            f(&mut *current)
        })
    }

    pub(crate) fn enter<R>(world: &RefCell<World>, f: impl FnOnce() -> R) -> R {
        CURRENT.set(world, f)
    }

    /// Return a mutable reference to the currently running host state
    pub(crate) fn current_mut(&mut self) -> &mut Host {
        let addr = self.current.expect("no current host");
        self.hosts.get_mut(&addr).expect("no host for address")
    }

    pub(crate) fn lookup(&mut self, host: impl ToSocketAddr) -> SocketAddr {
        self.dns.lookup(host)
    }

    /// Register a new host with the simulation
    pub(crate) fn register(&mut self, addr: SocketAddr, epoch: Instant, notify: Arc<Notify>) {
        assert!(
            !self.hosts.contains_key(&addr),
            "already registered host for the given socket address"
        );

        // Register links between the new host and all existing hosts
        for existing in self.hosts.keys() {
            self.topology.register(*existing, addr);
        }

        // Initialize host state
        self.hosts.insert(addr, Host::new(addr, epoch, notify));
    }

    /// Send a message between two hosts
    pub(crate) fn send(&mut self, src: SocketAddr, dst: SocketAddr, message: Box<dyn Any>) {
        if let Some(delay) = self.topology.send_delay(&mut self.rng, src, dst) {
            let src = version::Dot {
                host: src,
                version: self.hosts[&src].version
            };

            self.hosts[&dst].send(src, delay, message);
        }
    }
}

// ===== TODO: Move this to host.rs =====

/// A host in the simulated network
pub(crate) struct Host {
    /// Host's socket address
    pub(crate) addr: SocketAddr,

    /// Messages in-flight to the host. Some of these may still be "on the
    /// network".
    inbox: IndexMap<SocketAddr, VecDeque<inbox::Envelope>>,

    /// Signaled when a message becomes available to receive
    notify: Arc<Notify>,

    /// Current instant at the host
    now: Instant,

    /// Instant at which the host began
    epoch: Instant,

    /// Current host version. This is incremented each time a message is received.
    version: u64,
}

impl Host {
    pub(crate) fn new(addr: SocketAddr, epoch: Instant, notify: Arc<Notify>) -> Host {
        Host {
            addr,
            inbox: IndexMap::new(),
            notify,
            now: epoch,
            epoch,
            version: 0,
        }
    }

    pub(crate) fn send(&mut self, src: version::Dot, delay: Duration, message: Box<dyn Any>) {
        let deliver_at = self.now + delay;

        self.inbox.entry(src.host).or_default().push_back(inbox::Envelope {
            src,
            deliver_at,
            message,
        });

        self.notify.notify_one();
    }

    pub(crate) fn recv(&mut self) -> Option<inbox::Envelope> {
        let now = Instant::now();

        for deque in self.inbox.values_mut() {
            match deque.front() {
                Some(inbox::Envelope { deliver_at, .. }) if *deliver_at <= now => {
                    self.version += 1;
                    return deque.pop_front();
                }
                _ => {
                    // Fall through to the notify
                }
            }
        }

        None
    }

    pub(crate) fn recv_from(&mut self, src: SocketAddr) -> Option<inbox::Envelope> {
        let now = Instant::now();
        let deque = self.inbox.entry(src).or_default();

        match deque.front() {
            Some(inbox::Envelope { deliver_at, .. }) if *deliver_at <= now => {
                self.version += 1;
                deque.pop_front()
            }
            _ => None,
        }
    }
}