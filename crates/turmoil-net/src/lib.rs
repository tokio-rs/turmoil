//! Deterministic network simulation for turmoil.
//!
//! `turmoil-net` is a simulated socket stack. Production code imports
//! [`tokio::net`]; tests swap the import for [`shim::tokio::net`] and
//! run the same code against a deterministic network.
//!
//! The crate README has the motivation and code examples.
//!
//! - [`shim`]: simulated sockets with Tokio-compatible APIs.
//! - [`fixture`]: a scheduler and runtime for single-host ([`fixture::lo`])
//!   and multi-host ([`fixture::ClientServer`]) tests.
//! - [`Net`] / [`EnterGuard`]: primitives for custom harnesses.
//!   Build a topology with [`Net::add_host`], install it with [`Net::enter`],
//!   and drive packet delivery with [`EnterGuard::egress_all`],
//!   [`EnterGuard::evaluate`], and [`EnterGuard::deliver`].
//! - [`Rule`] / [`Verdict`] / [`rule`]: packet fault injection.
//!   Rules pass, delay, or drop outbound packets.
//! - [`KernelConfig`]: MTUs, buffer limits, and retry thresholds.
//! - [`netstat`]: Linux-style socket snapshots for debugging a test.
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
pub use crate::kernel::{
    Interface, InterfaceId, IpFragment, KernelConfig, Packet, TcpFlags, TcpSegment, Transport,
    UdpDatagram,
};
pub use crate::netstat::{Netstat, NetstatEntry, NetstatState, Proto};
pub use crate::rule::{Latency, Rule, RuleGuard, RuleId, Verdict};

thread_local! {
    static CURRENT: RefCell<Option<Net>> = const { RefCell::new(None) };
}

type Resolver = Box<dyn Fn(&str) -> Option<std::net::IpAddr>>;

/// Simulated hosts, address resolution, packet routing, and fault rules.
///
/// Install with [`Net::enter`] before using socket shims on the current thread.
pub struct Net {
    fabric: Fabric,
    dns: Dns,
    /// Harness-supplied name lookup, consulted before `dns`.
    resolver: Option<Resolver>,
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
            resolver: None,
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

    /// Inspect a host's loopback and shared-link interfaces.
    pub fn interfaces(&self, host: HostId) -> &[Interface] {
        self.fabric.kernel(host).interfaces()
    }

    /// Resolve `name` through the built-in DNS, allocating an IP if unseen.
    /// Socket lookups use existing mappings or the resolver set by
    /// [`Net::set_resolver`]; they do not allocate addresses.
    pub fn lookup(&mut self, name: &str) -> std::net::IpAddr {
        self.dns.resolve(name)
    }

    pub fn host_ids(&self) -> impl Iterator<Item = HostId> + '_ {
        self.fabric.host_ids()
    }

    /// The host owning `ip`. `None` for loopback and unclaimed IPs.
    pub fn host_for_ip(&self, ip: std::net::IpAddr) -> Option<HostId> {
        self.fabric.host_for_ip(ip)
    }

    /// Use `resolver` for socket and netstat lookups, falling back to the
    /// built-in DNS when it returns `None`.
    ///
    /// Register hosts by literal IP when the harness owns name resolution.
    pub fn set_resolver(&mut self, resolver: impl Fn(&str) -> Option<std::net::IpAddr> + 'static) {
        self.resolver = Some(Box::new(resolver));
    }

    /// Drain one host's outbound packets into `out`.
    /// See [`EnterGuard::egress_all`] to drain all hosts.
    pub fn egress(&mut self, host: HostId, out: &mut Vec<Packet>) {
        self.fabric.egress(host, out);
    }

    /// Return the destination host for unicast, or the other hosts on the
    /// shared link for UDP broadcast and multicast. Unknown unicast
    /// destinations return an empty list.
    ///
    /// Use with [`Net::deliver_to`] to apply faults separately to each receiver.
    /// If the source IP has no registered owner, the harness must exclude the
    /// sender itself.
    pub fn receivers(&self, pkt: &Packet) -> Vec<HostId> {
        self.fabric.receivers(pkt)
    }

    /// Deliver `pkt` to `host`'s stack, bypassing routing. Pair with
    /// [`Net::receivers`].
    pub fn deliver_to(&mut self, host: HostId, pkt: Packet) {
        self.fabric.deliver_to(host, pkt);
    }

    /// Walk the installed rules for `pkt`. First non-`Pass` verdict
    /// wins; an empty rule chain returns `Verdict::Pass`.
    pub fn evaluate(&mut self, pkt: &Packet) -> Verdict {
        for rule in self.rules.values_mut() {
            match rule.on_packet(pkt) {
                Verdict::Pass => continue,
                v => return v,
            }
        }
        Verdict::Pass
    }

    /// Discard `host`'s sockets and queued packets without sending FIN or RST.
    /// Packets already drained into the harness are unaffected.
    pub fn reset_host(&mut self, host: HostId) {
        self.fabric.kernel_mut(host).reset();
    }

    /// Update the retransmit interval, in egress passes, for existing
    /// and future hosts after a harness changes link latency.
    pub fn set_retx_threshold(&mut self, threshold: u32) {
        self.fabric.set_retx_threshold(threshold.max(1));
    }

    /// Snapshot `host`'s socket table.
    /// Use [`netstat`] from code running inside an installed `Net`.
    pub fn netstat(&self, host: HostId) -> Netstat {
        netstat::snapshot(self.fabric.kernel(host))
    }

    /// Install a rule for the lifetime of this `Net`.
    /// Use [`rule`] inside the simulation for a rule removed on guard drop.
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

    fn lookup_name(&self, name: &str) -> Option<std::net::IpAddr> {
        self.resolver
            .as_ref()
            .and_then(|resolve| resolve(name))
            .or_else(|| self.dns.lookup(name))
    }

    /// Install this network on the current thread.
    ///
    /// # Panics
    ///
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
            .field("resolver", &self.resolver.is_some())
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
/// An installed network. Dropping the guard removes and drops the network.
///
/// Use [`EnterGuard::exit`] to remove it while retaining its state.
pub struct EnterGuard {
    _priv: (),
}

