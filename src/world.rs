use crate::net::{Segment, SocketPair, Stream, Syn};
use crate::top::Embark;
use crate::{config, message, version, Dns, Envelope, Host, Log, Message, ToSocketAddr, Topology};

use indexmap::IndexMap;
use rand::RngCore;
use scoped_tls::scoped_thread_local;
use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::Rc;
use tokio::sync::{oneshot, Notify};
use tokio::time::Instant;

/// Tracks all the state for the simulated world.
pub(crate) struct World {
    /// Tracks all individual hosts
    hosts: IndexMap<SocketAddr, Host>,

    /// Tracks how each host is connected to each other.
    pub(crate) topology: Topology,

    /// Maps hostnames to socket addresses.
    pub(crate) dns: Dns,

    /// If set, this is the current host being executed.
    pub(crate) current: Option<SocketAddr>,

    /// Handle to the logger
    pub(crate) log: Log,

    /// Random number generator used for all decisions. To make execution
    /// determinstic, reuse the same seed.
    rng: Box<dyn RngCore>,
}

scoped_thread_local!(static CURRENT: RefCell<World>);

impl World {
    /// Initialize a new world.
    pub(crate) fn new(link: config::Link, log: Log, rng: Box<dyn RngCore>) -> World {
        World {
            hosts: IndexMap::new(),
            topology: Topology::new(link),
            dns: Dns::new(),
            current: None,
            log,
            rng,
        }
    }

    /// Run `f` on the world.
    pub(crate) fn current<R>(f: impl FnOnce(&mut World) -> R) -> R {
        CURRENT.with(|current| {
            let mut current = current.borrow_mut();
            f(&mut *current)
        })
    }

    /// Run `f` if the world is set - otherwise no-op.
    ///
    /// Used in drop paths, where the simulation may be shutting
    /// down and we don't need to do anything.
    pub(crate) fn current_if_set(f: impl FnOnce(&mut World) -> ()) {
        if CURRENT.is_set() {
            Self::current(f);
        }
    }

    pub(crate) fn enter<R>(world: &RefCell<World>, f: impl FnOnce() -> R) -> R {
        CURRENT.set(world, f)
    }

    /// Return a reference to the currently executing host.
    pub(crate) fn current_host(&self) -> &Host {
        let addr = self.current.expect("current host missing");
        self.hosts.get(&addr).expect("host missing")
    }

    /// Return a mutable reference to the currently executing host.
    pub(crate) fn current_host_mut(&mut self) -> &mut Host {
        let addr = self.current.expect("current host missing");
        self.hosts.get_mut(&addr).expect("host missing")
    }

    /// Return a reference to the host at `addr`.
    pub(crate) fn host(&self, addr: SocketAddr) -> &Host {
        self.hosts.get(&addr).expect("host missing")
    }

    pub(crate) fn lookup(&mut self, host: impl ToSocketAddr) -> SocketAddr {
        self.dns.lookup(host)
    }

    pub(crate) fn hold(&mut self, a: SocketAddr, b: SocketAddr) {
        self.topology.hold(a, b);
    }

    // TODO: Should all held packets be immediately released, or should they be
    // subject to delay and potentially broken links when the hold is removed?
    pub(crate) fn release(&mut self, a: SocketAddr, b: SocketAddr) {
        self.topology.release(a, b);
        let dst = &mut self.hosts[&b];
        dst.release(a);
    }

    pub(crate) fn partition(&mut self, a: SocketAddr, b: SocketAddr) {
        self.topology.partition(a, b);
    }

    pub(crate) fn repair(&mut self, a: SocketAddr, b: SocketAddr) {
        self.topology.repair(a, b);
    }

    /// Register a new host with the simulation.
    pub(crate) fn register(&mut self, addr: SocketAddr, epoch: Instant, notify: Rc<Notify>) {
        assert!(
            !self.hosts.contains_key(&addr),
            "already registered host for the given socket address"
        );

        // Register links between the new host and all existing hosts
        for existing in self.hosts.keys() {
            self.topology.register(*existing, addr);
        }

        // Initialize host state
        self.hosts.insert(addr, Host::new(addr, epoch, notify));
    }

    /// Initiate a new connection with `dst` from the currently executing host.
    pub(crate) fn connect(
        &mut self,
        dst: SocketAddr,
    ) -> (version::Dot, oneshot::Receiver<version::Dot>) {
        let (sender, receiver) = oneshot::channel();
        let syn = Syn { notify: sender };

        let dot = self.current_host_mut().bump();
        let elapsed = self.current_host().elapsed();

        match self.topology.embark_one(&mut self.rng, dot.host, dst) {
            it @ Embark::Delay(_) | it @ Embark::Hold => {
                let delay = if let Embark::Delay(d) = it {
                    Some(d)
                } else {
                    None
                };

                self.log.syn(&self.dns, dot, elapsed, dst, delay, false);

                self.hosts[&dst].syn(dot, delay, syn);
                (dot, receiver)
            }
            Embark::Drop => {
                self.log.syn(&self.dns, dot, elapsed, dst, None, true);

                // Let sender drop naturally to err on the receive side
                (dot, receiver)
            }
        }
    }

