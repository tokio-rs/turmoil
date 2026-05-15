//! Completion queue types mirroring [`io_uring::cqueue`](https://docs.rs/io-uring/0.7/io_uring/cqueue).

use crate::fs::FsContext;
use std::os::fd::RawFd;

/// Sealed trait selecting between [`Entry`] (default) and the larger
/// `Entry32` shape exposed by the real crate. Only `Entry` is
/// implemented in the simulation.
pub trait EntryMarker: private::Sealed + Sized {}

/// One completion queue entry. Yielded by iterating a
/// [`CompletionQueue`].
#[derive(Clone, Debug)]
pub struct Entry {
    pub(crate) user_data: u64,
    pub(crate) result: i32,
    pub(crate) flags: u32,
}

impl Entry {
    /// User-data tag set on the originating SQE.
    pub fn user_data(&self) -> u64 {
        self.user_data
    }

    /// Op result: non-negative on success (e.g. bytes read), negative
    /// `errno` on failure.
    pub fn result(&self) -> i32 {
        self.result
    }

    /// Per-CQE flags. The simulation always reports zero.
    pub fn flags(&self) -> u32 {
        self.flags
    }
}

impl EntryMarker for Entry {}

/// Completion queue handle. Iterating it yields any CQEs whose
/// scheduled completion time has elapsed in simulated time, in
/// RNG-shuffled order.
///
/// # Real-Linux fidelity: `sync()` is required before iterating
///
/// On real Linux, [`Self::sync`] copies the kernel's CQ head pointer
/// into userspace; without it, `next()` returns `None` even when the
/// kernel has posted CQEs. The simulation enforces the same contract:
/// a freshly-constructed `CompletionQueue` has `visible == 0`, and
/// `next()` returns `None` until `sync()` exposes the matured CQEs.
/// This catches at sim time the bug where a consumer forgets to
/// `sync()` and silently misses CQEs on real Linux.
///
/// # Divergence from real Linux: side effects run at `next()` time
///
/// `next()` is where the read/write/fsync side effect actually runs
/// against the simulated `Fs`. On real Linux, the kernel has already
/// completed the op by the time the CQE is in the ring. This means
/// in the simulation, dropping the `CompletionQueue` mid-iteration
/// leaves any not-yet-yielded matured ops un-executed (their effects
/// are queued but not staged into `Fs`). Drain to completion or hold
/// the iterator across the full set of expected CQEs.
pub struct CompletionQueue<'a> {
    ring_fd: RawFd,
    /// Number of CQEs the most recent `sync()` exposed for this
    /// handle. `next()` decrements this on each yield; when it hits
    /// zero, further `next()` calls return `None` until `sync()` is
    /// called again. `None` means "not yet synced," which behaves
    /// like zero.
    visible: Option<usize>,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> CompletionQueue<'a> {
    pub(crate) fn new(ring_fd: RawFd) -> Self {
        Self {
            ring_fd,
            visible: None,
            _lifetime: std::marker::PhantomData,
        }
    }

    /// Snapshot the ring: subsequent `next()` calls may yield up to
    /// the number of CQEs currently matured. Mirrors the real crate's
    /// `sync()` which copies the kernel CQ head into userspace.
    pub fn sync(&mut self) {
        self.visible = Some(FsContext::current(|ctx| {
            ctx.fs
                .io_uring_rings
                .get(&self.ring_fd)
                .map(|r| r.ready_cq_count(ctx.now))
                .unwrap_or(0)
        }));
    }

    /// Number of CQEs the last `sync()` exposed and not yet drained.
    /// Returns `0` for a freshly-constructed handle (matches real
    /// Linux: nothing visible until you sync).
    pub fn len(&self) -> usize {
        self.visible.unwrap_or(0)
    }

    /// Whether [`Self::len`] is zero.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<'a> Iterator for CompletionQueue<'a> {
    type Item = Entry;

    fn next(&mut self) -> Option<Entry> {
        // Real Linux only yields CQEs the most recent `sync()`
        // exposed. We enforce the same: a consumer that forgets to
        // call `sync()` before iterating sees an empty CQ even if
        // ops have matured.
        let remaining = self.visible.as_mut()?;
        if *remaining == 0 {
            return None;
        }
        let entry = FsContext::current(|ctx| {
            let now = ctx.now;
            let ring_fd = self.ring_fd;
            // Borrow the ring mutably and pop one ready scheduled op.
            let scheduled = ctx
                .fs
                .io_uring_rings
                .get_mut(&ring_fd)?
                .pop_ready(now, ctx.rng)?;
            // Execute the op against the fs (this is where reads,
            // writes, and fsync touch persisted/pending state). We
            // re-borrow `ctx` because `apply` needs `&mut Fs` and the
            // `RingState` is no longer in scope.
            let result = scheduled.apply.execute(ctx.fs, ctx.rng, now);
            Some(Entry {
                user_data: scheduled.user_data,
                result,
                flags: 0,
            })
        })?;
        *remaining -= 1;
        Some(entry)
    }
}

mod private {
    pub trait Sealed {}
    impl Sealed for super::Entry {}
}
