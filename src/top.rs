use std::collections::VecDeque;
use std::io::{Error, ErrorKind, Result};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use indexmap::IndexMap;
use rand::{Rng, RngCore};
use rand_distr::{Distribution, Exp};
use tokio::time::Instant;

use crate::envelope::{Envelope, Protocol};
use crate::host::Host;
use crate::rt::Rt;
use crate::{config, TRACING_TARGET};

/// Describes the network topology.
pub(crate) struct Topology {
    config: config::Link,

    /// Specific configuration overrides between specific hosts.
    links: IndexMap<Pair, Link>,

    /// We don't use a Rt for async. Right now, we just use it to tick time
    /// forward in the same way we do it elsewhere. We'd like to represent
    /// network state with async in the future.
    rt: Rt<'static>,
}

/// This type is used as the key in the [`Topology::links`] map. See [`new`]
/// which orders the addrs, such that this type uniquely identifies the link
/// between two hosts on the network.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct Pair(IpAddr, IpAddr);

impl Pair {
    fn new(a: IpAddr, b: IpAddr) -> Pair {
        assert_ne!(a, b);

        if a < b {
            Pair(a, b)
        } else {
            Pair(b, a)
        }
    }
}

/// An iterator for the network topology, providing access to all active links
/// in the simulated network.
pub struct LinksIter<'a> {
    iter: indexmap::map::IterMut<'a, Pair, Link>,
}

/// An iterator for the link, providing access to sent messages that have not
/// yet been delivered.
pub struct LinkIter<'a> {
    a: IpAddr,
    b: IpAddr,
    now: Instant,
    iter: std::collections::vec_deque::IterMut<'a, Sent>,
}

impl LinkIter<'_> {
    /// The [`IpAddr`] pair for the link. Always ordered to uniquely identify
    /// the link.
    pub fn pair(&self) -> (IpAddr, IpAddr) {
        (self.a, self.b)
    }

    /// Schedule all messages on the link for delivery the next time the
    /// simulation steps, consuming the iterator.
    pub fn deliver_all(self) {
        for sent in self {
            sent.deliver();
        }
    }
}

/// Provides a reference to a message that is currently inflight on the network
/// from one host to another.
pub struct SentRef<'a> {
    src: SocketAddr,
    dst: SocketAddr,
    now: Instant,
    sent: &'a mut Sent,
}

impl SentRef<'_> {
    /// The (src, dst) [`SocketAddr`] pair for the message.
    pub fn pair(&self) -> (SocketAddr, SocketAddr) {
        (self.src, self.dst)
    }

    /// The message [`Protocol`].
    pub fn protocol(&self) -> &Protocol {
        &self.sent.protocol
    }

    /// Schedule the message for delivery the next time the simulation steps,
    /// consuming the item.
    pub fn deliver(self) {
        self.sent.deliver(self.now);
    }
}

impl<'a> Iterator for LinksIter<'a> {
    type Item = LinkIter<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let (pair, link) = self.iter.next()?;

        Some(LinkIter {
            a: pair.0,
            b: pair.1,
            now: link.now,
            iter: link.sent.iter_mut(),
        })
    }
}

impl<'a> Iterator for LinkIter<'a> {
    type Item = SentRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let sent = self.iter.next()?;

        Some(SentRef {
            src: sent.src,
            dst: sent.dst,
            now: self.now,
            sent,
        })
    }
}

/// A two-way link between two hosts on the network.
struct Link {
    /// The state of the link from node 'a' to node 'b'.
    /// Of the two nodes attached to a link, 'a' is the
    /// one whose ip address sorts smallest.
    state_a_b: State,
    /// The state of the link from node 'b' to node 'a'.
    /// Of the two nodes attached to a link, 'b' is the
    /// one whose ip address sorts greatest.
    state_b_a: State,

    /// Optional, per-link configuration.
    config: config::Link,

    /// Sent messages that are either scheduled for delivery in the future
    /// or are on hold.
    sent: VecDeque<Sent>,

    /// Messages that are ready to be delivered.
    deliverable: IndexMap<IpAddr, VecDeque<Envelope>>,

    /// The current network time, moved forward with [`Link::tick`].
    now: Instant,
}

