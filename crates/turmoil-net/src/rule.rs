//! Packet rules — the extension point for latency, drops, partitions,
//! and other fault injection.
//!
//! The fabric consults every installed rule, in install order, for
//! each non-loopback packet leaving a host. The first rule to return
//! a non-[`Verdict::Pass`] verdict wins; if every rule passes, the
//! packet is delivered immediately.
//!
//! Loopback is inline and skips rules — a single-host test doesn't
//! need fault injection on 127.0.0.1 traffic, and bypassing keeps the
//! fast path fast.
//!
//! # Installing
//!
//! ```ignore
//! // Free fn (from any task inside the Net): uninstalls on drop.
//! let guard = turmoil_net::rule(Latency::fixed(Duration::from_millis(10)));
//! // Or permanently at Net construction:
//! let net = Net::new();
//! net.install_permanent(Latency::fixed(/* .. */));
//! ```

use std::{marker::PhantomData, time::Duration};

use crate::{kernel::Packet, uninstall_rule};

/// What should happen to a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Defer to the next rule. If every rule passes, the packet is
    /// delivered with zero delay.
    Pass,
    /// Deliver the packet, optionally after `delay`. `Duration::ZERO`
    /// means "immediately" — functionally equivalent to the default
    /// no-rules behavior, but short-circuits any later rules.
    Deliver(Duration),
    /// Drop the packet silently.
    Drop,
}

/// Decides the fate of each packet the fabric sees.
///
/// Rules have exclusive `&mut` during evaluation, so they can hold
/// counters, RNG state, or other per-rule bookkeeping without
/// synchronization. Evaluation is deterministic: rules run in install
/// order, and for each packet the first non-[`Verdict::Pass`] wins.
pub trait Rule: 'static {
    fn on_packet(&mut self, pkt: &Packet) -> Verdict;
}

/// Handle returned by every installer. The `RuleId` is stable for the
/// life of the rule — uninstalling twice is a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuleId(pub(crate) u64);

/// A rule that delays every packet by a fixed duration.
///
/// Apply selectively by src/dst if you want one-way latency — the
/// matcher closure is called for each packet and receives the
/// endpoints.
#[derive(Debug, Clone, Copy)]
pub struct Latency {
    delay: Duration,
}

impl Latency {
    /// Delay every packet by `delay`. Returning a rule that matches
    /// only specific endpoints is left to the caller — wrap this in
    /// your own [`Rule`] impl and call `Latency::on_packet` from it,
    /// or pattern-match src/dst directly in a closure rule.
    pub fn fixed(delay: Duration) -> Self {
        Self { delay }
    }
}

impl Rule for Latency {
    fn on_packet(&mut self, _pkt: &Packet) -> Verdict {
        Verdict::Deliver(self.delay)
    }
}

/// Adapter that lifts a `FnMut(&Packet) -> Verdict` into a [`Rule`].
/// Lets tests write ad-hoc rules without a full type declaration.
impl<F> Rule for F
where
    F: FnMut(&Packet) -> Verdict + 'static,
{
    fn on_packet(&mut self, pkt: &Packet) -> Verdict {
        self(pkt)
    }
}

/// RAII handle for a rule installed via [`rule`] or [`EnterGuard::rule`].
/// Drop uninstalls the rule; [`RuleGuard::forget`] leaks it instead,
/// leaving the rule installed for the rest of the simulation.
///
/// The guard is `!Send`: it's tied to the thread that owns the `Net`,
/// and the thread-local uninstall on drop would be unsound across
/// threads.
#[must_use = "dropping the guard uninstalls the rule"]
pub struct RuleGuard {
    id: RuleId,
    _not_send: PhantomData<*const ()>,
}

impl RuleGuard {
    pub fn new(id: RuleId) -> Self {
        Self {
            id,
            _not_send: PhantomData,
        }
    }

    pub fn id(&self) -> RuleId {
        self.id
    }

    /// Leak the guard — the rule stays installed until the `Net` is
    /// dropped. Useful when an early-phase guard should survive the
    /// call that produced it.
    pub fn forget(self) {
        std::mem::forget(self);
    }
}

impl Drop for RuleGuard {
    fn drop(&mut self) {
        uninstall_rule(self.id);
    }
}
