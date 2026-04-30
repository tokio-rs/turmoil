//! Time-driven packet scheduler for the tokio fixtures.
//!
//! The fabric is deliberately clock-free — it exposes egress/deliver
//! as sync primitives and lets the harness decide how packets flow
//! through time. This scheduler is that policy for our tokio-based
//! fixtures: monotonic sim clock, a sorted pending queue for delayed
//! delivery (`Verdict::Deliver(d)`), and a `tick(dt)` for driving the
//! fabric.

use std::time::Duration;

use crate::kernel::Packet;
use crate::rule::Verdict;
use crate::EnterGuard;

/// Packet held for future delivery (`Verdict::Deliver(d)` with d > 0).
#[derive(Debug)]
struct Scheduled {
    deliver_at: Duration,
    /// Monotonic tie-breaker so two packets sharing a deadline
    /// deliver in emission order.
    seq: u64,
    pkt: Packet,
}

/// Sim-clock packet scheduler. Owns the pending queue; drives the
/// fabric via [`EnterGuard`] primitives.
#[derive(Debug, Default)]
pub(crate) struct Scheduler {
    now: Duration,
    /// Kept sorted by `(deliver_at, seq)` so the ready prefix is a
    /// contiguous drain.
    pending: Vec<Scheduled>,
    next_seq: u64,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// One fabric tick:
    /// 1. Advance sim time by `dt`.
    /// 2. Deliver any scheduled packets whose deadline has come due.
    /// 3. Drain egress from every host, consult rules, and route
    ///    each packet (deliver now, schedule for later, or drop).
    pub fn tick(&mut self, guard: &EnterGuard, dt: Duration) {
        self.now += dt;

        let ready_end = self
            .pending
            .iter()
            .position(|s| s.deliver_at > self.now)
            .unwrap_or(self.pending.len());
        let ready: Vec<Scheduled> = self.pending.drain(..ready_end).collect();
        for s in ready {
            guard.deliver(s.pkt);
        }

        for pkt in guard.egress_all() {
            match guard.evaluate(&pkt) {
                Verdict::Drop => {}
                Verdict::Deliver(d) if d.is_zero() => guard.deliver(pkt),
                Verdict::Deliver(d) => self.schedule(pkt, d),
                Verdict::Pass => guard.deliver(pkt),
            }
        }
    }

    fn schedule(&mut self, pkt: Packet, delay: Duration) {
        let deliver_at = self.now + delay;
        let seq = self.next_seq;
        self.next_seq += 1;
        let entry = Scheduled {
            deliver_at,
            seq,
            pkt,
        };
        let idx = self
            .pending
            .binary_search_by(|s| (s.deliver_at, s.seq).cmp(&(entry.deliver_at, entry.seq)))
            .unwrap_or_else(|i| i);
        self.pending.insert(idx, entry);
    }
}
