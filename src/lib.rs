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

mod log;
use log::Log;

mod message;
pub use message::Message;

mod top;
use top::Topology;

mod version;

use indexmap::IndexMap;
use rand::RngCore;
use std::cell::RefCell;
use std::future::Future;
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

/// Network simulation
pub struct Sim {
    /// Strong reference to shared simulation state
    inner: Rc<Inner>,
}

struct Inner {
    /// Configuration settings
    config: Config,

    /// Hostname lookup
    dns: Dns,

    /// Map of socket address to host
    hosts: RefCell<IndexMap<SocketAddr, Host>>,

    /// Tracks the state of links between node pairs.
    topology: RefCell<Topology>,

    rand: RefCell<Box<dyn RngCore>>,
}

impl Sim {
    /// Register a host with the simulation
    pub fn register<F, M, R>(&mut self, addr: impl ToSocketAddr, host: F)
    where
        F: FnOnce(Io<M>) -> R,
        M: Message,
        R: Future<Output = ()> + 'static,
    {
        let addr = self.inner.dns.lookup(addr);
        let mut hosts = self.inner.hosts.borrow_mut();
        let mut topology = self.inner.topology.borrow_mut();

        assert!(
            !hosts.contains_key(&addr),
            "already registered host for the given socket address"
        );

        // Register links between the new host and all existing hosts
        for existing in hosts.keys() {
            topology.register(*existing, addr);
        }

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

    pub fn client<M: Message>(&self, addr: impl dns::ToSocketAddr) -> Io<M> {
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

    /// Set the max message latency
    pub fn set_max_message_latency(&self, value: Duration) {
        self.inner
            .topology
            .borrow_mut()
            .set_max_message_latency(value);
    }

    /// Set the message latency distribution curve.
    ///
    /// Message latency follows an exponential distribution curve. The `value`
    /// is the lambda argument to the probability function.
    pub fn set_message_latency_curve(&self, value: f64) {
        self.inner
            .topology
            .borrow_mut()
            .set_message_latency_curve(value);
    }
}