/// States that a link between two nodes can be in.
#[derive(Clone, Copy)]
enum State {
    /// The link is healthy.
    Healthy,

    /// The link was explicitly partitioned.
    ExplicitPartition,

    /// The link was randomly partitioned.
    RandPartition,

    /// Messages are being held indefinitely.
    Hold,
}

impl Topology {
    pub(crate) fn new(config: config::Link) -> Topology {
        Topology {
            config,
            links: IndexMap::new(),
            rt: Rt::no_software(),
        }
    }

    /// Register a link between two hosts
    pub(crate) fn register(&mut self, a: IpAddr, b: IpAddr) {
        let pair = Pair::new(a, b);
        assert!(self
            .links
            .insert(pair, Link::new(self.rt.now(), self.config.clone()))
            .is_none());
    }

    pub(crate) fn set_max_message_latency(&mut self, value: Duration) {
        self.config.latency_mut().max_message_latency = value;
    }

    pub(crate) fn set_link_message_latency(&mut self, a: IpAddr, b: IpAddr, value: Duration) {
        let latency = self.links[&Pair::new(a, b)].latency(self.config.latency());
        latency.min_message_latency = value;
        latency.max_message_latency = value;
    }

    pub(crate) fn set_link_max_message_latency(&mut self, a: IpAddr, b: IpAddr, value: Duration) {
        self.links[&Pair::new(a, b)]
            .latency(self.config.latency())
            .max_message_latency = value;
    }

    pub(crate) fn set_message_latency_curve(&mut self, value: f64) {
        self.config.latency_mut().latency_distribution = Exp::new(value).unwrap();
    }

    pub(crate) fn set_fail_rate(&mut self, value: f64) {
        self.config.message_loss_mut().fail_rate = value;
    }

    pub(crate) fn set_link_fail_rate(&mut self, a: IpAddr, b: IpAddr, value: f64) {
        self.links[&Pair::new(a, b)]
            .message_loss(self.config.message_loss())
            .fail_rate = value;
    }

    // Send a `message` from `src` to `dst`. This method returns immediately,
    // and message delivery happens at a later time (or never, if the link is
    // broken).
    pub(crate) fn enqueue_message(
        &mut self,
        rand: &mut dyn RngCore,
        src: SocketAddr,
        dst: SocketAddr,
        message: Protocol,
    ) -> Result<()> {
        if let Some(link) = self
            .links
            .get_mut(&Pair::new(src.ip(), dst.ip()))
            // Treat partitions as unlinked hosts
            .and_then(|l| match l.get_state_for_message(src.ip(), dst.ip()) {
                State::Healthy | State::Hold => Some(l),
                State::ExplicitPartition | State::RandPartition => None,
            })
        {
            link.enqueue_message(&self.config, rand, src, dst, message);
            Ok(())
        } else {
            Err(Error::new(ErrorKind::HostUnreachable, "host unreachable"))
        }
    }

    // Move messages from any network links to the `dst` host.
    pub(crate) fn deliver_messages(&mut self, rand: &mut dyn RngCore, dst: &mut Host) {
        for (pair, link) in &mut self.links {
            if pair.0 == dst.addr || pair.1 == dst.addr {
                link.deliver_messages(&self.config, rand, dst);
            }
        }
    }

    pub(crate) fn hold(&mut self, a: IpAddr, b: IpAddr) {
        self.links[&Pair::new(a, b)].hold();
    }

    pub(crate) fn release(&mut self, a: IpAddr, b: IpAddr) {
        self.links[&Pair::new(a, b)].release();
    }

    pub(crate) fn partition(&mut self, a: IpAddr, b: IpAddr) {
        self.links[&Pair::new(a, b)].explicit_partition();
    }

    pub(crate) fn partition_oneway(&mut self, a: IpAddr, b: IpAddr) {
        let link = &mut self.links[&Pair::new(a, b)];
        link.partition_oneway(a, b);
    }

    pub(crate) fn repair(&mut self, a: IpAddr, b: IpAddr) {
        self.links[&Pair::new(a, b)].explicit_repair();
    }

