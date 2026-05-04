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
//!   [`Net::enter`], drive the fabric with [`EnterGuard::egress_all`] /
//!   [`EnterGuard::evaluate`] / [`EnterGuard::deliver`].
//! - [`Rule`] / [`Verdict`] / [`rule`] — packet-level fault injection.
//!   Rules see every non-loopback packet and decide Pass / Deliver
//!   (with optional delay) / Drop.
//! - [`netstat`] — Linux-style socket snapshot for debugging a test.
//!
//! [`tokio::net`]: https://docs.rs/tokio/latest/tokio/net/index.html

use std::cell::RefCell;

use indexmap::IndexMap;

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

pub struct Net {
    fabric: Fabric,
    dns: Dns,
    current: Option<HostId>,
    /// Installed rules, consulted in insertion order.
    rules: IndexMap<RuleId, Box<dyn Rule>>,
    next_rule_id: u64,
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
            rules: IndexMap::new(),
            next_rule_id: 1,
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
        self.install_rule(Box::new(rule));
    }

    fn install_rule(&mut self, rule: Box<dyn Rule>) -> RuleId {
        let id = RuleId(self.next_rule_id);
        self.next_rule_id += 1;
        self.rules.insert(id, rule);
        id
    }

    fn uninstall_rule(&mut self, id: RuleId) {
        self.rules.shift_remove(&id);
    }

    /// Walk the installed rules in insertion order; first non-`Pass`
    /// wins. Harness code calls this once per outbound packet to let
    /// rules interpose.
    fn evaluate(&mut self, pkt: &Packet) -> Verdict {
        for rule in self.rules.values_mut() {
            match rule.on_packet(pkt) {
                Verdict::Pass => continue,
                v => return v,
            }
        }
        Verdict::Pass
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

impl std::fmt::Debug for Net {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Net")
            .field("fabric", &self.fabric)
            .field("dns", &self.dns)
            .field("current", &self.current)
            .field("rules", &format!("{} installed", self.rules.len()))
            .finish()
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
    /// Drain every host's outbound queue into `out`. The caller decides
    /// what happens next — typically: consult rules via
    /// [`EnterGuard::evaluate`] for each packet and then
    /// [`EnterGuard::deliver`] (or schedule for later). The buffer is
    /// passed in so the caller can reuse its allocation across ticks.
    /// See [`fixture`] for the default tokio-driven loop.
    pub fn egress_all(&self, out: &mut Vec<Packet>) {
        CURRENT.with(|c| {
            c.borrow_mut()
                .as_mut()
                .expect("guard is live")
                .fabric
                .egress_all(out)
        });
    }

    /// Route a packet to the host owning its destination IP. Drops
    /// silently if no host is registered for that IP.
    pub fn deliver(&self, pkt: Packet) {
        CURRENT.with(|c| {
            c.borrow_mut()
                .as_mut()
                .expect("guard is live")
                .fabric
                .deliver(pkt)
        });
    }

    /// Walk installed rules for `pkt`. First non-`Pass` verdict wins;
    /// empty rule chain returns `Verdict::Pass`.
    pub fn evaluate(&self, pkt: &Packet) -> Verdict {
        CURRENT.with(|c| {
            c.borrow_mut()
                .as_mut()
                .expect("guard is live")
                .evaluate(pkt)
        })
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

/// Resolve `name` against the installed `Net`'s DNS. Returns `None`
/// if there is no `Net` installed, or if the name isn't registered
/// and can't be parsed as an IP literal.
pub fn lookup_host(name: &str) -> Option<std::net::IpAddr> {
    CURRENT.with(|c| c.borrow().as_ref().and_then(|net| net.dns.lookup(name)))
}

/// Pin which host subsequent [`sys`](crate) calls (i.e. socket
/// syscalls from the caller's task) talk to.
///
/// This is the free-function form of [`EnterGuard::set_current`], for
/// use where the guard isn't in scope — typically inside a future
/// wrapper that rescopes every poll. Harnesses with shared-runtime
/// fixtures (see [`fixture::ClientServer`] for the canonical pattern)
/// call this on entry to each task's poll so `sys()` lookups land in
/// the right kernel.
///
/// Panics if no `Net` is installed.
pub fn set_current(id: HostId) {
    CURRENT.with(|c| {
        c.borrow_mut()
            .as_mut()
            .expect("no Net installed — call Net::enter() first")
            .current = Some(id);
    });
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
            .install_rule(r)
    })
}

fn uninstall_rule(id: RuleId) {
    CURRENT.with(|c| {
        // Tolerant of the Net already being gone — drop order during
        // teardown isn't guaranteed.
        if let Some(net) = c.borrow_mut().as_mut() {
            net.uninstall_rule(id);
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
