use crate::{config, Dns, Envelope, Host, Log, Message, ToSocketAddr, Topology};

use indexmap::IndexMap;
use rand::RngCore;
use scoped_tls::scoped_thread_local;
use std::cell::RefCell;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::Instant;

/// Tracks all the state for the simulated world.
pub(crate) struct World {
    /// Tracks all individual hosts
    pub(crate) hosts: IndexMap<SocketAddr, Host>,

    /// Tracks how each host is connected to each other.
    pub(crate) topology: Topology,

    /// Maps hostnames to socket addresses.
    pub(crate) dns: Dns,

    /// If set, this is the current host being executed.
    pub(crate) current: Option<SocketAddr>,

    /// Handle to the logger
    pub(crate) log: Log,

    /// Random number generator used for all decisions. To make execution
    /// determinstic, reuse the same seed.
    rng: Box<dyn RngCore>,
}

scoped_thread_local!(static CURRENT: RefCell<World>);

impl World {
    /// Initialize a new world
    pub(crate) fn new(link: config::Link, log: Log, rng: Box<dyn RngCore>) -> World {
        World {
            hosts: IndexMap::new(),
            topology: Topology::new(link),
            dns: Dns::new(),
            current: None,
            log,
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

    /// Return a reference to the host
    pub(crate) fn host(&self, addr: SocketAddr) -> &Host {
        self.hosts.get(&addr).expect("host missing")
    }

    pub(crate) fn lookup(&mut self, host: impl ToSocketAddr) -> SocketAddr {
        self.dns.lookup(host)
    }

    pub(crate) fn partition(&mut self, a: SocketAddr, b: SocketAddr) {
        self.topology.partition(a, b);
    }

    pub(crate) fn repair(&mut self, a: SocketAddr, b: SocketAddr) {
        self.topology.repair(a, b);
    }

    /// Register a new host with the simulation
    pub(crate) fn client(&mut self, addr: SocketAddr, epoch: Instant, notify: Arc<Notify>) {
        self.register(addr);

        // Initialize host state
        self.hosts.insert(addr, Host::client(addr, epoch, notify));
    }

    /// Register a new host with the simulation
    pub(crate) fn simulated(&mut self, addr: SocketAddr, epoch: Instant, notify: Arc<Notify>) {
        self.register(addr);

        // Initialize host state
        self.hosts
            .insert(addr, Host::simulated(addr, epoch, notify));
    }

    fn register(&mut self, addr: SocketAddr) {
        assert!(
            !self.hosts.contains_key(&addr),
            "already registered host for the given socket address"
        );

        // Register links between the new host and all existing hosts
        for existing in self.hosts.keys() {
            self.topology.register(*existing, addr);
        }
    }

    /// Send a message between two hosts
    pub(crate) fn send(&mut self, host: SocketAddr, dst: SocketAddr, message: Box<dyn Message>) {
        if let Some(delay) = self.topology.send_delay(&mut self.rng, host, dst) {
            let host = &self.hosts[&host];
            let dot = host.dot();

            self.log.send(
                &self.dns,
                dot,
                host.elapsed(),
                dst,
                Some(delay),
                false,
                &*message,
            );

            self.hosts[&dst].send(dot, delay, message);
        } else {
            let host = &self.hosts[&host];
            let dot = host.dot();

            self.log
                .send(&self.dns, dot, host.elapsed(), dst, None, true, &*message);
        }
    }

    /// Receive a message
    pub(crate) fn recv(&mut self, host: SocketAddr) -> Option<Envelope> {
        let host = &mut self.hosts[&host];
        let ret = host.recv();

        if let Some(Envelope { src, message, .. }) = &ret {
            self.log
                .recv(&self.dns, host.dot(), host.elapsed(), *src, &**message);
        }

        ret
    }

    pub(crate) fn recv_from(&mut self, host: SocketAddr, peer: SocketAddr) -> Option<Envelope> {
        let host = &mut self.hosts[&host];
        let ret = host.recv_from(peer);

        if let Some(Envelope { src, message, .. }) = &ret {
            self.log
                .recv(&self.dns, host.dot(), host.elapsed(), *src, &**message);
        }

        ret
    }

    /// Tick a host
    pub(crate) fn tick(&mut self, addr: SocketAddr, now: Instant) {
        self.hosts.get_mut(&addr).expect("missing host").tick(now);
    }

    /// Returns a view of all the world's client hosts
    pub(crate) fn clients(&self) -> Vec<SocketAddr> {
        self.hosts
            .iter()
            .filter_map(|(addr, host)| match host {
                Host::Client(_) => Some(*addr),
                Host::Simulated(_) => None,
            })
            .collect()
    }
}
