mod dns;
use dns::Dns;
pub use dns::ToSocketAddr;

mod inbox;

mod io;
pub use io::Io;

mod host;
use host::Host;

mod top;
use top::Topology;

use indexmap::IndexMap;
use rand::RngCore;
use std::cell::RefCell;
use std::future::Future;
use std::net::SocketAddr;
use std::rc::{self, Rc};

/// Network simulation
pub struct Sim<T: 'static> {
    /// Strong reference to shared simulation state
    inner: Rc<Inner<T>>,

    /// Handle to the hostname lookup
    dns: Dns,
}

/// Network builder
pub struct Net<T: 'static> {
    /// Shared state
    inner: rc::Weak<Inner<T>>,

    dns: Dns,

    /// Hosts in the simulated network
    hosts: IndexMap<SocketAddr, Host<T>>,

    /// How often any given link should fail (on a per-message basis).
    fail_rate: f64,

    /// How often any given link should be repaired (on a per-message basis);
    repair_rate: f64,
}

struct Inner<T: 'static> {
    /// Map of socket address to host
    hosts: RefCell<IndexMap<SocketAddr, Host<T>>>,

    /// Tracks the state of links between node pairs.
    topology: RefCell<Topology>,

    rand: RefCell<Box<dyn RngCore>>,
}

impl<T: 'static> Sim<T> {
    pub fn build(rng: Box<dyn RngCore>, builder: impl FnOnce(&mut Net<T>)) -> Sim<T> {
        let dns = Dns::new();

        let inner = Rc::new_cyclic(|inner| {
            let mut net = Net {
                inner: inner.clone(),
                dns: dns.clone(),
                hosts: IndexMap::new(),
                fail_rate: 0.0,
                repair_rate: 0.0,
            };

            builder(&mut net);

            Inner {
                hosts: RefCell::new(net.hosts),
                topology: RefCell::new(Topology::new(net.fail_rate, net.repair_rate)),
                rand: RefCell::new(rng),
            }
        });

        Sim { inner, dns }
    }

    pub fn run_until<R>(&self, until: impl Future<Output = R>) -> R {
        use std::task::Poll;

        let mut task = tokio_test::task::spawn(until);

        loop {
            if let Poll::Ready(ret) = task.poll() {
                return ret;
            }

            let hosts = self.inner.hosts.borrow();

            for host in hosts.values() {
                host.tick();
            }
        }
    }

    pub fn client(&self, addr: impl dns::ToSocketAddr) -> Io<T> {
        let addr = self.lookup(addr);
        let (host, stream) = Host::new_client(addr, self.dns.clone(), &self.inner);
        self.inner.hosts.borrow_mut().insert(addr, host);
        stream
    }

    pub fn lookup(&self, addr: impl dns::ToSocketAddr) -> SocketAddr {
        self.dns.lookup(addr)
    }

    // Introduce a full network partition between two hosts
    pub fn partition(&self, a: impl dns::ToSocketAddr, b: impl dns::ToSocketAddr) {
        let a = self.lookup(a);
        let b = self.lookup(b);

        self.inner.topology.borrow_mut().partition(a, b);
    }

    // Repair a partition between two hosts
    pub fn repair(&self, a: impl dns::ToSocketAddr, b: impl dns::ToSocketAddr) {
        let a = self.lookup(a);
        let b = self.lookup(b);

        self.inner.topology.borrow_mut().repair(a, b);
    }
}

impl<T: 'static> Net<T> {
    pub fn set_fail_rate(&mut self, value: f64) {
        self.fail_rate = value;
    }

    pub fn set_repair_rate(&mut self, value: f64) {
        self.repair_rate = value;
    }

    pub fn register<F, R>(&mut self, addr: impl dns::ToSocketAddr, host: F)
    where
        F: FnOnce(Io<T>) -> R,
        R: Future<Output = ()> + 'static,
    {
        let addr = self.dns.lookup(addr);
        assert!(
            !self.hosts.contains_key(&addr),
            "already registered host for the given socket address"
        );

        // Create the node with its paired inbound channel
        let (node, stream) = Host::new_simulated(addr, self.dns.clone(), &self.inner);

        match &node {
            Host::Simulated(host::Simulated { rt, local, .. }) => {
                // Initialize the host
                let _guard = rt.enter();
                let host_task = host(stream);
                local.spawn_local(host_task);
            }
            _ => unreachable!(),
        }

        self.hosts.insert(addr, node);
    }

    pub fn lookup(&self, addr: impl dns::ToSocketAddr) -> SocketAddr {
        self.dns.lookup(addr)
    }
}
