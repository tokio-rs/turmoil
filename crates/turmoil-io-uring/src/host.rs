//! Per-host io_uring state and the `enter` thread-local.
//!
//! Mirrors `turmoil_fs::enter`: the embedder calls
//! [`enter`](super::enter) once per host tick, which locks the
//! supplied `Arc<Mutex<IoUringHostState>>` and sets a thread-local
//! pointer. While the returned guard is alive, runtime sites
//! (`Submitter`, `AsyncFd`, `CompletionQueue`, `IoUring::new`/`Drop`)
//! reach the current ring state via [`IoUringContext::current`].

use super::sim::RingState;
use indexmap::IndexMap;
use std::cell::Cell;
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Base for sim-allocated ring fds. Mirrors `turmoil_fs::SIM_FD_BASE`
/// so both file fds and ring fds occupy distinct subranges of the
/// "high" sim-fd region — fs takes the bottom, io_uring the bottom +
/// `IO_URING_FD_OFFSET`. Kept in this crate so `turmoil-io-uring`
/// without `fs` still builds.
const SIM_FD_BASE: RawFd = 1 << 30;

/// Offset above [`SIM_FD_BASE`] for io_uring ring fds. Keeps file fds
/// and ring fds in non-overlapping ranges so a stray fd in a panic
/// message hints at which subsystem allocated it.
const IO_URING_FD_OFFSET: RawFd = 1 << 20;

/// Per-host io_uring state.
///
/// Owned by the embedding harness (typically `turmoil`'s `Host`),
/// behind an `Arc<Mutex<...>>`. Holds every active ring on the host
/// plus a counter for fresh ring fds.
pub struct IoUringHostState {
    /// Active rings keyed by sim ring fd.
    pub(crate) rings: IndexMap<RawFd, RingState>,
    /// Counter for ring fd allocation. Bumped past `SIM_FD_BASE` by
    /// an extra offset so io_uring fds and fs file fds occupy distinct
    /// (visually-distinguishable) ranges — purely a debugging aid.
    next_ring_fd: RawFd,
}

impl IoUringHostState {
    /// Construct a fresh per-host state.
    pub fn new() -> Self {
        Self {
            rings: IndexMap::new(),
            next_ring_fd: SIM_FD_BASE + IO_URING_FD_OFFSET,
        }
    }

    /// Allocate a fresh ring fd in io_uring's range.
    pub(crate) fn alloc_ring_fd(&mut self) -> RawFd {
        let fd = self.next_ring_fd;
        self.next_ring_fd = self
            .next_ring_fd
            .checked_add(1)
            .expect("io_uring fd counter overflowed RawFd range");
        fd
    }

    /// Drop every ring on this host and wake any [`super::AsyncFd`]
    /// waiters so they observe an empty registry on their next
    /// snapshot. Called from the embedder's crash path.
    pub fn crash(&mut self) {
        for ring in self.rings.values() {
            ring.cq_notify.notify_waiters();
        }
        self.rings.clear();
    }
}

impl Default for IoUringHostState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Enter pattern ──────────────────────────────────────────────────
//
// `enter` locks the supplied `Arc<Mutex<IoUringHostState>>` and stores
// a `*mut IoUringHostState` (plus `now`) in a thread-local. Runtime
// sites read from there via `IoUringContext::current`. Mirrors
// `turmoil_fs::enter`.

thread_local! {
    /// `Arc<Mutex<IoUringHostState>>` of the entered ring registry.
    /// Locked on each `IoUringContext::current` call; not held for
    /// the lifetime of the guard. See `turmoil_fs::enter` for why.
    static CURRENT_IOU_ARC: std::cell::RefCell<Option<Arc<Mutex<IoUringHostState>>>> =
        const { std::cell::RefCell::new(None) };
    static CURRENT_NOW: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

/// Per-tick context handed to [`enter`].
pub struct EnterCtx {
    /// Simulated time elapsed since unix epoch on the current host.
    pub now: Duration,
}

/// Guard returned by [`enter`]. While alive, the entered
/// `IoUringHostState` is the current one for [`IoUringContext::current`]
/// on this thread.
///
/// `enter` does NOT hold the mutex on the entered
/// `IoUringHostState` — it locks on each call to
/// `IoUringContext::current`. See `turmoil_fs::enter` for the
/// rationale.
#[must_use = "the entered IoUringHostState is only current while the guard is held"]
pub struct IoUringEnterGuard<'a> {
    prev_arc: Option<Arc<Mutex<IoUringHostState>>>,
    prev_now: Duration,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> Drop for IoUringEnterGuard<'a> {
    fn drop(&mut self) {
        CURRENT_IOU_ARC.with(|c| *c.borrow_mut() = self.prev_arc.take());
        CURRENT_NOW.with(|c| c.set(self.prev_now));
    }
}

