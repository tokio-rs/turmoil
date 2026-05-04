//! Inter-host packet routing.
//!
//! `Fabric` is a pure router: it owns the host registry and exposes
//! `egress_all` / `deliver` as primitives.

use std::io::{Error, ErrorKind};
use std::net::IpAddr;

use indexmap::IndexMap;

use crate::kernel::{Kernel, KernelConfig, Packet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostId(u32);

#[derive(Debug)]
pub struct Host {
    #[allow(dead_code)]
    pub addrs: Vec<IpAddr>,
    pub kernel: Kernel,
}

#[derive(Debug)]
pub struct Fabric {
    hosts: IndexMap<HostId, Host>,
    ip_to_host: IndexMap<IpAddr, HostId>,
    next_id: u32,
    default_cfg: KernelConfig,
}

impl Fabric {
    pub fn new(cfg: KernelConfig) -> Self {
        Self {
            hosts: IndexMap::new(),
            ip_to_host: IndexMap::new(),
            next_id: 0,
            default_cfg: cfg,
        }
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

    /// Drain outbound packets from every host, in host-insertion order,
    /// appending to `out`. The caller decides what happens to each
    /// packet — deliver, drop, schedule for later — and reuses the
    /// buffer across ticks. Loopback packets fold back through the
    /// source host's `deliver` inline (inside `Kernel::egress`) and
    /// never appear in `out`.
    pub fn egress_all(&mut self, out: &mut Vec<Packet>) {
        for host in self.hosts.values_mut() {
            host.kernel.egress(out);
        }
    }

    /// Route a packet to the host bound to its destination IP. Drops
    /// silently if the destination isn't registered — matches real-
    /// world "packet hit an unreachable address" behavior, and keeps
    /// the harness free of routing concerns.
    pub fn deliver(&mut self, pkt: Packet) {
        if let Some(&dst) = self.ip_to_host.get(&pkt.dst) {
            self.hosts.get_mut(&dst).unwrap().kernel.deliver(pkt);
        }
    }
}
