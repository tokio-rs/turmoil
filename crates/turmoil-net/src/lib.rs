//! Deterministic network simulation for turmoil.
//!
//! `turmoil-net` is a simulated socket stack. Production code imports
//! [`tokio::net`]; tests swap the import for [`shim::tokio::net`] and
//! run the same code against a fully deterministic network.
//!
//! The crate README has the motivation and code examples. The module
//! list below is a map of the code:
//!
//! - [`shim`] — drop-in replacements for `tokio::net` types.
//!   Production code changes one import and runs unchanged.
//! - [`fixture`] — batteries-included scheduler + runtime for the
//!   common shapes ([`fixture::lo`] single-host, [`fixture::ClientServer`]
//!   multi-host). Start here; drop to the primitives when you outgrow
//!   them.
//! - [`Net`] / [`EnterGuard`] — the primitives the fixtures are built
//!   on. Build a topology with [`Net::add_host`], install it with
//!   [`Net::enter`], drive the fabric with [`EnterGuard::step`].
//! - [`Rule`] / [`Verdict`] / [`rule`] — packet-level fault injection.
//!   Rules see every non-loopback packet and decide Pass / Deliver
//!   (with optional delay) / Drop.
//! - [`netstat`] — Linux-style socket snapshot for debugging a test.
//!
//! [`tokio::net`]: https://docs.rs/tokio/latest/tokio/net/index.html

use std::cell::RefCell;

mod dns;
mod fabric;
pub mod fixture;
mod kernel;
mod netstat;
mod rule;
pub mod shim;

use crate::dns::Dns;
pub use crate::dns::{ToIpAddr, ToIpAddrs};
use crate::fabric::Fabric;
pub use crate::fabric::HostId;
use crate::kernel::Kernel;
pub use crate::kernel::{KernelConfig, Packet, TcpFlags, TcpSegment, Transport, UdpDatagram};
pub use crate::netstat::{Netstat, NetstatEntry, NetstatState, Proto};
use crate::rule::RuleGuard;
pub use crate::rule::{Latency, Rule, RuleId, Verdict};

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

    /// Install a rule for the life of the `Net`. Use this when the
    /// rule is part of the test's fixed setup (symmetric latency,
    /// permanent packet filter, etc). For rules that only apply to
    /// a phase of the test, use the guard-returning [`rule`] free
    /// function from inside the sim instead.
    pub fn rule(&mut self, rule: impl Rule) {
        self.fabric.install_rule(Box::new(rule));
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
    /// One fabric tick: advance sim time by `dt`, deliver any packets
    /// whose scheduled arrival has come due, and pump egress/rules for
    /// packets emitted this turn. Pass [`Duration::ZERO`] to pump
    /// without advancing time.
    pub fn step(&self, dt: std::time::Duration) {
        CURRENT.with(|c| {
            c.borrow_mut()
                .as_mut()
                .expect("guard is live")
                .fabric
                .step(dt);
        });
    }

    /// Pin which host subsequent `sys()` calls (i.e. socket syscalls
    /// from any task spawned inside this guard) talk to.
    pub fn set_current(&self, id: HostId) {
        CURRENT.with(|c| {
            c.borrow_mut().as_mut().expect("guard is live").current = Some(id);
        });
    }

    /// Install a rule, vending a [`RuleGuard`] that uninstalls it on
    /// drop. Useful in schedulers or fixture code that owns the
    /// guard's lifetime explicitly — async tasks should call the
    /// free [`rule`] function instead.
    pub fn rule(&self, r: impl Rule) -> RuleGuard {
        RuleGuard::new(install_rule(Box::new(r)))
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

/// Install a rule and return a guard that uninstalls it on drop.
/// Callable from any task inside an installed `Net`. Panics if no
/// `Net` is installed.
///
/// For rules that should live for the entire simulation, use
/// [`Net::rule`] before calling [`Net::enter`].
pub fn rule(r: impl Rule) -> RuleGuard {
    RuleGuard::new(install_rule(Box::new(r)))
}

fn install_rule(r: Box<dyn Rule>) -> RuleId {
    CURRENT.with(|c| {
        c.borrow_mut()
            .as_mut()
            .expect("no Net installed — call Net::enter() first")
            .fabric
            .install_rule(r)
    })
}

fn uninstall_rule(id: RuleId) {
    CURRENT.with(|c| {
        // Tolerant of the Net already being gone — drop order during
        // teardown isn't guaranteed.
        if let Some(net) = c.borrow_mut().as_mut() {
            net.fabric.uninstall_rule(id);
        }
    });
}

/// Snapshot a host's socket table, Linux `netstat`-style. `host`
/// accepts a hostname (resolved via DNS like [`Net::add_host`]) or a
/// literal IP. Panics if no `Net` is installed, or if the address
/// doesn't match a registered host. Loopback isn't routable on its
/// own — pass a hostname or the host's configured IP.
pub fn netstat<H: ToIpAddr>(host: H) -> Netstat {
    CURRENT.with(|c| {
        let cell = c.borrow();
        let net = cell
            .as_ref()
            .expect("no Net installed — call Net::enter() first");
        let ip = host
            .try_to_ip_addr(&net.dns)
            .expect("hostname not registered");
        let id = net
            .fabric
            .host_for_ip(ip)
            .unwrap_or_else(|| panic!("no host registered for {ip}"));
        netstat::snapshot(net.fabric.kernel(id))
    })
}
