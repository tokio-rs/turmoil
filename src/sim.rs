use rand::seq::SliceRandom;
use std::cell::RefCell;
use std::future::Future;
use std::net::IpAddr;
use std::ops::DerefMut;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use indexmap::IndexMap;
use tokio::time::Duration;
use tracing::Level;

use crate::host::HostTimer;
use crate::{
    for_pairs, rt, Config, LinksIter, Result, Rt, ToIpAddr, ToIpAddrs, World, TRACING_TARGET,
};

/// A handle for interacting with the simulation.
pub struct Sim<'a> {
    /// Simulation configuration
    config: Config,

    /// Tracks the simulated world state
    ///
    /// This is what is stored in the thread-local
    world: RefCell<World>,

    /// Per simulated host runtimes
    rts: IndexMap<IpAddr, Rt<'a>>,

    /// Simulation duration since unix epoch. Set when the simulation is
    /// created.
    since_epoch: Duration,

    /// Simulation elapsed time
    elapsed: Duration,

    steps: usize,
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
            steps: 1, // bumped after each step
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
        let addr = self.lookup(addr);
        let nodename: Arc<str> = self
            .world
            .borrow_mut()
            .dns
            .reverse(addr)
            .map(str::to_string)
            .unwrap_or_else(|| addr.to_string())
            .into();

        {
            let world = RefCell::get_mut(&mut self.world);

            // Register host state with the world
            world.register(addr, &nodename, HostTimer::new(self.elapsed), &self.config);
        }

        let rt = World::enter(&self.world, || {
            Rt::client(nodename, client, self.config.enable_tokio_io)
        });

        self.rts.insert(addr, rt);
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
        let addr = self.lookup(addr);
        let nodename: Arc<str> = self
            .world
            .borrow_mut()
            .dns
            .reverse(addr)
            .map(str::to_string)
            .unwrap_or_else(|| addr.to_string())
            .into();

        {
            let world = RefCell::get_mut(&mut self.world);

            // Register host state with the world
            world.register(addr, &nodename, HostTimer::new(self.elapsed), &self.config);
        }

        let rt = World::enter(&self.world, || {
            Rt::host(nodename, host, self.config.enable_tokio_io)
        });

        self.rts.insert(addr, rt);
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
    fn run_with_hosts(&mut self, addrs: impl ToIpAddrs, mut f: impl FnMut(IpAddr, &mut Rt)) {
        let hosts = self.world.borrow_mut().lookup_many(addrs);
        for h in hosts {
            let rt = self.rts.get_mut(&h).expect("missing host");

            self.world.borrow_mut().current = Some(h);

            World::enter(&self.world, || f(h, rt));
        }

        self.world.borrow_mut().current = None;
    }

    /// Check whether a host has software running.
    pub fn is_host_running(&mut self, addr: impl ToIpAddr) -> bool {
        let host = self.world.borrow_mut().lookup(addr);

        self.rts
            .get(&host)
            .expect("missing host")
            .is_software_running()
    }

    /// Lookup an IP address by host name.
    pub fn lookup(&self, addr: impl ToIpAddr) -> IpAddr {
        self.world.borrow_mut().lookup(addr)
    }

    /// Perform a reverse DNS lookup, returning the hostname if the entry
    /// exists.
    pub fn reverse_lookup(&self, addr: IpAddr) -> Option<String> {
        self.world
            .borrow()
            .reverse_lookup(addr)
            .map(|h| h.to_owned())
    }

    /// Hold messages between two hosts, or sets of hosts, until [`release`](crate::release) is
    /// called.
    pub fn hold(&self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let mut world = self.world.borrow_mut();
        world.hold_many(a, b);
    }

    /// Repair the connection between two hosts, or sets of hosts, resulting in
    /// messages to be delivered.
    pub fn repair(&self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let mut world = self.world.borrow_mut();
        world.repair_many(a, b);
    }

    /// Repair the connection between two hosts, or sets of hosts, removing
    /// the effect of a previous [`Self::partition_oneway`].
    ///
    /// Combining this feature with [`Self::hold`] is presently not supported.
    pub fn repair_oneway(&self, from: impl ToIpAddrs, to: impl ToIpAddrs) {
        let mut world = self.world.borrow_mut();
        world.repair_oneway_many(from, to);
    }

    /// The opposite of [`hold`](crate::hold). All held messages are immediately delivered.
    pub fn release(&self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let mut world = self.world.borrow_mut();
        world.release_many(a, b);
    }

    /// Partition two hosts, or sets of hosts, resulting in all messages sent
    /// between them to be dropped.
    pub fn partition(&self, a: impl ToIpAddrs, b: impl ToIpAddrs) {
        let mut world = self.world.borrow_mut();
        world.partition_many(a, b);
    }

    /// Partition two hosts, or sets of hosts, such that messages can not be sent
    /// from 'from' to 'to', while not affecting the ability for them to be delivered
    /// in the other direction.
    ///
    /// Partitioning first from->to, then to->from, will stop all messages, similarly to
    /// a single call to [`Self::partition`].
    ///
    /// Combining this feature with [`Self::hold`] is presently not supported.
    pub fn partition_oneway(&self, from: impl ToIpAddrs, to: impl ToIpAddrs) {
        let mut world = self.world.borrow_mut();
        world.partition_oneway_many(from, to);
    }

    /// Resolve host names for an [`IpAddr`] pair.
    ///
    /// Useful when interacting with network [links](#method.links).
    pub fn reverse_lookup_pair(&self, pair: (IpAddr, IpAddr)) -> (String, String) {
        let world = self.world.borrow();

        (
            world
                .dns
                .reverse(pair.0)
                .expect("no hostname found for ip address")
                .to_owned(),
            world
                .dns
                .reverse(pair.1)
                .expect("no hostname found for ip address")
                .to_owned(),
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

    /// Run the simulation until all client hosts have completed.
    ///
    /// Executes a simple event loop that calls [step](#method.step) each
    /// iteration, returning early if any host software errors.
    pub fn run(&mut self) -> Result {
        // check if we have any clients
        if !self
            .rts
            .iter()
            .any(|(_, rt)| matches!(rt.kind, rt::Kind::Client))
        {
            tracing::info!(target: TRACING_TARGET, "No client hosts registered, exiting simulation");
            return Ok(());
        }

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
        tracing::trace!(target: TRACING_TARGET, "step {}", self.steps);

        let tick = self.config.tick;
        let mut is_finished = true;

        // Tick the networking, processing messages. This is done before
        // ticking any other runtime, as they might be waiting on network
        // IO. (It also might be waiting on something else, such as time.)
        self.world.borrow_mut().topology.tick_by(tick);

        // Tick each host runtimes with running software. If the software
        // completes, extract the result and return early if an error is
        // encountered.

        let (mut running, stopped): (Vec<_>, Vec<_>) = self
            .rts
            .iter_mut()
            .partition(|(_, rt)| rt.is_software_running());
        if self.config.random_node_order {
            running.shuffle(&mut self.world.borrow_mut().rng);
        }

        for (&addr, rt) in running {
            let _span_guard = tracing::span!(Level::INFO, "node", name = &*rt.nodename,).entered();
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

                world.current_host_mut().timer.now(rt.now());
            }

            let is_software_finished = World::enter(&self.world, || rt.tick(tick))?;

            if rt.is_client() {
                is_finished = is_finished && is_software_finished;
            }

            // Unset the current host
            let mut world = self.world.borrow_mut();
            world.current = None;

            world.tick(addr, tick);
        }

        // Tick the nodes that are not actively running (i.e., crashed) to ensure their clock keeps up
        // with the rest of the simulation when they are restarted (bounced).
        for (&addr, _rt) in stopped {
            let mut world = self.world.borrow_mut();
            world.tick(addr, tick);
        }

        self.elapsed += tick;
        self.steps += 1;

        if self.elapsed > self.config.duration && !is_finished {
            return Err(format!(
                "Ran for duration: {:?} steps: {} without completing",
                self.config.duration, self.steps,
            ))?;
        }

        Ok(is_finished)
    }
}

