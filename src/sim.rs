use crate::{Config, Result, Role, Rt, ToIpAddr, World};

use indexmap::IndexMap;
use std::cell::RefCell;
use std::future::Future;
use std::net::IpAddr;
use std::ops::DerefMut;
use std::time::UNIX_EPOCH;
use tokio::time::Duration;

/// Network simulation
pub struct Sim<'a> {
    /// Simulation configuration
    config: Config,

    /// Tracks the simulated world state
    ///
    /// This is what is stored in the thread-local
    world: RefCell<World>,

    /// Per simulated host runtimes
    rts: IndexMap<IpAddr, Role<'a>>,

    /// Simulation duration since unix epoch. Set when the simulation is
    /// created.
    since_epoch: Duration,

    /// Simulation elapsed time
    elapsed: Duration,
}

impl<'a> Sim<'a> {
    pub(crate) fn new(config: Config, world: World) -> Self {
        let since_epoch = config
            .epoch
            .duration_since(UNIX_EPOCH)
            .expect("now must be >= UNIX_EPOCH");

        Self {
            config,
            world: RefCell::new(world),
            rts: IndexMap::new(),
            since_epoch,
            elapsed: Duration::ZERO,
        }
    }

    /// How much logical time has elapsed since the simulation started.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// The logical duration from [`UNIX_EPOCH`] until now.
    ///
    /// On creation the simulation picks a `SystemTime` and calculates the
    /// duration since the epoch. Each `run()` invocation moves logical time
    /// forward the configured tick duration.
    pub fn since_epoch(&self) -> Duration {
        self.since_epoch + self.elapsed
    }

    /// Register a client with the simulation.
    pub fn client<F>(&mut self, addr: impl ToIpAddr, client: F)
    where
        F: Future<Output = Result> + 'static,
    {
        let rt = Rt::new();
        let addr = self.lookup(addr);

        {
            let world = RefCell::get_mut(&mut self.world);

            // Register host state with the world
            world.register(addr);
        }

        let handle = World::enter(&self.world, || rt.with(|| tokio::task::spawn_local(client)));

        self.rts.insert(addr, Role::client(rt, handle));
    }

    /// Register a host with the simulation.
    ///
    /// This method takes a `Fn` that builds a future, as opposed to
    /// [`Sim::client`] which just takes a future. The reason for this is we
    /// might restart the host, and so need to be able to call the future
    /// multiple times.
    pub fn host<F, Fut>(&mut self, addr: impl ToIpAddr, host: F)
    where
        F: Fn() -> Fut + 'a,
        Fut: Future<Output = Result> + 'static,
    {
        let rt = Rt::new();
        let addr = self.lookup(addr);

        {
            let world = RefCell::get_mut(&mut self.world);

            // Register host state with the world
            world.register(addr);
        }

        let handle = World::enter(&self.world, || rt.with(|| tokio::task::spawn_local(host())));

        self.rts.insert(addr, Role::simulated(rt, host, handle));
    }

    /// Crash a host. Nothing will be running on the host after this method. You
    /// can use [`Sim::bounce`] to start the host up again.
    pub fn crash(&mut self, addr: impl ToIpAddr) {
        self.run_with_host(addr, |rt| match rt {
            Role::Client { .. } => panic!("can only bounce hosts, not clients"),
            Role::Simulated { rt, .. } => {
                rt.cancel_tasks();
            }
        });
    }

    /// Bounce a host. The software is restarted.
    pub fn bounce(&mut self, addr: impl ToIpAddr) {
        self.run_with_host(addr, |rt| match rt {
            Role::Client { .. } => panic!("can only bounce hosts, not clients"),
            Role::Simulated {
                rt,
                software,
                handle,
            } => {
                rt.cancel_tasks();
                *handle = rt.with(|| tokio::task::spawn_local(software()));
            }
        });
    }

    // Run `f` with the host at `addr` set on the world.
    fn run_with_host(&mut self, addr: impl ToIpAddr, f: impl FnOnce(&mut Role)) {
        let h = self.world.borrow_mut().lookup(addr);
        let rt = self.rts.get_mut(&h).expect("missing host");

        self.world.borrow_mut().current = Some(h);

        World::enter(&self.world, || f(rt));

        self.world.borrow_mut().current = None;
    }

