//! Submitter handle mirroring [`io_uring::Submitter`](https://docs.rs/io-uring/0.7/io_uring/struct.Submitter.html).

use super::sim::PendingApply;
use super::squeue::{Entry, OpKind};
use super::types::SubmitArgs;
use crate::fs::FsContext;
use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

/// Handle for the submit side of the ring.
///
/// In the simulation, `submit()` walks the SQ, schedules a per-op
/// completion timestamp drawn from the configured I/O latency
/// distribution, and returns. CQEs become observable via the
/// [`crate::io_uring::cqueue::CompletionQueue`] iterator as simulated
/// time advances past each timestamp.
pub struct Submitter<'a> {
    ring_fd: RawFd,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> Submitter<'a> {
    pub(crate) fn new(ring_fd: RawFd) -> Self {
        Self {
            ring_fd,
            _lifetime: std::marker::PhantomData,
        }
    }

    /// Push every queued SQE into the in-flight set.
    ///
    /// Returns the number of SQEs accepted (matching the real kernel,
    /// which may submit fewer than queued). The simulation always
    /// accepts the entire SQ.
    pub fn submit(&self) -> io::Result<usize> {
        FsContext::current(|ctx| schedule_pending(ctx.fs, ctx.rng, ctx.now, self.ring_fd))
    }

    /// `submit()`, then in the real kernel block the calling thread
    /// until at least `want` CQEs are ready.
    ///
    /// In the simulation the call is sync and returns as soon as the
    /// SQ has been scheduled — there is no thread to block, and the
    /// turmoil runtime advances simulated time only on `await`
    /// points. Callers awaiting CQEs should use
    /// [`crate::io_uring::AsyncFd::readable`] (which honors simulated
    /// time) instead. The `want` argument is accepted for API
    /// compatibility but has no in-sim effect.
    pub fn submit_and_wait(&self, want: usize) -> io::Result<usize> {
        self.submit_with_args(want, &SubmitArgs::default())
    }

    /// Submit + bounded wait, mirroring the real
    /// `Submitter::submit_with_args`. `args.timeout` is recorded but
    /// not honored: the simulation cannot block sync code on
    /// simulated time. If a test needs a timeout, await
    /// [`crate::io_uring::AsyncFd::readable`] inside a
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
        FsContext::current(|ctx| schedule_pending(ctx.fs, ctx.rng, ctx.now, self.ring_fd))
    }
}

/// Walk the ring's SQ; for each entry, sample latency and either
/// schedule a future CQE or post an immediate `EINVAL` CQE for
/// unsupported ops.
fn schedule_pending(
    fs: &mut crate::fs::Fs,
    rng: &mut dyn rand::RngCore,
    now: Duration,
    ring_fd: RawFd,
) -> io::Result<usize> {
    // Drain SQ first (single mutable borrow), then process each entry
    // by re-borrowing the ring on demand. Latency sampling needs
    // `&Fs`, so we cannot keep `&mut RingState` live across it.
    let entries: Vec<Entry> = match fs.io_uring_rings.get_mut(&ring_fd) {
        Some(ring) => ring.sq.drain(..).collect(),
        None => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "io_uring ring is no longer registered",
            ));
        }
    };

    let mut accepted = 0usize;
    for entry in entries {
        accepted += 1;
        if entry.flags.has_unsupported() {
            let ring = fs.io_uring_rings.get_mut(&ring_fd).expect("ring vanished");
            ring.post_immediate_error(entry.user_data, libc_einval(), now);
            continue;
        }
        match entry.op {
            OpKind::Read {
                fd,
                ptr,
                len,
                offset,
            } => {
                // Mirror shim::tokio::fs::apply_read_latency: O_DIRECT
                // bypasses the cache; otherwise probe `access()` and
                // bump the LRU via `insert()`. We sample latency with
                // the resulting cache_hit so reads of recently-touched
                // offsets are near-instant.
                //
                // Divergence: on real Linux, two reads at the same
                // offset submitted in one batch both miss the cache
                // until the first completes. Here, the second sees a
                // hit because we insert at submit time. This matches
                // how the existing tokio shim models the cache —
                // the simulation is internally consistent with
                // itself even where it differs from the kernel.
                let direct_io = fs.direct_io_fds.contains(&fd);
                let cache_hit = if direct_io {
                    false
                } else if let Some(path) = fs.open_handles.get(&fd).cloned() {
                    if let Some(cache) = &mut fs.page_cache {
                        let hit = cache.access(&path, offset, rng);
                        cache.insert(&path, offset);
                        hit
                    } else {
                        false
                    }
                } else {
                    // Bad fd; let exec_read surface -EBADF later. No
                    // sensible cache state to consult.
                    false
                };
                let latency = fs.calculate_latency(rng, cache_hit);
                let ring = fs.io_uring_rings.get_mut(&ring_fd).expect("ring vanished");
                ring.schedule(
                    entry.user_data,
                    now + latency,
                    PendingApply::Read {
                        fd,
                        ptr,
                        len,
                        offset,
                    },
                );
            }
            OpKind::Write {
                fd,
                ptr,
                len,
                offset,
            } => {
                // Mirror shim::tokio::fs::apply_write_latency: writes
                // populate the cache (write-through) unless O_DIRECT.
                // Latency itself never sees a cache hit on writes —
                // they always wait for disk.
                let direct_io = fs.direct_io_fds.contains(&fd);
                if !direct_io {
                    if let Some(path) = fs.open_handles.get(&fd).cloned() {
                        if let Some(cache) = &mut fs.page_cache {
                            cache.insert(&path, offset);
                        }
                    }
                }
                let latency = fs.calculate_latency(rng, false);
                let ring = fs.io_uring_rings.get_mut(&ring_fd).expect("ring vanished");
                ring.schedule(
                    entry.user_data,
                    now + latency,
                    PendingApply::Write {
                        fd,
                        ptr,
                        len,
                        offset,
                    },
                );
            }
            OpKind::Fsync { fd } => {
                let latency = fs.calculate_latency(rng, false);
                let ring = fs.io_uring_rings.get_mut(&ring_fd).expect("ring vanished");
                ring.schedule(entry.user_data, now + latency, PendingApply::Fsync { fd });
            }
            OpKind::AsyncCancel { target_user_data } => {
                let ring = fs.io_uring_rings.get_mut(&ring_fd).expect("ring vanished");
                ring.cancel(entry.user_data, target_user_data, now);
            }
        }
    }
    Ok(accepted)
}

/// `EINVAL` as a negative `errno`, the form CQE results take for
/// errors. Linux's value; we use the constant directly to avoid
/// pulling in libc on non-Linux targets.
fn libc_einval() -> i32 {
    -22
}
