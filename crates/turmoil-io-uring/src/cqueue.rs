//! Completion queue types mirroring [`io_uring::cqueue`](https://docs.rs/io-uring/0.7/io_uring/cqueue).

#[cfg(feature = "fs")]
use crate::host::RingId;

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
#[cfg(feature = "fs")]
pub struct CompletionQueue<'a> {
    ring: &'a RingId,
    /// Number of CQEs the most recent `sync()` exposed for this
    /// handle. `next()` decrements this on each yield; when it hits
    /// zero, further `next()` calls return `None` until `sync()` is
    /// called again.
    visible: usize,
}

#[cfg(feature = "fs")]
impl<'a> CompletionQueue<'a> {
    pub(crate) fn new(ring: &'a RingId) -> Self {
        Self { ring, visible: 0 }
    }

    /// Snapshot the ring: subsequent `next()` calls may yield up to
    /// the number of CQEs currently matured. Mirrors the real crate's
    /// `sync()` which copies the kernel CQ head into userspace.
    pub fn sync(&mut self) {
        self.visible = self.ring.with(|r, now| r.ready_cq_count(now)).unwrap_or(0);
    }

    /// Number of CQEs the last `sync()` exposed and not yet drained.
    /// Returns `0` for a freshly-constructed handle (matches real
    /// Linux: nothing visible until you sync).
    pub fn len(&self) -> usize {
        self.visible
    }

    /// Whether [`Self::len`] is zero.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(feature = "fs")]
impl<'a> Iterator for CompletionQueue<'a> {
    type Item = Entry;

    fn next(&mut self) -> Option<Entry> {
        // Real Linux only yields CQEs the most recent `sync()`
        // exposed. We enforce the same: a consumer that forgets to
        // call `sync()` before iterating sees an empty CQ even if
        // ops have matured.
        if self.visible == 0 {
            return None;
        }
        let ready = self.ring.with_fs(|fs, ring, now| {
            let scheduled = ring.pop_ready(now, &mut *fs.rng)?;
            let result = scheduled.apply.execute(fs);
            Some(Entry {
                user_data: scheduled.user_data,
                result,
                flags: 0,
            })
        });
        let entry = ready.ok().flatten()?;
        self.visible -= 1;
        Some(entry)
    }
}

mod private {
    pub trait Sealed {}
    impl Sealed for super::Entry {}
}
