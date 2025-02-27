use crate::{
    config::{self, Config},
    envelope::Protocol,
    for_pairs,
    host::HostTimer,
    ip::IpVersionAddrIter,
    net::{IpAddr, SocketAddr},
    Dns, Host, Result as TurmoilResult, ToIpAddr, ToIpAddrs, Topology, TRACING_TARGET,
};

use indexmap::IndexMap;
use rand::RngCore;
use scoped_tls::scoped_thread_local;
use std::cell::RefCell;
use std::io::Result;
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

    /// Run duration for each host on every step.
    // TODO: Remove this once we've cleaned up the loopback implementation hacks
    pub(crate) tick_duration: Duration,
}

scoped_thread_local!(static CURRENT: RefCell<World>);

impl World {
    /// Initialize a new world.
    pub(crate) fn new(
        link: config::Link,
        rng: Box<dyn RngCore>,
        addrs: IpVersionAddrIter,
        tick_duration: Duration,
    ) -> World {
        World {
            hosts: IndexMap::new(),
            topology: Topology::new(link),
            dns: Dns::new(addrs),
            current: None,
            rng,
            tick_duration,
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

    pub(crate) fn try_current<R>(f: impl FnOnce(&World) -> R) -> TurmoilResult<R> {
        if CURRENT.is_set() {
            CURRENT.with(|current| match current.try_borrow() {
                Ok(world) => Ok(f(&world)),
                Err(_) => Err("World already borrowed".into()),
            })
        } else {
            Err("World not set".into())
        }
    }

    pub(crate) fn enter<R>(world: &RefCell<World>, f: impl FnOnce() -> R) -> R {
        CURRENT.set(world, f)
    }

    pub(crate) fn current_host_mut(&mut self) -> &mut Host {
        let addr = self.current.expect("current host missing");
        self.hosts.get_mut(&addr).expect("host missing")
    }

    pub(crate) fn current_host(&self) -> &Host {
        let addr = self.current.expect("current host missing");
        self.hosts.get(&addr).expect("host missing")
    }

    pub(crate) fn try_current_host(&self) -> TurmoilResult<&Host> {
        let addr = self.current.ok_or("current host missing")?;
        self.hosts.get(&addr).ok_or_else(|| "host missing".into())
    }

    pub(crate) fn lookup(&mut self, host: impl ToIpAddr) -> IpAddr {
        self.dns.lookup(host)
    }

    pub(crate) fn reverse_lookup(&self, addr: IpAddr) -> Option<&str> {
        self.dns.reverse(addr)
    }

    pub(crate) fn lookup_many(&mut self, hosts: impl ToIpAddrs) -> Vec<IpAddr> {
        self.dns.lookup_many(hosts)
    }

    pub(crate) fn hold(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.hold(a, b);
    }

    pub(crate) fn hold_many(&mut self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let a = self.lookup_many(a);
        let b = self.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            self.hold(a, b);
        });
    }

    pub(crate) fn release(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.release(a, b);
    }

    pub(crate) fn release_many(&mut self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let a = self.lookup_many(a);
        let b = self.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            self.release(a, b);
        });
    }

    pub(crate) fn partition(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.partition(a, b);
    }

    pub(crate) fn partition_oneway(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.partition_oneway(a, b);
    }

    pub(crate) fn partition_many(&mut self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let a = self.lookup_many(a);
        let b = self.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            self.partition(a, b);
        });
    }
    pub(crate) fn partition_oneway_many(&mut self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let a = self.lookup_many(a);
        let b = self.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            self.partition_oneway(a, b);
        });
    }
    pub(crate) fn repair(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.repair(a, b);
    }
    pub(crate) fn repair_oneway(&mut self, a: IpAddr, b: IpAddr) {
        self.topology.repair_oneway(a, b);
    }

    pub(crate) fn repair_many(&mut self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let a = self.lookup_many(a);
        let b = self.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            self.repair(a, b);
        });
    }

    pub(crate) fn repair_oneway_many(&mut self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let a = self.lookup_many(a);
        let b = self.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            self.repair_oneway(a, b);
        });
    }

    pub(crate) fn est_tcp_streams(&mut self) -> usize {
        self.current_host().tcp.stream_count()
    }

    pub(crate) fn est_tcp_streams_on(&mut self, addr: impl ToIpAddr) -> usize {
        self.hosts
            .get(&self.dns.lookup(addr))
            .unwrap()
            .tcp
            .stream_count()
    }

    /// Register a new host with the simulation.
    pub(crate) fn register(
        &mut self,
        addr: IpAddr,
        nodename: &str,
        timer: HostTimer,
        config: &Config,
    ) {
        assert!(
            !self.hosts.contains_key(&addr),
            "already registered host for the given ip address"
        );

        tracing::info!(target: TRACING_TARGET, nodename, ?addr, "New");

        // Register links between the new host and all existing hosts
        for existing in self.hosts.keys() {
            self.topology.register(*existing, addr);
        }

        // Initialize host state
        self.hosts.insert(
            addr,
            Host::new(
                nodename,
                addr,
                timer,
                config.ephemeral_ports.clone(),
                config.tcp_capacity,
                config.udp_capacity,
            ),
        );
    }

    /// Send `message` from `src` to `dst`. Delivery is asynchronous and not
    /// guaranteed.
    pub(crate) fn send_message(
        &mut self,
        src: SocketAddr,
        dst: SocketAddr,
        message: Protocol,
    ) -> Result<()> {
        self.topology
            .enqueue_message(&mut self.rng, src, dst, message)
    }

    /// Tick the host at `addr` by `duration`.
    pub(crate) fn tick(&mut self, addr: IpAddr, duration: Duration) {
        self.hosts
            .get_mut(&addr)
            .expect("missing host")
            .timer
            .tick(duration);
    }
}