    /// Lookup an ip address by host name.
    pub fn lookup(&self, addr: impl ToIpAddr) -> IpAddr {
        self.world.borrow_mut().lookup(addr)
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
        a: impl ToIpAddr,
        b: impl ToIpAddr,
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

    pub fn set_link_fail_rate(&mut self, a: impl ToIpAddr, b: impl ToIpAddr, value: f64) {
        let mut world = self.world.borrow_mut();
        let a = world.lookup(a);
        let b = world.lookup(b);

        world.topology.set_link_fail_rate(a, b, value);
    }

    /// Run the simulation until all clients finish.
    ///
    /// For each runtime, we [`Rt::tick`] it forward, which allows time to
    /// advance just a little bit. In this way, only one runtime is ever active.
    /// The turmoil APIs operate on the active host, and so we remember which
    /// host is active before yielding to user code.
    ///
    /// If any client errors, the simulation returns early with that Error.
    pub fn run(&mut self) -> Result {
        let tick = self.config.tick;

        loop {
            let mut is_finished = true;
            let mut finished = vec![];

            // Tick the networking, processing messages. This is done before
            // ticking any other runtime, as they might be waiting on network
            // IO. (It also might be waiting on something else, such as time.)
            self.world.borrow_mut().topology.tick_by(tick);

            for (&addr, rt) in self.rts.iter() {
                {
                    let mut world = self.world.borrow_mut();
                    // We need to move deliverable messages off the network and
                    // into the dst host. This requires two mutable borrows.
                    let World {
                        rng,
                        topology,
                        hosts,
                        ..
                    } = world.deref_mut();
                    topology.deliver_messages(rng, hosts.get_mut(&addr).expect("missing host"));

                    // Set the current host (see method docs)
                    world.current = Some(addr);

                    world.current_host_mut().now(rt.now());
                }

                World::enter(&self.world, || rt.tick(tick));

                // Unset the current host
                let mut world = self.world.borrow_mut();
                world.current = None;

                world.tick(addr, tick);

                match rt {
                    Role::Client { handle, .. } => {
                        if handle.is_finished() {
                            finished.push(addr);
                        }
                        is_finished = is_finished && handle.is_finished();
                    }
                    Role::Simulated { handle, .. } => {
                        if handle.is_finished() {
                            finished.push(addr);
                        }
                    }
                }
            }

            self.elapsed += tick;

            // Check finished clients and hosts for err results. Runtimes are removed at
            // this stage.
            for addr in finished.into_iter() {
                if let Some(role) = self.rts.remove(&addr) {
                    let (rt, handle) = match role {
                        Role::Client { rt, handle } => (rt, handle),
                        Role::Simulated { rt, handle, .. } => (rt, handle),
                    };
                    rt.block_on(handle)??;
                }
            }

            if is_finished {
                return Ok(());
            }

            if self.elapsed > self.config.duration {
                Err(format!(
                    "Ran for {:?} without completing",
                    self.config.duration
                ))?;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{
        rc::Rc,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::Duration,
    };

    use std::future;
    use tokio::sync::Semaphore;

    use crate::{elapsed, Builder, Result};

    #[test]
    fn client_error() {
        let mut sim = Builder::new().build();

        sim.client("doomed", async { Err("An Error")? });

        assert!(sim.run().is_err());
    }

    #[test]
    fn timeout() {
        let mut sim = Builder::new()
            .simulation_duration(Duration::from_millis(500))
            .build();

        sim.client("timeout", async {
            tokio::time::sleep(Duration::from_secs(1)).await;

            Ok(())
        });

        assert!(sim.run().is_err());
    }

    #[test]
    fn multiple_clients_all_finish() -> Result {
        let how_many = 3;
        let tick_ms = 10;

        // N = how_many runs, each with a different client finishing immediately
        for run in 0..how_many {
            let mut sim = Builder::new()
                .tick_duration(Duration::from_millis(tick_ms))
                .build();

            let ct = Rc::new(Semaphore::new(how_many));

            for client in 0..how_many {
                let ct = ct.clone();

                sim.client(format!("client-{}", client), async move {
                    let ms = if run == client { 0 } else { 2 * tick_ms };
                    tokio::time::sleep(Duration::from_millis(ms)).await;

                    let p = ct.acquire().await?;
                    p.forget();

                    Ok(())
                });
            }

            sim.run()?;
            assert_eq!(0, ct.available_permits());
        }

        Ok(())
    }

    /// This is a regression test that ensures host software completes when the
    /// host crashes. Before this fix we simply dropped the LocalSet, which did
    /// not ensure resources owned by spawned tasks were dropped. Now we drop
    /// and replace both the tokio Runtime and the LocalSet.
    #[test]
    fn crash_blocks_until_complete() -> Result {
        let ct = Arc::new(());

        let mut sim = Builder::new().build();

        sim.host("host", || {
            let ct = ct.clone();

            async move {
                tokio::spawn(async move {
                    let _into_task = ct;
                    future::pending::<()>().await;
                });

                future::pending().await
            }
        });

        sim.run()?;
        assert_eq!(2, Arc::strong_count(&ct));

        sim.crash("host");
        assert_eq!(1, Arc::strong_count(&ct));

        Ok(())
    }

    #[test]
    fn elapsed_time() -> Result {
        let tick = Duration::from_millis(5);
        let mut sim = Builder::new().tick_duration(tick).build();

        let duration = Duration::from_millis(500);

        sim.client("c1", async move {
            tokio::time::sleep(duration).await;
            assert_eq!(duration, elapsed());

            Ok(())
        });

        sim.client("c2", async move {
            tokio::time::sleep(duration).await;
            assert_eq!(duration, elapsed());

            Ok(())
        });

        sim.run()?;

        // sleep duration plus one tick to complete
        assert_eq!(duration + tick, sim.elapsed());

        let start = sim.elapsed();
        sim.client("c3", async move {
            assert_eq!(Duration::ZERO, elapsed());

            Ok(())
        });

        sim.run()?;

        // one tick to complete
        assert_eq!(tick, sim.elapsed() - start);

        Ok(())
    }

    #[test]
    fn elapsed_time_across_restarts() -> Result {
        let tick_ms = 5;
        let mut sim = Builder::new()
            .tick_duration(Duration::from_millis(tick_ms))
            .build();

        let clock = Arc::new(AtomicU64::new(0));
        let actual = clock.clone();

        sim.host("host", move || {
            let clock = clock.clone();

            async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    clock.store(elapsed().as_millis() as u64, Ordering::SeqCst);
                }
            }
        });

        sim.run()?;
        assert_eq!(tick_ms - 1, actual.load(Ordering::SeqCst));

        sim.bounce("host");
        sim.run()?;
        assert_eq!((tick_ms * 2) - 1, actual.load(Ordering::SeqCst));

        Ok(())
    }

    #[test]
    fn host_finishes_with_error() {
        let mut sim = Builder::new().build();

        sim.host("host", || async {
            Err("Host software finished unexpectedly")?
        });

        assert!(sim.run().is_err());
    }
}