    pub(crate) fn repair_oneway(&mut self, a: IpAddr, b: IpAddr) {
        let link = &mut self.links[&Pair::new(a, b)];
        link.repair_oneway(a, b);
    }

    pub(crate) fn tick_by(&mut self, duration: Duration) {
        let _ = self.rt.tick(duration);
        for link in self.links.values_mut() {
            link.tick(self.rt.now());
        }
    }

    pub(crate) fn iter_mut(&mut self) -> LinksIter {
        LinksIter {
            iter: self.links.iter_mut(),
        }
    }
}

/// Represents a message sent between two hosts on the network.
struct Sent {
    src: SocketAddr,
    dst: SocketAddr,
    status: DeliveryStatus,
    protocol: Protocol,
}

impl Sent {
    fn deliver(&mut self, now: Instant) {
        self.status = DeliveryStatus::DeliverAfter(now);
    }
}

enum DeliveryStatus {
    DeliverAfter(Instant),
    Hold,
}

impl Link {
    fn new(now: Instant, config: config::Link) -> Link {
        Link {
            state_a_b: State::Healthy,
            state_b_a: State::Healthy,
            config,
            sent: VecDeque::new(),
            deliverable: IndexMap::new(),
            now,
        }
    }

    fn enqueue_message(
        &mut self,
        global_config: &config::Link,
        rand: &mut dyn RngCore,
        src: SocketAddr,
        dst: SocketAddr,
        message: Protocol,
    ) {
        tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %message, "Send");