    /// Accept a new incoming connection on the currently executing host.
    pub(crate) fn accept(&mut self) -> Option<(Stream, SocketAddr)> {
        let ret = self.current_host_mut().accept();

        if let Some(Envelope { src, message, .. }) = ret {
            let host = self.current_host();

            let syn = message::downcast::<Syn>(message);
            let local = host.dot();
            let elapsed = host.elapsed();

            // notify the peer, returning early if they have hung up to avoid
            // host mutations
            syn.notify.send(local).ok()?;

            let pair = SocketPair { local, peer: src };
            let notify = self.current_host_mut().finish_connect(pair);

            self.log.syn_ack(&self.dns, local, elapsed, src);

            // Setup the connection on the peer. This has to happen on the
            // accept side because as soon as this returns the currently
            // executing host may use the stream, even though initiating host
            // hasn't seen the ack yet.
            self.hosts[&src.host].register_connection(pair.flip());

            return Some((Stream::new(pair, notify), src.host));
        }

        None
    }

    /// Embark a message from the currently executing host to `dst`.
    ///
    /// This begins the message's journey, queuing it on the destination inbox,
    /// but it may still be "on the network" depending on the current toplogy.
    pub(crate) fn embark(&mut self, dst: SocketAddr, message: Box<dyn Message>) {
        let host = self.current_host_mut();
        let elapsed = host.elapsed();

        // Log takes a message ref, so we bump here to emit the correct value
        // before embarking.
        let dot = host.bump();

        match self.topology.embark_one(&mut self.rng, dot.host, dst) {
            it @ Embark::Delay(_) | it @ Embark::Hold => {
                let delay = if let Embark::Delay(d) = it {
                    Some(d)
                } else {
                    None
                };

                self.log
                    .send(&self.dns, dot, elapsed, dst, delay, false, &*message);

                self.hosts[&dst].embark(dot, delay, message);
            }
            Embark::Drop => {
                self.log
                    .send(&self.dns, dot, elapsed, dst, None, true, &*message);
            }
        }
    }

    /// Embark a segment from the currently executing host to `pair`'s peer.
    pub(crate) fn embark_on(&mut self, pair: SocketPair, segment: Segment) {
        let host = self.current_host_mut();
        let dst = pair.peer.host;
        let elapsed = host.elapsed();

        // Log takes a message ref, so we bump here to emit the correct value
        // before embarking.
        let dot = host.bump();

        match self.topology.embark_one(&mut self.rng, dot.host, dst) {
            it @ Embark::Delay(_) | it @ Embark::Hold => {
                let delay = if let Embark::Delay(d) = it {
                    Some(d)
                } else {
                    None
                };

                self.log
                    .send(&self.dns, dot, elapsed, dst, delay, false, &segment);

                self.hosts[&pair.peer.host].embark_on(pair.flip(), delay, segment);
            }
            _ => unimplemented!("Drop is not supported yet"),
        }
    }

    /// Receive a message on the currently executing host.
    pub(crate) fn recv(&mut self) -> (Option<Envelope>, Rc<Notify>) {
        let addr = self.current_host().addr;
        let host = &mut self.hosts[&addr];
        let ret = host.recv();

        if let Some(Envelope { src, message, .. }) = &ret.0 {
            self.log
                .recv(&self.dns, host.dot(), host.elapsed(), *src, &**message);
        }

        ret
    }

    /// Receive a message on the currently executing host from a `peer`.
    pub(crate) fn recv_from(&mut self, peer: SocketAddr) -> (Option<Envelope>, Rc<Notify>) {
        let addr = self.current_host().addr;
        let host = &mut self.hosts[&addr];
        let ret = host.recv_from(peer);

        if let Some(Envelope { src, message, .. }) = &ret.0 {
            self.log
                .recv(&self.dns, host.dot(), host.elapsed(), *src, &**message);
        }

        ret
    }

    /// Receive a segment from `pair's peer on the currently executing host.
    pub(crate) fn recv_on(&mut self, pair: SocketPair) -> Option<Segment> {
        let host = &mut self.hosts[&pair.local.host];
        let ret = host.recv_on(pair);

        if let Some(seg) = &ret {
            self.log
                .recv(&self.dns, host.dot(), host.elapsed(), pair.peer, seg);
        }

        ret
    }

    /// Tick the host at `addr` to `now`.
    pub(crate) fn tick(&mut self, addr: SocketAddr, now: Instant) {
        self.hosts.get_mut(&addr).expect("missing host").tick(now);
    }
}
