mod builder;
pub use builder::Builder;

mod config;
use config::Config;

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
use std::fmt::Debug;
use std::future::Future;
use std::net::SocketAddr;
use std::rc::Rc;

/// Network simulation
pub struct Sim<T: Debug + 'static> {
    /// Strong reference to shared simulation state
    inner: Rc<Inner<T>>,
}

struct Inner<T: Debug + 'static> {
    /// Configuration settings
    config: Config,

    /// Hostname lookup
    dns: Dns,

    /// Map of socket address to host
    hosts: RefCell<IndexMap<SocketAddr, Host<T>>>,

    /// Tracks the state of links between node pairs.
    topology: RefCell<Topology>,

    rand: RefCell<Box<dyn RngCore>>,
}

impl<T: Debug + 'static> Sim<T> {
    /// Register a host with the simulation
    pub fn register<F, R>(&mut self, addr: impl ToSocketAddr, host: F)
    where
        F: FnOnce(Io<T>) -> R,
        R: Future<Output = ()> + 'static,
    {
        let addr = self.inner.dns.lookup(addr);
        let mut hosts = self.inner.hosts.borrow_mut();

        assert!(
            !hosts.contains_key(&addr),
            "already registered host for the given socket address"
        );

        // Create the node with its paired inbound channel
        let (node, stream) = Host::new_simulated(addr, &self.inner);

        match &node {
            Host::Simulated(crate::host::Simulated { rt, local, .. }) => {
                // Initialize the host
                let _guard = rt.enter();
                let host_task = host(stream);
                local.spawn_local(host_task);
            }
            _ => unreachable!(),
        }

        hosts.insert(addr, node);
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
                host.tick(&self.inner.config);
            }
        }
    }

    pub fn client(&self, addr: impl dns::ToSocketAddr) -> Io<T> {
        let addr = self.lookup(addr);
        let (host, stream) = Host::new_client(addr, &self.inner);
        self.inner.hosts.borrow_mut().insert(addr, host);
        stream
    }

    pub fn lookup(&self, addr: impl dns::ToSocketAddr) -> SocketAddr {
        self.inner.dns.lookup(addr)
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
