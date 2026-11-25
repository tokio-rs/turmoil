//! Submitter handle mirroring [`io_uring::Submitter`](https://docs.rs/io-uring/0.7/io_uring/struct.Submitter.html).

use super::host::RingId;
use super::sim::{PendingApply, RingState, EINVAL};
use super::squeue::OpKind;
use super::types::SubmitArgs;
use std::io;
use std::time::Duration;
use turmoil_fs::FsState;

/// Handle for the submit side of the ring.
///
/// In the simulation, `submit()` walks the SQ, schedules a per-op
/// completion timestamp drawn from the configured I/O latency
/// distribution, and returns. CQEs become observable via the
/// [`crate::cqueue::CompletionQueue`] iterator as simulated
/// time advances past each timestamp.
pub struct Submitter<'a> {
    ring: &'a RingId,
}

impl<'a> Submitter<'a> {
    pub(crate) fn new(ring: &'a RingId) -> Self {
        Self { ring }
    }

    /// Push every queued SQE into the in-flight set.
    ///
    /// Returns the number of SQEs accepted (matching the real kernel,
    /// which may submit fewer than queued). The simulation always
    /// accepts the entire SQ.
    pub fn submit(&self) -> io::Result<usize> {
        self.ring.with_fs(schedule_pending)
    }

    /// `submit()`, then in the real kernel block the calling thread
    /// until at least `want` CQEs are ready.
    ///
    /// In the simulation the call is sync and returns as soon as the
    /// SQ has been scheduled — there is no thread to block, and the
    /// turmoil runtime advances simulated time only on `await`
    /// points. Callers awaiting CQEs should use
    /// [`crate::AsyncFd::readable`] (which honors simulated
    /// time) instead. The `want` argument is accepted for API
    /// compatibility but has no in-sim effect.
    pub fn submit_and_wait(&self, want: usize) -> io::Result<usize> {
        self.submit_with_args(want, &SubmitArgs::default())
    }

    /// Submit + bounded wait, mirroring the real
    /// `Submitter::submit_with_args`. `args.timeout` is recorded but
    /// not honored: the simulation cannot block sync code on
    /// simulated time. If a test needs a timeout, await
    /// [`crate::AsyncFd::readable`] inside a
    /// `tokio::time::timeout` — both fire under simulated time and
    /// give the same observable behavior as the real kernel call.
    pub fn submit_with_args(&self, _want: usize, args: &SubmitArgs<'_>) -> io::Result<usize> {
        // Validate the timespec shape: tv_nsec must be < 1_000_000_000
        // per real-kernel rules. Surfaces an EINVAL on the call
        // itself, before any SQE is scheduled.
        if let Some(ts) = args.timeout {
            if ts.nsec >= 1_000_000_000 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SubmitArgs::timespec: nsec must be < 1_000_000_000",
                ));
            }
        }
        self.submit()
    }
}

/// Walk the ring's SQ; for each entry, sample latency and either
/// schedule a future CQE or post an immediate `EINVAL` CQE for
/// unsupported ops.
fn schedule_pending(fs: &mut FsState, ring: &mut RingState, now: Duration) -> usize {
    let accepted = ring.sq.len();
    while let Some(entry) = ring.sq.pop_front() {
        if entry.flags.has_unsupported() {
            ring.post_immediate_error(entry.user_data, -EINVAL, now);
            continue;
        }
        match entry.op {
            OpKind::Transfer(io) => {
                let io = io.issue(fs);
                let when = now + io.latency(fs);
                ring.schedule(entry.user_data, when, PendingApply::Transfer(io));
            }
            OpKind::Fsync { fd } => {
                let latency = fs.calculate_latency(false);
                ring.schedule(entry.user_data, now + latency, PendingApply::Fsync { fd });
            }
            OpKind::AsyncCancel { target_user_data } => {
                ring.cancel(entry.user_data, target_user_data, now);
            }
        }
    }
    accepted
}