#[cfg(test)]
mod test {
    use rand::Rng;
    use std::future;
    use std::{
        net::{IpAddr, Ipv4Addr},
        rc::Rc,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::Semaphore,
        time::Instant,
    };

    use crate::net::UdpSocket;
    use crate::{
        elapsed, hold,
        net::{TcpListener, TcpStream},
        sim_elapsed, Builder, Result, Sim, World,
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
            // For hosts/clients started before the first `sim.run()` call, the
            // `elapsed` and `sim_elapsed` time will be identical.
            assert_eq!(duration, sim_elapsed().unwrap());

            Ok(())
        });

        sim.client("c2", async move {
            tokio::time::sleep(duration).await;
            assert_eq!(duration, elapsed());
            assert_eq!(duration, sim_elapsed().unwrap());

            Ok(())
        });

        sim.run()?;

        // sleep duration plus one tick to complete
        assert_eq!(duration + tick, sim.elapsed());

        let start = sim.elapsed();
        sim.client("c3", async move {
            assert_eq!(Duration::ZERO, elapsed());
            // Note that sim_elapsed is total simulation time while elapsed is
            // still zero for this newly created host.
            assert_eq!(duration + tick, sim_elapsed().unwrap());
            tokio::time::sleep(duration).await;
            assert_eq!(duration, elapsed());
            assert_eq!(duration + tick + duration, sim_elapsed().unwrap());

            Ok(())
        });

        sim.run()?;

        // Client "c3" takes one sleep duration plus one tick to complete
        assert_eq!(duration + tick, sim.elapsed() - start);

        Ok(())
    }

    /// This is a regression test to ensure it is safe to call sim_elapsed
    /// if current world of host is not set.
    #[test]
    fn sim_elapsed_time() -> Result {
        // Safe to call outside of simution while there
        // is no current world set
        assert!(sim_elapsed().is_none());

        let sim = Builder::new().build();
        // Safe to call while there is no current host set
        World::enter(&sim.world, || assert!(sim_elapsed().is_none()));

        Ok(())
    }

    #[test]
    fn hold_release_peers() -> Result {
        let global = Duration::from_millis(2);

        let mut sim = Builder::new()
            .min_message_latency(global)
            .max_message_latency(global)
            .build();

        sim.host("server", || async {
            let listener = TcpListener::bind((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1234)).await?;

            while let Ok((mut s, _)) = listener.accept().await {
                assert!(s.write_u8(42).await.is_ok());
            }

            Ok(())
        });

        sim.client("client", async move {
            let mut s = TcpStream::connect("server:1234").await?;

            s.read_u8().await?;

            Ok(())
        });

        sim.hold("server", "client");

        // Verify that msg is not delivered.
        sim.step()?;

        sim.links(|l| {
            assert!(l.count() == 1);
        });

        // Verify that msg is still not delivered.
        sim.step()?;

        sim.release("server", "client");

        sim.run()?;

        Ok(())
    }

    #[test]
    fn partition_peers() -> Result {
        let global = Duration::from_millis(2);

        let mut sim = Builder::new()
            .min_message_latency(global)
            .max_message_latency(global)
            .build();

        sim.host("server", || async {
            let _listener = TcpListener::bind((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1234)).await?;

            Ok(())
        });

        sim.client("client", async move {
            // Peers are partitioned. TCP setup should fail.
            let _ = TcpStream::connect("server:1234").await.unwrap_err();

            Ok(())
        });

        sim.partition("server", "client");

        sim.run()?;

        Ok(())
    }

    struct Expectation {
        expect_a_receive: bool,
        expect_b_receive: bool,
    }

    #[derive(Debug)]
    enum Action {
        Partition,
        PartitionOnewayAB,
        PartitionOnewayBA,
        RepairOnewayAB,
        RepairOnewayBA,
        Repair,
    }

    fn run_with_partitioning(
        host_a: &'static str,
        host_b: &'static str,
        mut partitioning: impl FnMut(&mut Sim) -> Expectation,
    ) -> Result {
        let global = Duration::from_millis(1);

        let mut sim = Builder::new()
            .min_message_latency(global)
            .max_message_latency(global)
            .build();

        let a_did_receive = Arc::new(Mutex::new(None));
        let b_did_receive = Arc::new(Mutex::new(None));

        let make_a = |sim: &mut Sim| {
            sim.client(host_a, {
                let a_did_receive = Arc::clone(&a_did_receive);
                async move {
                    let udp_socket =
                        UdpSocket::bind((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1234)).await?;
                    udp_socket
                        .send_to(&[42], format!("{host_b}:1234"))
                        .await
                        .expect("sending packet should appear to work, even if partitioned");

                    *a_did_receive.lock().unwrap() = Some(matches!(
                        tokio::time::timeout(
                            Duration::from_secs(1),
                            udp_socket.recv_from(&mut [0])
                        )
                        .await,
                        Ok(Ok(_))
                    ));

                    Ok(())
                }
            })
        };

        let make_b = |sim: &mut Sim| {
            sim.client(host_b, {
                let b_did_receive = Arc::clone(&b_did_receive);
                async move {
                    let udp_socket =
                        UdpSocket::bind((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1234)).await?;
                    udp_socket
                        .send_to(&[42], format!("{host_a}:1234"))
                        .await
                        .expect("sending packet should work");

                    *b_did_receive.lock().unwrap() = Some(matches!(
                        tokio::time::timeout(
                            Duration::from_secs(1),
                            udp_socket.recv_from(&mut [0])
                        )
                        .await,
                        Ok(Ok(_))
                    ));

                    Ok(())
                }
            })
        };

        let construct_a_first = sim.world.borrow_mut().rng.gen_bool(0.5);
        if construct_a_first {
            make_a(&mut sim);
            make_b(&mut sim);
        } else {
            make_b(&mut sim);
            make_a(&mut sim);
        }

        let Expectation {
            expect_a_receive,
            expect_b_receive,
        } = partitioning(&mut sim);
        sim.run()?;

        assert_eq!(*a_did_receive.lock().unwrap(), Some(expect_a_receive));
        assert_eq!(*b_did_receive.lock().unwrap(), Some(expect_b_receive));

        Ok(())
    }

    #[test]
    fn partition_peers_oneway() -> Result {
        run_with_partitioning("a", "b", |sim: &mut Sim| {
            sim.partition_oneway("a", "b");
            Expectation {
                expect_a_receive: true,
                expect_b_receive: false,
            }
        })
    }

    #[test]
    fn partition_peers_oneway_many_cases() -> Result {
        const HOST_A: &str = "a";
        const HOST_B: &str = "b";

        // Test all permutations of the above 6 actions.

        fn run_with_actions(actions: &[Action]) -> Result {
            run_with_partitioning(HOST_A, HOST_B, |sim: &mut Sim| {
                let mut expect_a_receive = true;
                let mut expect_b_receive = true;
                for action in actions {
                    match action {
                        Action::Partition => {
                            sim.partition(HOST_A, HOST_B);
                            expect_a_receive = false;
                            expect_b_receive = false;
                        }
                        Action::PartitionOnewayAB => {
                            sim.partition_oneway(HOST_A, HOST_B);
                            expect_b_receive = false;
                        }
                        Action::PartitionOnewayBA => {
                            sim.partition_oneway(HOST_B, HOST_A);
                            expect_a_receive = false;
                        }
                        Action::RepairOnewayAB => {
                            sim.repair_oneway(HOST_A, HOST_B);
                            expect_b_receive = true;
                        }
                        Action::RepairOnewayBA => {
                            sim.repair_oneway(HOST_B, HOST_A);
                            expect_a_receive = true;
                        }
                        Action::Repair => {
                            sim.repair(HOST_A, HOST_B);
                            expect_a_receive = true;
                            expect_b_receive = true;
                        }
                    }
                }
                Expectation {
                    expect_a_receive,
                    expect_b_receive,
                }
            })?;
            Ok(())
        }

        run_with_actions(&[Action::PartitionOnewayAB])?;
        run_with_actions(&[Action::PartitionOnewayBA])?;
        run_with_actions(&[Action::Partition, Action::RepairOnewayAB])?;
        run_with_actions(&[Action::Partition, Action::RepairOnewayBA])?;
        run_with_actions(&[Action::PartitionOnewayAB, Action::Repair])?;
        run_with_actions(&[Action::PartitionOnewayBA, Action::Repair])?;
        run_with_actions(&[Action::PartitionOnewayBA, Action::RepairOnewayAB])?;
        run_with_actions(&[Action::PartitionOnewayAB, Action::PartitionOnewayBA])?;
        run_with_actions(&[
            Action::Partition,
            Action::RepairOnewayAB,
            Action::RepairOnewayBA,
        ])?;

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

        sim.step()?;
        assert_eq!(tick_ms - 1, actual.load(Ordering::SeqCst));

        sim.bounce("host");
        sim.step()?;
        assert_eq!((tick_ms * 2) - 1, actual.load(Ordering::SeqCst));

        Ok(())
    }

    #[test]
    fn elapsed_time_across_crashes() -> Result {
        let tick_ms = 5;
        let mut sim = Builder::new()
            .tick_duration(Duration::from_millis(tick_ms))
            .build();

        let clock_1 = Arc::new(AtomicU64::new(0));
        let clock_1_moved = clock_1.clone();

        sim.host("host1", move || {
            let clock = clock_1_moved.clone();
            async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    clock.store(sim_elapsed().unwrap().as_millis() as u64, Ordering::SeqCst);
                }
            }
        });

        // Crashing host 1
        sim.crash("host1");
        sim.step()?;
        // After bouncing host 1, host's clock must be synced.
        sim.bounce("host1");
        sim.step()?;
        assert_eq!(
            2 * tick_ms - 1,
            clock_1.load(Ordering::SeqCst),
            "Host 1 should have caught up"
        );

        Ok(())
    }

    #[test]
    fn host_finishes_with_error() {
        let mut sim = Builder::new().build();

        sim.host("host", || async {
            Err("Host software finished unexpectedly")?
        });

        assert!(sim.step().is_err());
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
    fn is_host_running() -> Result {
        let mut sim = Builder::new().build();

        sim.client("client", async { future::pending().await });
        sim.host("host", || async { future::pending().await });

        assert!(!sim.step()?);

        assert!(sim.is_host_running("client"));
        assert!(sim.is_host_running("host"));

        sim.crash("host");
        assert!(!sim.is_host_running("host"));

        Ok(())
    }

    #[test]
    #[cfg(feature = "regex")]
    fn host_scan() -> Result {
        let mut sim = Builder::new().build();

        let how_many = 3;
        for i in 0..how_many {
            sim.host(format!("host-{i}"), || async { future::pending().await })
        }

        let mut ips = sim.lookup_many(regex::Regex::new(".*")?);
        ips.sort();

        assert_eq!(how_many, ips.len());

        for (i, ip) in ips.iter().enumerate() {
            assert_eq!(
                format!("host-{i}"),
                sim.reverse_lookup(*ip).ok_or("Unable to resolve ip")?
            );
        }

        Ok(())
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

        sim.step()?;
        assert_eq!(count.load(Ordering::SeqCst), 3);
        sim.bounce(regex::Regex::new("host-[12]")?);
        sim.step()?;
        assert_eq!(count.load(Ordering::SeqCst), 5);

        Ok(())
    }

    #[test]
    #[cfg(feature = "regex")]
    fn hold_all() -> Result {
        let mut sim = Builder::new().build();

        sim.host("host", || async {
            let l = TcpListener::bind("0.0.0.0:1234").await?;

            loop {
                _ = l.accept().await?;
            }
        });

        sim.client("test", async {
            hold(regex::Regex::new(r".*")?, regex::Regex::new(r".*")?);

            assert!(tokio::time::timeout(
                Duration::from_millis(100),
                TcpStream::connect("host:1234")
            )
            .await
            .is_err());

            crate::release(regex::Regex::new(r".*")?, regex::Regex::new(r".*")?);

            assert!(TcpStream::connect("host:1234").await.is_ok());

            Ok(())
        });

        sim.run()
    }
}