        self.rand_partition_or_repair(global_config, rand);
        self.enqueue(global_config, rand, src, dst, message);
        self.process_deliverables();
    }

    fn get_state_for_message(&self, src: IpAddr, dst: IpAddr) -> State {
        // Between each pair of ip-addresses there can exist a link.
        // A link between two such pairs can be constructed as
        // Pair::new(x,y) or Pair::new(y,x). These two expressions create
        // the exact same link. We denote one of the ends of the link as 'a',
        // and the other 'b'. 'a' is always the one that compares smaller, i.e,
        // `a < b` holds.
        if src < dst {
            self.state_a_b
        } else {
            self.state_b_a
        }
    }

    // src -> link -> dst
    //        ^-- you are here!
    //
    // Messages may be dropped, sit on the link for a while (due to latency, or
    // because the link has stalled), or be delivered immediately.
    fn enqueue(
        &mut self,
        global_config: &config::Link,
        rand: &mut dyn RngCore,
        src: SocketAddr,
        dst: SocketAddr,
        message: Protocol,
    ) {
        let state = self.get_state_for_message(src.ip(), dst.ip());
        let status = match state {
            State::Healthy => {
                let delay = self.delay(global_config.latency(), rand);
                DeliveryStatus::DeliverAfter(self.now + delay)
            }
            // Only A->B is blocked, so B can send, so we can send if src is B
            State::Hold => {
                tracing::trace!(target: TRACING_TARGET,?src, ?dst, protocol = %message, "Hold");
                DeliveryStatus::Hold
            }
            _ => {
                tracing::trace!(target: TRACING_TARGET,?src, ?dst, protocol = %message, "Drop");
                return;
            }
        };

        let sent = Sent {
            src,
            dst,
            status,
            protocol: message,
        };

        self.sent.push_back(sent);
    }

    fn tick(&mut self, now: Instant) {
        self.now = now;
        self.process_deliverables();
    }

    fn process_deliverables(&mut self) {
        // TODO: `drain_filter` is not yet stable, and so we have a low quality
        // implementation here that avoids clones.
        let mut deliverable = 0;
        for i in 0..self.sent.len() {
            let index = i - deliverable;
            let sent = &self.sent[index];
            if let DeliveryStatus::DeliverAfter(time) = sent.status {
                if time <= self.now {
                    let sent = self.sent.remove(index).unwrap();
                    let envelope = Envelope {
                        src: sent.src,
                        dst: sent.dst,
                        message: sent.protocol,
                    };
                    self.deliverable
                        .entry(sent.dst.ip())
                        .or_default()
                        .push_back(envelope);
                    deliverable += 1;
                }
            }
        }
    }

    // FIXME: This implementation does not respect message delivery order. If
    // host A and host B are ordered (by addr), and B sends before A, then this
    // method will deliver A's message before B's.
    fn deliver_messages(
        &mut self,
        global_config: &config::Link,
        rand: &mut dyn RngCore,
        host: &mut Host,
    ) {
        let deliverable = self
            .deliverable
            .entry(host.addr)
            .or_default()
            .drain(..)
            .collect::<Vec<Envelope>>();

        for message in deliverable {
            let (src, dst) = (message.src, message.dst);
            if let Err(message) = host.receive_from_network(message) {
                self.enqueue_message(global_config, rand, dst, src, message);
            }
        }
    }

    // Randomly break or repair this link.
    //
    // FIXME: pull this operation up to the topology level. A link shouldn't be
    // responsible for determining its own random failure.
    fn rand_partition_or_repair(&mut self, global_config: &config::Link, rand: &mut dyn RngCore) {
        match (self.state_a_b, self.state_b_a) {
            (State::Healthy, _) | (_, State::Healthy) => {
                if self.rand_fail(global_config.message_loss(), rand) {
                    self.rand_partition();
                }
            }
            (State::RandPartition, _) | (_, State::RandPartition) => {
                if self.rand_repair(global_config.message_loss(), rand) {
                    self.release();
                }
            }
            _ => {}
        }
    }

    /// With some chance `config.fail_rate`, randomly partition this link.
    fn rand_fail(&self, global: &config::MessageLoss, rand: &mut dyn RngCore) -> bool {
        let config = self.config.message_loss.as_ref().unwrap_or(global);
        let fail_rate = config.fail_rate;
        fail_rate > 0.0 && rand.gen_bool(fail_rate)
    }

    /// With some chance `config.repair_rate`, randomly repair this link.
    fn rand_repair(&self, global: &config::MessageLoss, rand: &mut dyn RngCore) -> bool {
        let config = self.config.message_loss.as_ref().unwrap_or(global);
        let repair_rate = config.repair_rate;
        repair_rate > 0.0 && rand.gen_bool(repair_rate)
    }

    fn hold(&mut self) {
        self.state_a_b = State::Hold;
        self.state_b_a = State::Hold;
    }

    // This link becomes healthy, and any held messages are scheduled for delivery.
    fn release(&mut self) {
        self.state_a_b = State::Healthy;
        self.state_b_a = State::Healthy;
        for sent in &mut self.sent {
            if let DeliveryStatus::Hold = sent.status {
                sent.deliver(self.now);
            }
        }
    }

    fn rand_partition(&mut self) {
        self.state_a_b = State::RandPartition;
        self.state_b_a = State::RandPartition;
    }

    fn explicit_partition(&mut self) {
        self.state_a_b = State::ExplicitPartition;
        self.state_b_a = State::ExplicitPartition;
    }

    fn partition_oneway(&mut self, from: IpAddr, to: IpAddr) {
        if from < to {
            self.state_a_b = State::ExplicitPartition;
        } else {
            self.state_b_a = State::ExplicitPartition;
        }
    }

    fn repair_oneway(&mut self, from: IpAddr, to: IpAddr) {
        if from < to {
            self.state_a_b = State::Healthy;
        } else {
            self.state_b_a = State::Healthy;
        }
    }

    // Repair the link, without releasing any held messages.
    fn explicit_repair(&mut self) {
        self.state_a_b = State::Healthy;
        self.state_b_a = State::Healthy;
    }

    fn delay(&self, global: &config::Latency, rand: &mut dyn RngCore) -> Duration {
        let config = self.config.latency.as_ref().unwrap_or(global);

        let mult = config.latency_distribution.sample(rand);
        let range = (config.max_message_latency - config.min_message_latency).as_millis() as f64;
        let delay = config.min_message_latency + Duration::from_millis((range * mult) as _);

        std::cmp::min(delay, config.max_message_latency)
    }

    fn latency(&mut self, global: &config::Latency) -> &mut config::Latency {
        self.config.latency.get_or_insert_with(|| global.clone())
    }

    fn message_loss(&mut self, global: &config::MessageLoss) -> &mut config::MessageLoss {
        self.config
            .message_loss
            .get_or_insert_with(|| global.clone())
    }
}
