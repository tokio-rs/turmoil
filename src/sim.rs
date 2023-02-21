use crate::{
    for_pairs, Config, LinksIter, Result, Role, Rt, ToIpAddr, ToIpAddrs, World, TRACING_TARGET,
};

use indexmap::IndexMap;
use std::cell::RefCell;
use std::future::Future;
use std::net::IpAddr;
use std::ops::DerefMut;
use std::time::UNIX_EPOCH;
use tokio::time::Duration;

/// A handle for interacting with the simulation.
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

    /// Crashes the resolved hosts. Nothing will be running on the matched hosts
    /// after this method. You can use [`Sim::bounce`] to start the hosts up
    /// again.
    pub fn crash(&mut self, addrs: impl ToIpAddrs) {
        self.run_with_hosts(addrs, |addr, rt| {
            rt.crash();

            tracing::trace!(target: TRACING_TARGET, addr = ?addr, "Crash");
        });
    }

    /// Bounces the resolved hosts. The software is restarted.
    pub fn bounce(&mut self, addrs: impl ToIpAddrs) {
        self.run_with_hosts(addrs, |addr, rt| {
            rt.bounce();

            tracing::trace!(target: TRACING_TARGET, addr = ?addr, "Bounce");
        });
    }

    /// Run `f` with the resolved hosts at `addrs` set on the world.
    fn run_with_hosts(&mut self, addrs: impl ToIpAddrs, mut f: impl FnMut(IpAddr, &mut Role)) {
        let hosts = self.world.borrow_mut().lookup_many(addrs);
        for h in hosts {
            let rt = self.rts.get_mut(&h).expect("missing host");

            self.world.borrow_mut().current = Some(h);

            World::enter(&self.world, || f(h, rt));
        }

        self.world.borrow_mut().current = None;
    }

    /// Lookup an IP address by host name.
    pub fn lookup(&self, addr: impl ToIpAddr) -> IpAddr {
        self.world.borrow_mut().lookup(addr)
    }

    /// Resolve host names for an [`IpAddr`] pair.
    ///
    /// Useful when interacting with network [links](#method.links).
    pub fn reverse_lookup_pair(&self, pair: (IpAddr, IpAddr)) -> (String, String) {
        let world = self.world.borrow();

        (
            world.dns.reverse(pair.0).to_owned(),
            world.dns.reverse(pair.1).to_owned(),
        )
    }

    /// Lookup IP addresses for resolved hosts.
    pub fn lookup_many(&self, addr: impl ToIpAddrs) -> Vec<IpAddr> {
        self.world.borrow_mut().lookup_many(addr)
    }

    /// Set the max message latency for all links.
    pub fn set_max_message_latency(&self, value: Duration) {
        self.world
            .borrow_mut()
            .topology
            .set_max_message_latency(value);
    }

    /// Set the message latency for any links matching `a` and `b`.
    ///
    /// This sets the min and max to the same value eliminating any variance in
    /// latency.
    pub fn set_link_latency(&self, a: impl ToIpAddrs, b: impl ToIpAddrs, value: Duration) {
        let mut world = self.world.borrow_mut();
        let a = world.lookup_many(a);
        let b = world.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            world.topology.set_link_message_latency(a, b, value);
        });
    }

    /// Set the max message latency for any links matching `a` and `b`.
    pub fn set_link_max_message_latency(
        &self,
        a: impl ToIpAddrs,
        b: impl ToIpAddrs,
        value: Duration,
    ) {
        let mut world = self.world.borrow_mut();
        let a = world.lookup_many(a);
        let b = world.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            world.topology.set_link_max_message_latency(a, b, value);
        });
    }

    /// Set the message latency distribution curve for all links.
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

    pub fn set_link_fail_rate(&mut self, a: impl ToIpAddrs, b: impl ToIpAddrs, value: f64) {
        let mut world = self.world.borrow_mut();
        let a = world.lookup_many(a);
        let b = world.lookup_many(b);

        for_pairs(&a, &b, |a, b| {
            world.topology.set_link_fail_rate(a, b, value);
        });
    }

    /// Access a [`LinksIter`] to introspect inflight messages between hosts.
    pub fn links(&self, f: impl FnOnce(LinksIter)) {
        let top = &mut self.world.borrow_mut().topology;

        f(top.iter_mut())
    }

    /// Run the simulation to completion.
    ///
    /// Executes a simple event loop that calls [step](#method.step) each iteration,
    /// returning early if any host software errors.
    pub fn run(&mut self) -> Result {
        loop {
            let is_finished = self.step()?;

            if is_finished {
                return Ok(());
            }
        }
    }

    /// Step the simulation.
    ///
    /// Runs each host in the simulation a fixed duration configured by
    /// `tick_duration` in the builder.
    ///
    /// The simulated network also steps, processing in flight messages, and
    /// delivering them to their destination if appropriate.
    ///
    /// Returns whether or not all clients have completed.
    pub fn step(&mut self) -> Result<bool> {
        let tick = self.config.tick;

        let mut is_finished = true;

        // Tick the networking, processing messages. This is done before
        // ticking any other runtime, as they might be waiting on network
        // IO. (It also might be waiting on something else, such as time.)
        self.world.borrow_mut().topology.tick_by(tick);
        // Tick each hosts with running software. If the software completes,
        // extract the result and return early if an error is encountered.
        for (&addr, rt) in self.rts.iter_mut().filter(|(_, rt)| rt.is_running()) {
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

            let is_rt_finished = World::enter(&self.world, || rt.tick(tick))?;

            if rt.is_client() {
                is_finished = is_finished && is_rt_finished;
            }

            // Unset the current host
            let mut world = self.world.borrow_mut();
            world.current = None;

            world.tick(addr, tick);
        }

        self.elapsed += tick;

        if self.elapsed > self.config.duration && !is_finished {
            return Err(format!(
                "Ran for {:?} without completing",
                self.config.duration
            ))?;
        }

        Ok(is_finished)
    }
}

