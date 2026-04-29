//! Inter-host packet routing.

use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::net::IpAddr;

use crate::kernel::{Kernel, KernelConfig};

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
    hosts: HashMap<HostId, Host>,
    ip_to_host: HashMap<IpAddr, HostId>,
    next_id: u32,
    default_cfg: KernelConfig,
}

impl Fabric {
    pub fn new(cfg: KernelConfig) -> Self {
        Self {
            hosts: HashMap::new(),
            ip_to_host: HashMap::new(),
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

    // TODO: synthetic latency. Today packets delivered in the same
    // step are observed in the same scheduler turn. Once the fabric
    // has a clock, stamp each packet with a `deliver_after: Instant`
    // and queue it until the clock reaches that time.
    pub fn step(&mut self) {
        // Two-phase: drain all egress first (releases `&mut` on the
        // map), then dispatch.
        let mut in_flight = Vec::new();
        let ids: Vec<_> = self.hosts.keys().copied().collect();
        for id in ids {
            let host = self.hosts.get_mut(&id).expect("id from iteration");
            in_flight.extend(host.kernel.egress());
        }
        for pkt in in_flight {
            if let Some(&dst) = self.ip_to_host.get(&pkt.dst) {
                self.hosts.get_mut(&dst).unwrap().kernel.deliver(pkt);
            }
            // else: no route, drop silently.
        }
    }
}
