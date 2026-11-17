use crate::config::Config;
use crate::host::HostTimer;
use crate::ip::IpVersionAddrIter;
use crate::top::{endpoints, Summary};
use crate::{
    config, for_pairs, Dns, Host, Result as TurmoilResult, ToIpAddr, ToIpAddrs, Topology,
    TRACING_TARGET,
};

use indexmap::IndexMap;
use rand::RngCore;
use scoped_tls::scoped_thread_local;
use std::cell::RefCell;
use std::net::IpAddr;
use std::time::Duration;
use turmoil_net::{
    HostId, KernelConfig, Net, NetstatState, Packet, TcpFlags, TcpSegment, Transport, Verdict,
};

/// Tracks all the state for the simulated world.
pub(crate) struct World {
    /// Tracks all individual hosts
    pub(crate) hosts: IndexMap<IpAddr, Host>,

    /// Reverse of [`Host::net`].
    addrs: IndexMap<HostId, IpAddr>,

    /// Tracks how each host is connected to each other.
    pub(crate) topology: Topology,

    /// Maps hostnames to ip addresses.
    pub(crate) dns: Dns,

    /// Every host's network stack. Installed for `turmoil_net`'s
    /// socket shims while host software runs (see [`World::enter`]);
    /// a placeholder sits here meanwhile.
    net: Net,

    /// Reused buffer for packets leaving a host.
    egress: Vec<Packet>,

    /// If set, this is the current host being executed.
    pub(crate) current: Option<IpAddr>,

    /// Random number generator used for all decisions. To make execution
    /// deterministic, reuse the same seed.
    pub(crate) rng: Box<dyn RngCore>,
}

scoped_thread_local!(static CURRENT: RefCell<World>);

impl World {
    /// Initialize a new world.
    pub(crate) fn new(
        link: config::Link,
        rng: Box<dyn RngCore>,
        addrs: IpVersionAddrIter,
        kernel: KernelConfig,
    ) -> World {
        let mut net = Net::with_config(kernel);
        // Hosts are registered by IP; turmoil owns the names.
        net.set_resolver(|name| World::try_current(|world| world.dns.get(name)).ok()?);

        World {
            hosts: IndexMap::new(),
            addrs: IndexMap::new(),
            topology: Topology::new(link),
            dns: Dns::new(addrs),
            net,
            egress: Vec::new(),
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

    /// Run `f` with `world` as the current world, and its network
    /// installed for the socket shims with the current host's stack
    /// selected.
    pub(crate) fn enter<R>(world: &RefCell<World>, f: impl FnOnce() -> R) -> R {
        let _net = NetScope::enter(world);
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

    /// Connected TCP streams on the current host. Must run inside
    /// [`World::enter`].
    pub(crate) fn est_tcp_streams(&mut self) -> usize {
        est_tcp_streams(self.current_host().addr)
    }

    /// Connected TCP streams on `addr`. Must run inside
    /// [`World::enter`].
    pub(crate) fn est_tcp_streams_on(&mut self, addr: impl ToIpAddr) -> usize {
        est_tcp_streams(self.dns.lookup(addr))
    }

    /// Register a new host with the simulation.
    pub(crate) fn register(
        &mut self,
        addr: IpAddr,
        nodename: &str,
        timer: HostTimer,
        #[cfg_attr(not(feature = "unstable-fs"), allow(unused_variables))] config: &Config,
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

        let net = self.net.add_host(addr);
        self.addrs.insert(net, addr);

        // Initialize host state
        #[cfg(feature = "unstable-fs")]
        let fs_seed = self.rng.next_u64();
        #[cfg(feature = "unstable-fs")]
        self.hosts.insert(
            addr,
            Host::new(addr, net, timer, config.fs.clone(), fs_seed),
        );
        #[cfg(not(feature = "unstable-fs"))]
        self.hosts.insert(addr, Host::new(addr, net, timer));
    }

    /// Hand the packets that reached `addr` to its stack. A crashed
    /// host is powered off, so they are lost.
    pub(crate) fn deliver(&mut self, addr: IpAddr) {
        let packets = self.topology.take_deliverable(addr);
        let host = &self.hosts[&addr];
        for packet in packets {
            let (src, dst) = endpoints(&packet);
            if host.crashed {
                tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Summary(&packet), "Drop (host crashed)");
                continue;
            }
            tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Summary(&packet), "Delivered");
            self.net.deliver_to(host.net, packet);
        }
    }

    /// Put the packets `addr`'s stack has produced on the network.
    pub(crate) fn egress(&mut self, addr: IpAddr) {
        let host = self.hosts[&addr].net;
        let mut egress = std::mem::take(&mut self.egress);
        self.net.egress(host, &mut egress);
        for packet in egress.drain(..) {
            self.route(addr, host, packet);
        }
        self.egress = egress;
    }

    /// Send `packet` from host `from` to every receiver over its link.
    /// `turmoil_net` rules installed in the simulation run first.
    fn route(&mut self, from: IpAddr, from_id: HostId, packet: Packet) {
        let (src, dst) = endpoints(&packet);
        let delay = match self.net.evaluate(&packet) {
            Verdict::Drop => {
                tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Summary(&packet), "Drop (rule)");
                return;
            }
            Verdict::Deliver(delay) => Some(delay),
            Verdict::Pass => None,
        };

        let mut receivers = self.net.receivers(&packet);
        // A loopback-bound cast has no globally owned source IP, so
        // receivers() cannot identify its sender. Local copies were
        // already handled by the kernel; never create a self-link.
        receivers.retain(|&receiver| receiver != from_id);
        if receivers.is_empty() {
            // No host owns the destination. Refuse TCP right away, as
            // an unreachable network would, rather than leaving
            // connects to retransmit into the void.
            tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Summary(&packet), "Drop (no route)");
            if let Some(rst) = refuse(&packet) {
                self.net.deliver_to(from_id, rst);
            }
            return;
        }

