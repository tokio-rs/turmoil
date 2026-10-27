use crate::{config, TRACING_TARGET};

use indexmap::IndexMap;
use rand::{Rng, RngCore};
use rand_distr::{Distribution, Exp};
use std::collections::VecDeque;
use std::fmt::{self, Display};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use turmoil_net::{Packet, Transport};

/// Describes the network topology.
pub(crate) struct Topology {
    config: config::Link,

    /// Specific configuration overrides between specific hosts.
    links: IndexMap<Pair, Link>,

    /// Network time, moved forward with [`Topology::tick_by`].
    now: Duration,
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
    now: Duration,
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
    now: Duration,
    sent: &'a mut Sent,
}

impl SentRef<'_> {
    /// The (src, dst) [`SocketAddr`] pair for the message. For a broadcast
    /// or multicast, `dst` is the group address. Non-initial fragments
    /// have no transport header, so their ports are reported as zero.
    pub fn pair(&self) -> (SocketAddr, SocketAddr) {
        endpoints(&self.sent.packet)
    }

    /// The packet in flight.
    pub fn packet(&self) -> &Packet {
        &self.sent.packet
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

    /// Messages that are ready to be delivered, keyed by receiving host.
    deliverable: IndexMap<IpAddr, VecDeque<Packet>>,

    /// Latest delivery time for each directed TCP connection under
    /// the default latency distribution. Explicit rules may reorder.
    tcp_tail: IndexMap<(SocketAddr, SocketAddr), Duration>,

    /// The current network time, moved forward with [`Link::tick`].
    now: Duration,
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
            now: Duration::ZERO,
        }
    }

    /// Register a link between two hosts
    pub(crate) fn register(&mut self, a: IpAddr, b: IpAddr) {
        let pair = Pair::new(a, b);
        assert!(self.links.insert(pair, Link::new(self.now)).is_none());
    }

    pub(crate) fn set_max_message_latency(&mut self, value: Duration) {
        self.config.latency_mut().max_message_latency = value;
    }

    pub(crate) fn max_message_latency(&self) -> Duration {
        self.links
            .values()
            .filter_map(|link| link.config.latency.as_ref())
            .map(|latency| latency.max_message_latency)
            .fold(self.config.latency().max_message_latency, Duration::max)
    }

    pub(crate) fn set_link_message_latency(&mut self, a: IpAddr, b: IpAddr, value: Duration) {
        let pair = Pair::new(a, b);
        let latency = self
            .links
            .get_mut(&pair)
            .unwrap_or_else(|| panic!("unable to find link between {pair:?}"))
            .latency(self.config.latency());
        latency.min_message_latency = value;
        latency.max_message_latency = value;
    }

    pub(crate) fn set_link_max_message_latency(&mut self, a: IpAddr, b: IpAddr, value: Duration) {
        let pair = Pair::new(a, b);
        self.links
            .get_mut(&pair)
            .unwrap_or_else(|| panic!("unable to find link between {pair:?}"))
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
        let pair = Pair::new(a, b);
        self.links
            .get_mut(&pair)
            .unwrap_or_else(|| panic!("unable to find link between {pair:?}"))
            .message_loss(self.config.message_loss())
            .fail_rate = value;
    }

    /// Put `packet` on the link from host `from` to host `to`. Delivery
    /// happens at a later time, or never if the link is broken. `delay`
    /// overrides the link's latency.
    pub(crate) fn enqueue_message(
        &mut self,
        rand: &mut dyn RngCore,
        from: IpAddr,
        to: IpAddr,
        packet: Packet,
        delay: Option<Duration>,
    ) {
        match self.links.get_mut(&Pair::new(from, to)) {
            Some(link) => link.enqueue_message(&self.config, rand, from, to, packet, delay),
            None => {
                let (src, dst) = endpoints(&packet);
                tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Summary(&packet), "Drop (no link)");
            }
        }
    }

    /// Take the messages ready for delivery to host `dst`.
    pub(crate) fn take_deliverable(&mut self, dst: IpAddr) -> Vec<Packet> {
        let mut packets = Vec::new();
        for (pair, link) in &mut self.links {
            if pair.0 == dst || pair.1 == dst {
                if let Some(queue) = link.deliverable.get_mut(&dst) {
                    packets.extend(queue.drain(..));
                }
            }
        }
        packets
    }

    pub(crate) fn hold(&mut self, a: IpAddr, b: IpAddr) {
        self.get_link_mut(&Pair::new(a, b)).hold();
    }

    pub(crate) fn release(&mut self, a: IpAddr, b: IpAddr) {
        self.get_link_mut(&Pair::new(a, b)).release();
    }

    pub(crate) fn partition(&mut self, a: IpAddr, b: IpAddr) {
        self.get_link_mut(&Pair::new(a, b)).explicit_partition();
    }

    pub(crate) fn partition_oneway(&mut self, a: IpAddr, b: IpAddr) {
        let link = &mut self.get_link_mut(&Pair::new(a, b));
        link.partition_oneway(a, b);
    }

    pub(crate) fn repair(&mut self, a: IpAddr, b: IpAddr) {
        self.get_link_mut(&Pair::new(a, b)).explicit_repair();
    }

    pub(crate) fn repair_oneway(&mut self, a: IpAddr, b: IpAddr) {
        let link = &mut self.get_link_mut(&Pair::new(a, b));
        link.repair_oneway(a, b);
    }

    pub(crate) fn tick_by(&mut self, duration: Duration) {
        self.now += duration;
        for link in self.links.values_mut() {
            link.tick(self.now);
        }
    }

    pub(crate) fn iter_mut(&mut self) -> LinksIter<'_> {
        LinksIter {
            iter: self.links.iter_mut(),
        }
    }

    #[inline]
    fn get_link_mut(&mut self, pair: &Pair) -> &mut Link {
        self.links
            .get_mut(pair)
            .unwrap_or_else(|| panic!("unable to find link between {pair:?}"))
    }
}

