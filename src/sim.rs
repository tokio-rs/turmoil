use crate::{Config, Role, Rt, ToSocketAddr, World};

use indexmap::IndexMap;
use std::cell::RefCell;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::Duration;

/// Network simulation
pub struct Sim {
    /// Simulation configuration
    config: Config,

    /// Tracks the simulated world state
    ///
    /// This is what is stored in the thread-local
    world: RefCell<World>,

    /// Per simulated host runtimes
    rts: IndexMap<SocketAddr, Role>,
}

impl Sim {
    pub(crate) fn new(config: Config, world: World) -> Sim {
        Sim {
            config,
            world: RefCell::new(world),
            rts: IndexMap::new(),
        }
    }

    /// Register a client with the simulation
    pub fn client<F>(&mut self, addr: impl ToSocketAddr, client: F)
    where
        F: Future<Output = ()> + 'static,
    {
        let rt = Rt::new();
        let epoch = rt.now();
        let addr = self.lookup(addr);

        {
            let world = RefCell::get_mut(&mut self.world);
            let notify = Arc::new(Notify::new());

            // Register host state with the world
            world.register(addr, epoch, notify.clone());
        }

        let handle = World::enter(&self.world, || rt.with(|| tokio::task::spawn_local(client)));

        self.rts.insert(addr, Role::client(rt, handle));
    }

    /// Register a host with the simulation
    ///
    /// This method takes a `Fn` that builds a future, as opposed to
    /// [`Sim::client`] which just takes a future. The reason for this is we
    /// might restart the host, and so need to be able to call the future
    /// multiple times.
    ///
    // TODO: Removing the `'static` bound on `F` would make writing some tests
    // easier.
    pub fn host<F, Fut>(&mut self, addr: impl ToSocketAddr, host: F)
    where
        F: Fn() -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let rt = Rt::new();
        let epoch = rt.now();
        let addr = self.lookup(addr);

        {
            let world = RefCell::get_mut(&mut self.world);
            let notify = Arc::new(Notify::new());

            // Register host state with the world
            world.register(addr, epoch, notify.clone());
        }

        World::enter(&self.world, || {
            rt.with(|| {
                tokio::task::spawn_local(host());
            });
        });

        self.rts.insert(addr, Role::simulated(rt, host));
    }

    /// Crash a host. Nothing will be running on the host after this method. You
    /// can use [`Sim::bounce`] to start the host up again.
    pub fn crash(&mut self, addr: impl ToSocketAddr) {
        let h = self.world.borrow_mut().lookup(addr);
        let rt = self.rts.get_mut(&h).expect("missing host");
        match rt {
            Role::Client { .. } => panic!("can only bounce hosts, not clients"),
            Role::Simulated { rt, .. } => {
                rt.cancel_tasks();
            }
        }
    }

    /// Bounce a host. The software is restarted.
    // TODO: Should the host's version be bumped or reset?
    pub fn bounce(&mut self, addr: impl ToSocketAddr) {
        let h = self.world.borrow_mut().lookup(addr);
        let rt = self.rts.get_mut(&h).expect("missing host");
        match rt {
            Role::Client { .. } => panic!("can only bounce hosts, not clients"),
            Role::Simulated { rt, software } => {
                rt.cancel_tasks();
                World::enter(&self.world, || {
                    rt.with(|| {
                        tokio::task::spawn_local(software());
                    });
                });
            }
        }
    }

    /// Lookup a socket address by host name
    pub fn lookup(&self, addr: impl ToSocketAddr) -> SocketAddr {
        self.world.borrow_mut().lookup(addr)
    }

    // Introduce a full network partition between two hosts
    pub fn partition(&self, a: impl ToSocketAddr, b: impl ToSocketAddr) {
        let mut world = self.world.borrow_mut();
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.partition(a, b);
    }

    // Repair a partition between two hosts
    pub fn repair(&self, a: impl ToSocketAddr, b: impl ToSocketAddr) {
        let mut world = self.world.borrow_mut();
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.repair(a, b);
    }

    /// Set the max message latency
    pub fn set_max_message_latency(&self, value: Duration) {
        self.world
            .borrow_mut()
            .topology
            .set_max_message_latency(value);
    }

    pub fn set_link_max_message_latency(
        &self,
        a: impl ToSocketAddr,
        b: impl ToSocketAddr,
        value: Duration,
    ) {
        let mut world = self.world.borrow_mut();
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.topology.set_link_max_message_latency(a, b, value);
    }

    /// Set the message latency distribution curve.
    ///
    /// Message latency follows an exponential distribution curve. The `value`
    /// is the lambda argument to the probability function.
    pub fn set_message_latency_curve(&self, value: f64) {
        self.world
            .borrow_mut()
            .topology
            .set_message_latency_curve(value);
    }

    pub fn set_fail_rate(&mut self, value: f64) {
        self.world.borrow_mut().topology.set_fail_rate(value);
    }

    pub fn set_link_fail_rate(&mut self, a: impl ToSocketAddr, b: impl ToSocketAddr, value: f64) {
        let mut world = self.world.borrow_mut();
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.topology.set_link_fail_rate(a, b, value);
    }

    /// Run the simulation until all clients finish.
    ///
    /// For each runtime, we [`Rt::tick`] it forward, which allows time to
    /// advance just a little bit. In this way, only one runtime is ever active.
    /// The turmoil APIs (such as [`crate::io::send`]) operate on the active
    /// host, and so we remember which host is active before yielding to user
    /// code.
    pub fn run(&self) {
        let mut elapsed = Duration::default();
        let tick = self.config.tick;

        loop {
            let mut is_finished = true;

            for (&addr, rt) in self.rts.iter() {
                // Set the current host (see method docs)
                self.world.borrow_mut().current = Some(addr);

                let now = World::enter(&self.world, || rt.tick(tick));

                // Unset the current host
                self.world.borrow_mut().current = None;

                let mut world = self.world.borrow_mut();
                world.tick(addr, now);

                if let Role::Client { handle, .. } = rt {
                    is_finished = is_finished && handle.is_finished();
                }
            }

            if is_finished {
                return;
            }

            elapsed += tick;

            if elapsed > self.config.duration {
                panic!("Ran for {:?} without completing", self.config.duration);
            }
        }
    }
}
