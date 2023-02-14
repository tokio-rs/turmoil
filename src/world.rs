use crate::envelope::{Protocol};
use crate::{config, Dns, Host, ToIpAddr, ToIpAddrs, Topology, TRACING_TARGET};

use indexmap::IndexMap;
use rand::RngCore;
use scoped_tls::scoped_thread_local;
use std::cell::RefCell;
use std::net::{IpAddr, SocketAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

/// Tracks all the state for the simulated world.
pub(crate) struct World {
    /// Tracks all individual hosts
    pub(crate) hosts: IndexMap<IpAddr, Host>,

    /// Tracks how each host is connected to each other.
    pub(crate) topology: Topology,

    /// Maps hostnames to ip addresses.
    pub(crate) dns: Dns,

    /// If set, this is the current host being executed.
    pub(crate) current: Option<IpAddr>,

    /// Random number generator used for all decisions. To make execution
    /// determinstic, reuse the same seed.
    pub(crate) rng: Box<dyn RngCore>,
}

scoped_thread_local!(static CURRENT: RefCell<World>);

impl World {
    /// Initialize a new world.
    pub(crate) fn new(link: config::Link, rng: Box<dyn RngCore>) -> World {
        World {
            hosts: IndexMap::new(),
            topology: Topology::new(link),
            dns: Dns::new(),
            current: None,
            rng,
        }
    }

    /// Run `f` on the world.
    pub(crate) fn current<R>(f: impl FnOnce(&mut World) -> R) -> R {
        CURRENT.with(|current| {
            let mut current = current.borrow_mut();
            f(&mut current)
        })
    }

    /// Run `f` if the world is set - otherwise no-op.
    ///
    /// Used in drop paths, where the simulation may be shutting
    /// down and we don't need to do anything.
    pub(crate) fn current_if_set(f: impl FnOnce(&mut World)) {
        if CURRENT.is_set() {
            Self::current(f);
        }
    }

    pub(crate) fn enter<R>(world: &RefCell<World>, f: impl FnOnce() -> R) -> R {
        CURRENT.set(world, f)
    }

    pub(crate) fn current_host_mut(&mut self) -> &mut Host {
        let addr = self.current.expect("current host missing");
        self.hosts.get_mut(&addr).expect("host missing")
    }

    pub(crate) fn lookup(&mut self, host: impl ToIpAddr) -> IpAddr {
        self.dns.lookup(host)
    }

    pub(crate) fn lookup_many(&mut self, hosts: impl ToIpAddrs) -> Vec<IpAddr> {
        self.dns.lookup_many(hosts)
    }

    pub(crate) fn hold(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.hold(a, b);
    }

    pub(crate) fn release(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.release(a, b);
    }

    pub(crate) fn partition(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.partition(a, b);
    }

    pub(crate) fn repair(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.repair(a, b);
    }

    /// Register a new host with the simulation.
    pub(crate) fn register(&mut self, addr: IpAddr) {
        assert!(
            !self.hosts.contains_key(&addr),
            "already registered host for the given ip address"
        );

        tracing::info!(target: TRACING_TARGET, hostname = ?self.dns.reverse(addr), ?addr, "New");

        // // Handles connection within a host
        self.topology.register(addr, addr);
        self.topology.register(Ipv4Addr::LOCALHOST.into(), addr);
        self.topology.register(Ipv6Addr::LOCALHOST.into(), addr);


        // Register links between the new host and all existing hosts
        for existing in self.hosts.keys() {
            self.topology.register(*existing, addr);
        }

        // Initialize host state
        self.hosts.insert(addr, Host::new(addr));
    }

    /// Send `message` from `src` to `dst`.
    /// Delivery between hosts is asynchronous and not guaranteed.
    pub(crate) fn send_message(&mut self, src: SocketAddr, dst: SocketAddr, message: Protocol) {
        self.topology
            .enqueue_message(&mut self.rng, src, dst, message);
    }

    /// Tick the host at `addr` by `duration`.
    pub(crate) fn tick(&mut self, addr: IpAddr, duration: Duration) {
        self.hosts
            .get_mut(&addr)
            .expect("missing host")
            .tick(duration);
    }
}