#[cfg(test)]
mod test {
    use std::{
        net::{IpAddr, Ipv4Addr},
        rc::Rc,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::Duration,
    };

    use std::future;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::Semaphore,
        time::Instant,
    };

    use crate::{
        elapsed, hold,
        net::{TcpListener, TcpStream},
        Builder, Result,
    };

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

                sim.client(format!("client-{client}"), async move {
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

    #[test]
    fn manual_message_delivery() -> Result {
        let mut sim = Builder::new().build();

        sim.host("a", || async {
            let l = TcpListener::bind("0.0.0.0:1234").await?;

            _ = l.accept().await?;

            Ok(())
        });

        sim.client("b", async {
            hold("a", "b");

            _ = TcpStream::connect("a:1234").await?;

            Ok(())
        });

        assert!(!sim.step()?);

        sim.links(|mut l| {
            let a_to_b = l.next().unwrap();
            a_to_b.deliver_all();
        });

        assert!(sim.step()?);

        Ok(())
    }

    /// This is a regression test that ensures JoinError::Cancelled is not
    /// propagated to the test when the host crashes, which was causing
    /// incorrect test failure.
    #[test]
    fn run_after_host_crashes() -> Result {
        let mut sim = Builder::new().build();

        sim.host("h", || async { future::pending().await });

        sim.crash("h");

        sim.run()
    }

    #[test]
    fn restart_host_after_crash() -> Result {
        let mut sim = Builder::new().build();

        let data = Arc::new(AtomicU64::new(0));
        let data_cloned = data.clone();

        sim.host("h", move || {
            let data_cloned = data_cloned.clone();
            async move {
                data_cloned.store(data_cloned.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
                Ok(())
            }
        });

        // crash and step to execute the err handling logic
        sim.crash("h");
        sim.step()?;

        // restart and step to ensure the host software runs
        sim.bounce("h");
        sim.step()?;
        // check that software actually runs
        assert_eq!(1, data.load(Ordering::SeqCst));

        Ok(())
    }

    #[test]
    fn override_link_latency() -> Result {
        let global = Duration::from_millis(2);

        let mut sim = Builder::new()
            .min_message_latency(global)
            .max_message_latency(global)
            .build();

        sim.host("server", || async {
            let listener = TcpListener::bind((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1234)).await?;

            while let Ok((mut s, _)) = listener.accept().await {
                assert!(s.write_u8(9).await.is_ok());
            }

            Ok(())
        });

        sim.client("client", async move {
            let mut s = TcpStream::connect("server:1234").await?;

            let start = Instant::now();
            s.read_u8().await?;
            assert_eq!(global, start.elapsed());

            Ok(())
        });

        sim.run()?;

        let degraded = Duration::from_millis(10);

        sim.client("client2", async move {
            let mut s = TcpStream::connect("server:1234").await?;

            let start = Instant::now();
            s.read_u8().await?;
            assert_eq!(degraded, start.elapsed());

            Ok(())
        });

        sim.set_link_latency("client2", "server", degraded);

        sim.run()
    }

    #[test]
    #[cfg(feature = "regex")]
    fn bounce_multiple_hosts_with_regex() -> Result {
        let mut sim = Builder::new().build();

        let count = Arc::new(AtomicU64::new(0));
        for i in 1..=3 {
            let count = count.clone();
            sim.host(format!("host-{}", i), move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    future::pending().await
                }
            });
        }

        sim.run()?;
        assert_eq!(count.load(Ordering::SeqCst), 3);
        sim.bounce(regex::Regex::new("host-[12]")?);
        sim.run()?;
        assert_eq!(count.load(Ordering::SeqCst), 5);

        Ok(())
    }
}
