//! Deterministic network simulation for turmoil.

use std::cell::RefCell;

pub(crate) mod dns;
pub(crate) mod fabric;
pub mod fixture;
pub(crate) mod kernel;
pub mod shim;

use crate::dns::Dns;
pub use crate::dns::{ToIpAddr, ToIpAddrs};
use crate::fabric::Fabric;
pub use crate::fabric::HostId;
use crate::kernel::Kernel;
pub use crate::kernel::KernelConfig;

thread_local! {
    static CURRENT: RefCell<Option<Net>> = const { RefCell::new(None) };
}

#[derive(Debug)]
pub struct Net {
    fabric: Fabric,
    dns: Dns,
    current: Option<HostId>,
}

impl Net {
    pub fn new() -> Self {
        Self::with_config(KernelConfig::default())
    }

    /// `cfg` is applied to every host added later.
    pub fn with_config(cfg: KernelConfig) -> Self {
        Self {
            fabric: Fabric::new(cfg),
            dns: Dns::new(),
            current: None,
        }
    }

    /// Register a host. `addrs` accepts hostnames (auto-allocated to
    /// 192.168.x.x on first sight, idempotent on reuse) or literal
    /// IPs. Loopback (127.0.0.1, ::1) is implicit — do not pass it.
    /// Panics if an address is already claimed by another host, or
    /// if loopback is passed explicitly. The first host added becomes
    /// current.
    pub fn add_host<A: ToIpAddrs>(&mut self, addrs: A) -> HostId {
        let ips = addrs.to_ip_addrs(&mut self.dns);
        let id = self.fabric.add_host(ips);
        if self.current.is_none() {
            self.current = Some(id);
        }
        id
    }

    /// Resolve `name` to its registered IP, allocating if unseen.
    /// Mirrors the name resolution used by [`Net::add_host`] and the
    /// shim's hostname-aware socket addrs.
    pub fn lookup(&mut self, name: &str) -> std::net::IpAddr {
        self.dns.resolve(name)
    }

    pub fn host_ids(&self) -> impl Iterator<Item = HostId> + '_ {
        self.fabric.host_ids()
    }

    /// Panics if another `Net` is already installed on this thread.
    pub fn enter(self) -> EnterGuard {
        CURRENT.with(|c| {
            let mut slot = c.borrow_mut();
            assert!(slot.is_none(), "another Net is already installed");
            *slot = Some(self);
        });
        EnterGuard { _priv: () }
    }
}

impl Default for Net {
    fn default() -> Self {
        Self::new()
    }
}

#[must_use = "a Net is only active while the guard is held"]
pub struct EnterGuard {
    _priv: (),
}

impl EnterGuard {
    /// One fabric tick: drain egress across every host and deliver
    /// each packet to its destination kernel.
    pub fn step(&self) {
        CURRENT.with(|c| {
            c.borrow_mut()
                .as_mut()
                .expect("guard is live")
                .fabric
                .step();
        });
    }

    /// Pin which host subsequent `sys()` calls (i.e. socket syscalls
    /// from any task spawned inside this guard) talk to.
    pub fn set_current(&self, id: HostId) {
        CURRENT.with(|c| {
            c.borrow_mut().as_mut().expect("guard is live").current = Some(id);
        });
    }
}

impl Drop for EnterGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = None);
    }
}

pub(crate) fn sys<R>(f: impl FnOnce(&mut Kernel) -> R) -> R {
    CURRENT.with(|c| {
        let mut cell = c.borrow_mut();
        let net = cell
            .as_mut()
            .expect("no Net installed — call Net::enter() first");
        let id = net
            .current
            .expect("no current host — register one with Net::add_host()");
        f(net.fabric.kernel_mut(id))
    })
}

/// Resolve `name` against the installed `Net`'s DNS without allocating.
/// Returns `None` if there is no `Net` installed, or if the name isn't
/// registered and can't be parsed as an IP literal. Shim-side helper
/// for `ToSocketAddrs` impls that accept hostnames.
pub(crate) fn lookup_host(name: &str) -> Option<std::net::IpAddr> {
    CURRENT.with(|c| c.borrow().as_ref().and_then(|net| net.dns.lookup(name)))
}