impl EnterGuard {
    /// Remove and return the installed network, preserving its state.
    /// Call [`Net::enter`] to resume it.
    pub fn exit(self) -> Net {
        let net = CURRENT.with(|c| c.borrow_mut().take().expect("guard is live"));
        std::mem::forget(self);
        net
    }

    /// Drain every host's outbound queue into `out`.
    /// The caller applies [`EnterGuard::evaluate`] to each packet, then
    /// delivers, schedules, or drops it. See [`fixture`] for a scheduler.
    pub fn egress_all(&self, out: &mut Vec<Packet>) {
        CURRENT.with(|c| {
            c.borrow_mut()
                .as_mut()
                .expect("guard is live")
                .fabric
                .egress_all(out)
        });
    }

    /// Route unicast to the host owning its destination IP, or fan out
    /// UDP broadcast/multicast to other hosts on the shared link.
    /// Unknown unicast destinations are silently dropped.
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

    /// Select the host for subsequent socket operations on this thread.
    pub fn set_current(&self, id: HostId) {
        CURRENT.with(|c| {
            c.borrow_mut().as_mut().expect("guard is live").current = Some(id);
        });
    }

    /// Install a rule until the returned [`RuleGuard`] is dropped.
    /// Use [`rule`] when this guard is not in scope.
    pub fn rule(&self, r: impl Rule) -> RuleGuard {
        RuleGuard::new(install_rule(Box::new(r)))
    }
}

impl Drop for EnterGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = None);
    }
}

/// Like [`sys`], but a no-op when no `Net` or current host is
/// installed. For `Drop` impls: sockets may outlive their simulation
/// during teardown, and there is nothing left to close.
pub(crate) fn sys_if_installed(f: impl FnOnce(&mut Kernel)) {
    CURRENT.with(|c| {
        let mut cell = c.borrow_mut();
        let Some(net) = cell.as_mut() else { return };
        let Some(id) = net.current else { return };
        if let Some(kernel) = net.fabric.try_kernel_mut(id) {
            f(kernel);
        }
    })
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

/// Resolve `name` using the installed network's resolver and built-in DNS.
/// Returns `None` if no network is installed or neither can resolve the name.
pub fn lookup_host(name: &str) -> Option<std::net::IpAddr> {
    CURRENT.with(|c| c.borrow().as_ref().and_then(|net| net.lookup_name(name)))
}

/// Select the host for subsequent socket operations on this thread.
///
/// Equivalent to [`EnterGuard::set_current`]. A harness sharing a runtime
/// across hosts must select the correct host before each task poll.
///
/// # Panics
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
            .try_to_ip_addr_with(&net.dns, &|name| net.lookup_name(name))
            .expect("hostname not registered");
        let id = net
            .fabric
            .host_for_ip(ip)
            .unwrap_or_else(|| panic!("no host registered for {ip}"));
        netstat::snapshot(net.fabric.kernel(id))
    })
}