/// Represents a message sent between two hosts on the network.
struct Sent {
    /// Sending host.
    from: IpAddr,
    /// Receiving host. Differs from `packet.dst` for broadcast and
    /// multicast copies.
    to: IpAddr,
    status: DeliveryStatus,
    packet: Packet,
    ordered: bool,
}

impl Sent {
    fn deliver(&mut self, now: Duration) {
        self.status = DeliveryStatus::DeliverAfter(now);
    }
}

enum DeliveryStatus {
    DeliverAfter(Duration),
    Hold,
}

impl Link {
    fn new(now: Duration) -> Link {
        Link {
            state_a_b: State::Healthy,
            state_b_a: State::Healthy,
            config: config::Link::default(),
            sent: VecDeque::new(),
            deliverable: IndexMap::new(),
            tcp_tail: IndexMap::new(),
            now,
        }
    }

    fn enqueue_message(
        &mut self,
        global_config: &config::Link,
        rand: &mut dyn RngCore,
        from: IpAddr,
        to: IpAddr,
        packet: Packet,
        delay: Option<Duration>,
    ) {
        let (src, dst) = endpoints(&packet);
        tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Summary(&packet), "Send");

        self.rand_partition_or_repair(global_config, rand);
        self.enqueue(global_config, rand, from, to, packet, delay);
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
        from: IpAddr,
        to: IpAddr,
        packet: Packet,
        delay: Option<Duration>,
    ) {
        let state = self.get_state_for_message(from, to);
        let ordered = delay.is_none() && matches!(packet.payload, Transport::Tcp(_));
        let status = match state {
            State::Healthy => {
                let delay = delay.unwrap_or_else(|| self.delay(global_config.latency(), rand));
                let mut at = self.now + delay;
                if ordered {
                    let tail = self.tcp_tail.entry(endpoints(&packet)).or_default();
                    at = at.max(*tail);
                    *tail = at;
                }
                DeliveryStatus::DeliverAfter(at)
            }
            State::Hold => {
                let (src, dst) = endpoints(&packet);
                tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Summary(&packet), "Hold");
                DeliveryStatus::Hold
            }
            _ => {
                let (src, dst) = endpoints(&packet);
                tracing::trace!(target: TRACING_TARGET, ?src, ?dst, protocol = %Summary(&packet), "Drop");
                return;
            }
        };

        self.sent.push_back(Sent {
            from,
            to,
            status,
            packet,
            ordered,
        });
    }

    fn tick(&mut self, now: Duration) {
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
                    self.deliverable
                        .entry(sent.to)
                        .or_default()
                        .push_back(sent.packet);
                    deliverable += 1;
                }
            }
        }
        if deliverable > 0 {
            self.refresh_tcp_tails();
        }
    }

    fn refresh_tcp_tails(&mut self) {
        self.tcp_tail.clear();
        for sent in &self.sent {
            if sent.ordered {
                if let DeliveryStatus::DeliverAfter(at) = sent.status {
                    let tail = self.tcp_tail.entry(endpoints(&sent.packet)).or_default();
                    *tail = (*tail).max(at);
                }
            }
        }
    }

    // Randomly break or repair this link.
    fn rand_partition_or_repair(&mut self, global_config: &config::Link, rand: &mut dyn RngCore) {
        let do_rand = self.rand_partition(global_config.message_loss(), rand);
        match (self.state_a_b, self.state_b_a) {
            (State::Healthy, _) | (_, State::Healthy) if do_rand => {
                self.state_a_b = State::RandPartition;
                self.state_b_a = State::RandPartition;

                self.sent.clear();
                self.tcp_tail.clear();
            }
            (State::RandPartition, _) | (_, State::RandPartition)
                if self.rand_repair(global_config.message_loss(), rand) =>
            {
                self.release();
            }
            _ => {}
        }
    }

    fn hold(&mut self) {
        self.state_a_b = State::Hold;
        self.state_b_a = State::Hold;

        for sent in &mut self.sent {
            sent.status = DeliveryStatus::Hold;
        }
        self.tcp_tail.clear();
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
        self.refresh_tcp_tails();
    }

    fn explicit_partition(&mut self) {
        self.state_a_b = State::ExplicitPartition;
        self.state_b_a = State::ExplicitPartition;

        self.sent.clear();
        self.tcp_tail.clear();
    }

    fn partition_oneway(&mut self, from: IpAddr, to: IpAddr) {
        if from < to {
            self.state_a_b = State::ExplicitPartition;
        } else {
            self.state_b_a = State::ExplicitPartition;
        }

        self.sent.retain(|sent| sent.from != from);
        self.refresh_tcp_tails();
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

    /// Should the link be randomly partitioned
    fn rand_partition(&self, global: &config::MessageLoss, rand: &mut dyn RngCore) -> bool {
        let config = self.config.message_loss.as_ref().unwrap_or(global);
        let fail_rate = config.fail_rate;
        fail_rate > 0.0 && rand.random_bool(fail_rate)
    }

    fn rand_repair(&self, global: &config::MessageLoss, rand: &mut dyn RngCore) -> bool {
        let config = self.config.message_loss.as_ref().unwrap_or(global);
        let repair_rate = config.repair_rate;
        repair_rate > 0.0 && rand.random_bool(repair_rate)
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

/// Source and destination endpoints of `packet`.
pub(crate) fn endpoints(packet: &Packet) -> (SocketAddr, SocketAddr) {
    let (src, dst) = match &packet.payload {
        Transport::Tcp(s) => (s.src_port, s.dst_port),
        Transport::Udp(d) => (d.src_port, d.dst_port),
        Transport::Fragment(f) => f.ports().unwrap_or((0, 0)),
    };
    (
        SocketAddr::new(packet.src, src),
        SocketAddr::new(packet.dst, dst),
    )
}

/// Renders a packet for traces: protocol, TCP flags, and the payload
/// as hex bytes.
pub(crate) struct Summary<'a>(pub(crate) &'a Packet);

impl Display for Summary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let payload = match &self.0.payload {
            Transport::Fragment(fragment) => {
                write!(
                    f,
                    "IP fragment id={} offset={} more={}",
                    fragment.id, fragment.offset, fragment.more
                )?;
                &fragment.payload
            }
            Transport::Udp(d) => {
                write!(f, "UDP")?;
                &d.payload
            }
            Transport::Tcp(s) => {
                write!(f, "TCP")?;
                let flags = [
                    (s.flags.syn, "SYN"),
                    (s.flags.ack, "ACK"),
                    (s.flags.psh, "PSH"),
                    (s.flags.fin, "FIN"),
                    (s.flags.rst, "RST"),
                ];
                for (_, name) in flags.iter().filter(|(set, _)| *set) {
                    write!(f, " {name}")?;
                }
                if s.payload.is_empty() {
                    return Ok(());
                }
                &s.payload
            }
        };
        write!(f, " [")?;
        for (i, b) in payload.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{b:#04X}")?;
        }
        write!(f, "]")
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Link;

    use super::*;

    #[test]
    #[should_panic(expected = "unable to find link between Pair(10.0.0.1, 192.168.0.1)")]
    fn link_access_gives_useful_panic_message() {
        let mut topo = Topology::new(Link::default());
        topo.hold("10.0.0.1".parse().unwrap(), "192.168.0.1".parse().unwrap());
    }
}