        let (last, rest) = receivers.split_last().expect("non-empty");
        for to in rest {
            let to = self.addrs[to];
            self.topology
                .enqueue_message(&mut self.rng, from, to, packet.clone(), delay);
        }
        let to = self.addrs[last];
        self.topology
            .enqueue_message(&mut self.rng, from, to, packet, delay);
    }

    /// Crash `addr`'s network stack: every socket vanishes without a
    /// FIN or RST.
    pub(crate) fn crash_net(&mut self, addr: IpAddr) {
        let host = self.hosts.get_mut(&addr).expect("missing host");
        host.crashed = true;
        self.net.reset_host(host.net);
    }

    /// Power `addr` back on after a crash.
    pub(crate) fn revive(&mut self, addr: IpAddr) {
        self.hosts.get_mut(&addr).expect("missing host").crashed = false;
    }

    pub(crate) fn refresh_retx_threshold(&mut self, config: &Config) {
        self.net
            .set_retx_threshold(config.retx_threshold(self.topology.max_message_latency()));
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

/// Connected streams: established, or half-closed by the peer. A
/// socket the app dropped lingers through the close handshake, but no
/// longer counts.
fn est_tcp_streams(addr: IpAddr) -> usize {
    turmoil_net::netstat(addr)
        .entries
        .iter()
        .filter(|e| {
            matches!(
                e.state,
                Some(NetstatState::Established | NetstatState::CloseWait)
            )
        })
        .count()
}

/// The RST a router-less network answers with: a TCP segment to an
/// unknown host is refused, unless it is itself a RST.
fn refuse(packet: &Packet) -> Option<Packet> {
    let Transport::Tcp(s) = &packet.payload else {
        return None;
    };
    if s.flags.rst {
        return None;
    }
    // RFC 9293 §3.10.7.1: reflect the ACK, or acknowledge the segment.
    let (seq, ack, ack_flag) = if s.flags.ack {
        (s.ack, 0, false)
    } else {
        let len = s.payload.len() as u32 + u32::from(s.flags.syn) + u32::from(s.flags.fin);
        (0, s.seq.wrapping_add(len), true)
    };
    Some(Packet {
        src: packet.dst,
        dst: packet.src,
        ttl: 64,
        payload: Transport::Tcp(TcpSegment {
            src_port: s.dst_port,
            dst_port: s.src_port,
            seq,
            ack,
            flags: TcpFlags {
                rst: true,
                ack: ack_flag,
                ..TcpFlags::default()
            },
            window: 0,
            payload: Default::default(),
        }),
    })
}

/// Installs the world's network for the socket shims, and parks it
/// back in the world on drop — including when host software panics.
struct NetScope<'a> {
    world: &'a RefCell<World>,
    guard: Option<turmoil_net::EnterGuard>,
}

impl<'a> NetScope<'a> {
    fn enter(world: &'a RefCell<World>) -> Self {
        let mut w = world.borrow_mut();
        let current = w.current.map(|addr| w.hosts[&addr].net);
        let guard = std::mem::take(&mut w.net).enter();
        if let Some(id) = current {
            guard.set_current(id);
        }
        Self {
            world,
            guard: Some(guard),
        }
    }
}

impl Drop for NetScope<'_> {
    fn drop(&mut self) {
        let net = self.guard.take().expect("entered").exit();
        self.world.borrow_mut().net = net;
    }
}
