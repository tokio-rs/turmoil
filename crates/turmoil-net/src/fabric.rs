//! Inter-host packet routing.

use std::io::{Error, ErrorKind};
use std::net::IpAddr;
use std::time::Duration;

use indexmap::IndexMap;

use crate::kernel::{Kernel, KernelConfig, Packet};
use crate::rule::{Rule, RuleId, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostId(u32);

#[derive(Debug)]
pub struct Host {
    #[allow(dead_code)]
    pub addrs: Vec<IpAddr>,
    pub kernel: Kernel,
}

/// Packet scheduled for delivery at a future sim time.
struct Scheduled {
    /// Sim time at which this packet becomes deliverable. Compared
    /// against `Fabric::now`.
    deliver_at: Duration,
    /// Monotonic tie-breaker so two packets sharing a deadline
    /// deliver in emission order.
    seq: u64,
    pkt: Packet,
}

pub struct Fabric {
    hosts: IndexMap<HostId, Host>,
    ip_to_host: IndexMap<IpAddr, HostId>,
    next_id: u32,
    default_cfg: KernelConfig,
    /// Installed rules, consulted in insertion order. Loopback
    /// packets skip this entirely (delivered inline in egress).
    rules: IndexMap<RuleId, Box<dyn Rule>>,
    next_rule_id: u64,
    /// Monotonic sim time, advanced by `step(dt)`.
    now: Duration,
    /// Packets queued for future delivery. Kept sorted by
    /// `(deliver_at, seq)` so the ready prefix is a contiguous drain.
    pending: Vec<Scheduled>,
    next_pkt_seq: u64,
}

impl std::fmt::Debug for Fabric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fabric")
            .field("hosts", &self.hosts)
            .field("ip_to_host", &self.ip_to_host)
            .field("rules", &format!("{} installed", self.rules.len()))
            .field("now", &self.now)
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl Fabric {
    pub fn new(cfg: KernelConfig) -> Self {
        Self {
            hosts: IndexMap::new(),
            ip_to_host: IndexMap::new(),
            next_id: 0,
            default_cfg: cfg,
            rules: IndexMap::new(),
            next_rule_id: 1,
            now: Duration::ZERO,
            pending: Vec::new(),
            next_pkt_seq: 0,
        }
    }

    pub fn install_rule(&mut self, rule: Box<dyn Rule>) -> RuleId {
        let id = RuleId(self.next_rule_id);
        self.next_rule_id += 1;
        self.rules.insert(id, rule);
        id
    }

    pub fn uninstall_rule(&mut self, id: RuleId) {
        self.rules.shift_remove(&id);
    }

    #[allow(dead_code)]
    pub fn now(&self) -> Duration {
        self.now
    }

    /// Loopback is implicit; passing it explicitly panics. Panics
    /// if any address is already claimed.
    pub fn add_host(&mut self, addrs: Vec<IpAddr>) -> HostId {
        self.try_add_host(addrs)
            .expect("add_host validation failed")
    }

    fn try_add_host(&mut self, addrs: Vec<IpAddr>) -> std::io::Result<HostId> {
        for a in &addrs {
            if a.is_loopback() {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "loopback is implicit; do not register it explicitly",
                ));
            }
            if self.ip_to_host.contains_key(a) {
                return Err(Error::from(ErrorKind::AddrInUse));
            }
        }

        let id = HostId(self.next_id);
        self.next_id += 1;

        let mut kernel = Kernel::with_config(self.default_cfg.clone());
        for a in &addrs {
            kernel.add_address(*a);
        }
        for a in &addrs {
            self.ip_to_host.insert(*a, id);
        }
        self.hosts.insert(id, Host { addrs, kernel });
        Ok(id)
    }

    pub fn host_ids(&self) -> impl Iterator<Item = HostId> + '_ {
        self.hosts.keys().copied()
    }

    pub fn kernel_mut(&mut self, id: HostId) -> &mut Kernel {
        &mut self
            .hosts
            .get_mut(&id)
            .expect("HostId points at a live host")
            .kernel
    }

    pub fn kernel(&self, id: HostId) -> &Kernel {
        &self
            .hosts
            .get(&id)
            .expect("HostId points at a live host")
            .kernel
    }

    /// Reverse the IP→host mapping. Returns `None` for loopback (which
    /// every host implicitly owns) and for unclaimed IPs.
    pub fn host_for_ip(&self, ip: IpAddr) -> Option<HostId> {
        self.ip_to_host.get(&ip).copied()
    }

    /// Advance sim time by `dt`, then process packets. Caller-driven
    /// — the fabric has no real clock of its own. Pass `Duration::ZERO`
    /// to run the pump without advancing time (useful if you're
    /// stepping multi-host and only want to count one "tick" total).
    ///
    /// Each call:
    /// 1. Adds `dt` to `now`.
    /// 2. Drains ready entries from `pending` (deadline ≤ now) into
    ///    their destination kernels.
    /// 3. Drains egress from every host; for each packet consults
    ///    the rule chain and either delivers immediately or schedules
    ///    for later.
    pub fn step(&mut self, dt: Duration) {
        self.now += dt;

        // Drain the ready prefix of `pending`. The vec is kept sorted
        // by (deliver_at, seq), so scanning from the front until we
        // hit a future deadline is O(ready) not O(pending).
        let ready_end = self
            .pending
            .iter()
            .position(|s| s.deliver_at > self.now)
            .unwrap_or(self.pending.len());
        let ready: Vec<Scheduled> = self.pending.drain(..ready_end).collect();
        for s in ready {
            self.deliver_to_dst(s.pkt);
        }

        // Drain all egress first (releases `&mut` on the hosts map),
        // then consult rules for each packet.
        let mut in_flight = Vec::new();
        let ids: Vec<_> = self.hosts.keys().copied().collect();
        for id in ids {
            let host = self.hosts.get_mut(&id).expect("id from iteration");
            in_flight.extend(host.kernel.egress());
        }

        for pkt in in_flight {
            let verdict = self.evaluate(&pkt);
            match verdict {
                Verdict::Drop => {}
                Verdict::Deliver(d) if d.is_zero() => self.deliver_to_dst(pkt),
                Verdict::Deliver(d) => self.schedule(pkt, d),
                Verdict::Pass => self.deliver_to_dst(pkt),
            }
        }
    }

    /// Walk the rule chain in install order. First non-`Pass` wins;
    /// an empty chain (or all passes) yields `Verdict::Pass`.
    fn evaluate(&mut self, pkt: &Packet) -> Verdict {
        for rule in self.rules.values_mut() {
            match rule.on_packet(pkt) {
                Verdict::Pass => continue,
                v => return v,
            }
        }
        Verdict::Pass
    }

    fn schedule(&mut self, pkt: Packet, delay: Duration) {
        let deliver_at = self.now + delay;
        let seq = self.next_pkt_seq;
        self.next_pkt_seq += 1;
        let entry = Scheduled {
            deliver_at,
            seq,
            pkt,
        };
        // Binary search keeps the vec sorted by (deliver_at, seq).
        let idx = self
            .pending
            .binary_search_by(|s| (s.deliver_at, s.seq).cmp(&(entry.deliver_at, entry.seq)))
            .unwrap_or_else(|i| i);
        self.pending.insert(idx, entry);
    }

    fn deliver_to_dst(&mut self, pkt: Packet) {
        if let Some(&dst) = self.ip_to_host.get(&pkt.dst) {
            self.hosts.get_mut(&dst).unwrap().kernel.deliver(pkt);
        }
        // else: no route, drop silently.
    }
}