/// Mark `arc`'s `IoUringHostState` as the current one for
/// [`IoUringContext::current`] on this thread.
///
/// `enter` calls nest. The embedder (typically `turmoil`) calls
/// `io_uring::enter` alongside `turmoil_fs::enter` at the start of
/// each host tick.
pub fn enter(arc: &Arc<Mutex<IoUringHostState>>, ctx: EnterCtx) -> IoUringEnterGuard<'_> {
    let prev_arc = CURRENT_IOU_ARC.with(|c| c.borrow_mut().replace(Arc::clone(arc)));
    let prev_now = CURRENT_NOW.with(|c| c.replace(ctx.now));
    IoUringEnterGuard {
        prev_arc,
        prev_now,
        _marker: std::marker::PhantomData,
    }
}

/// Borrowed handle to the current host's io_uring state plus the
/// ambient sim time. Call sites that also need `&mut Fs` use
/// [`with_fs_and_io_uring`] instead.
pub(crate) struct IoUringContext<'a> {
    pub(crate) io_uring: &'a mut IoUringHostState,
    pub(crate) now: Duration,
}

impl IoUringContext<'_> {
    /// Run `f` with the current host's io_uring context.
    ///
    /// Panics if no `enter` guard is currently held on this thread.
    pub(crate) fn current<R>(f: impl FnOnce(IoUringContext<'_>) -> R) -> R {
        let arc = CURRENT_IOU_ARC
            .with(|c| c.borrow().as_ref().map(Arc::clone))
            .expect("turmoil-io-uring: no IoUringHostState is current (call enter first)");
        let mut lock = arc.lock().expect("IoUringHostState mutex poisoned");
        let now = CURRENT_NOW.with(|c| c.get());
        f(IoUringContext {
            io_uring: &mut lock,
            now,
        })
    }

    /// Run `f` if an `enter` guard is currently held; otherwise no-op.
    /// Used in drop paths where the simulation may be shutting down.
    pub(crate) fn current_if_set(f: impl FnOnce(IoUringContext<'_>)) {
        let Some(arc) = CURRENT_IOU_ARC.with(|c| c.borrow().as_ref().map(Arc::clone)) else {
            return;
        };
        let mut lock = arc.lock().expect("IoUringHostState mutex poisoned");
        let now = CURRENT_NOW.with(|c| c.get());
        f(IoUringContext {
            io_uring: &mut lock,
            now,
        });
    }
}

// ─── Combined fs+io_uring borrow (only available with `fs` feature) ──
//
// Sites in `submit::schedule_pending` and `cqueue::CompletionQueue::next`
// need to mutate both `Fs` and `IoUringHostState` at once. Both
// thread-locals are populated by their respective `enter` calls during
// a host tick, so this just reads from both.

#[cfg(feature = "fs")]
pub(crate) fn with_fs_and_io_uring<R>(
    f: impl FnOnce(&mut turmoil_fs::Fs, &mut IoUringHostState, &mut dyn rand::RngCore, Duration) -> R,
) -> R {
    // Lock fs first (matches lock order documented elsewhere in this
    // crate). FsContext::current handles the fs lock; we lock
    // io_uring inline.
    turmoil_fs::FsContext::current(|ctx| {
        let arc = CURRENT_IOU_ARC
            .with(|c| c.borrow().as_ref().map(Arc::clone))
            .expect("turmoil-io-uring: no IoUringHostState is current (call enter first)");
        let mut iou_lock = arc.lock().expect("IoUringHostState mutex poisoned");
        // Split-borrow rng from `&mut Fs` so the closure can take both
        // `&mut Fs` and `&mut dyn RngCore`. SAFETY: `rng` is a distinct
        // field from any other Fs field the closure mutates.
        let rng_ptr: *mut dyn rand::RngCore = &mut *ctx.fs.rng;
        let rng = unsafe { &mut *rng_ptr };
        f(ctx.fs, &mut iou_lock, rng, ctx.now)
    })
}
