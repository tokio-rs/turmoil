//! Per-host io_uring state and context accessor.
//!
//! Mirrors [`turmoil_fs::FsContext`] for the ring side: a thread-local
//! gate into the current host's [`IoUringHostState`], with a parallel
//! `with_fs_and_io_uring` helper for the call sites that need both at
//! the same time (CQE drain executes ops against `Fs` while holding
//! ring state; submit-side schedules a CQE after sampling fs latency).

use super::sim::RingState;
use indexmap::IndexMap;
use std::os::fd::RawFd;
use std::time::Duration;

/// Base for sim-allocated ring fds. Mirrors `turmoil_fs::SIM_FD_BASE`
/// so both file fds and ring fds occupy distinct subranges of the
/// "high" sim-fd region — fs takes the bottom, io_uring the bottom +
/// `IO_URING_FD_OFFSET`. Kept in this crate so `turmoil-io_uring`
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

// ─── Host accessor: dependency-inversion hook ──────────────────────
//
// Same shape `turmoil-fs` uses. The embedder (`turmoil`) installs
// closures once at startup; runtime sites (Submitter, AsyncFd,
// CompletionQueue, IoUring::new/Drop) call through them.

/// Borrow handed to the io_uring-only accessor closure.
pub struct HostBorrow<'a> {
    pub io_uring: &'a mut IoUringHostState,
    pub now: Duration,
}

/// HRTB-erased closure shape for [`HostBorrow`].
pub trait HostBorrowFn: for<'a> FnMut(HostBorrow<'a>) {}
impl<F: for<'a> FnMut(HostBorrow<'a>)> HostBorrowFn for F {}

type AccessorFn = fn(&mut dyn HostBorrowFn);
type TryAccessorFn = fn(&mut dyn HostBorrowFn);

static HOST_ACCESSOR: std::sync::OnceLock<AccessorFn> = std::sync::OnceLock::new();
static TRY_HOST_ACCESSOR: std::sync::OnceLock<TryAccessorFn> = std::sync::OnceLock::new();

/// Install the io_uring host accessor. Idempotent.
pub fn install_host_accessor(current: AccessorFn, current_if_set: TryAccessorFn) {
    let _ = HOST_ACCESSOR.set(current);
    let _ = TRY_HOST_ACCESSOR.set(current_if_set);
}

fn host_accessor() -> AccessorFn {
    *HOST_ACCESSOR
        .get()
        .expect("turmoil-io_uring: host accessor not installed (call install_host_accessor)")
}

fn try_host_accessor() -> Option<TryAccessorFn> {
    TRY_HOST_ACCESSOR.get().copied()
}

/// Borrowed handle to the current host's io_uring state plus the
/// ambient sim time. Mirrors `turmoil_fs::FsContext` but scoped to
/// ring state alone — call sites that also need `&mut Fs` use
/// [`with_fs_and_io_uring`] instead.
pub(crate) struct IoUringContext<'a> {
    pub(crate) io_uring: &'a mut IoUringHostState,
    pub(crate) now: Duration,
}

impl IoUringContext<'_> {
    /// Run `f` with the current host's io_uring context.
    ///
    /// Panics if no simulation context is active.
    pub(crate) fn current<R>(f: impl FnOnce(IoUringContext<'_>) -> R) -> R {
        let f_cell = std::cell::Cell::new(Some(f));
        let mut out: Option<R> = None;
        let mut take = |borrow: HostBorrow<'_>| {
            let f = f_cell.take().expect("host accessor invoked closure twice");
            out = Some(f(IoUringContext {
                io_uring: borrow.io_uring,
                now: borrow.now,
            }));
        };
        host_accessor()(&mut take);
        out.expect("host accessor did not run closure")
    }

    /// Run `f` if a simulation context is set; otherwise no-op. Used in
    /// drop paths where the simulation may already be shutting down.
    pub(crate) fn current_if_set(f: impl FnOnce(IoUringContext<'_>)) {
        if let Some(accessor) = try_host_accessor() {
            let f_cell = std::cell::Cell::new(Some(f));
            let mut take = |borrow: HostBorrow<'_>| {
                if let Some(f) = f_cell.take() {
                    f(IoUringContext {
                        io_uring: borrow.io_uring,
                        now: borrow.now,
                    });
                }
            };
            accessor(&mut take);
        }
    }
}

// ─── Combined fs+io_uring accessor (only available with `fs` feature) ──
//
// Sites in `submit::schedule_pending` and `cqueue::CompletionQueue::next`
// need to lock fs and io_uring at the same time: submit walks the SQ
// and samples fs latency before scheduling; CQE drain executes
// `PendingApply::execute(&mut Fs, ...)` after popping the matured op.
//
// Lock order is fs-first, io_uring-second; any future combined-borrow
// follows the same order.

#[cfg(feature = "fs")]
pub(crate) use combined::with_fs_and_io_uring;
#[cfg(feature = "fs")]
pub use combined::{install_combined_accessor, FsIoUringBorrow, FsIoUringFn};

#[cfg(feature = "fs")]
mod combined {
    use super::IoUringHostState;
    use std::time::Duration;
    use turmoil_fs::Fs;

    /// Borrow handed to the combined fs+io_uring accessor.
    pub struct FsIoUringBorrow<'a> {
        pub fs: &'a mut Fs,
        pub io_uring: &'a mut IoUringHostState,
        pub rng: &'a mut dyn rand::RngCore,
        pub now: Duration,
    }

    pub trait FsIoUringFn: for<'a> FnMut(FsIoUringBorrow<'a>) {}
    impl<F: for<'a> FnMut(FsIoUringBorrow<'a>)> FsIoUringFn for F {}

    type CombinedFn = fn(&mut dyn FsIoUringFn);
    static COMBINED_ACCESSOR: std::sync::OnceLock<CombinedFn> = std::sync::OnceLock::new();

    /// Install the combined fs+io_uring accessor. Idempotent.
    pub fn install_combined_accessor(accessor: CombinedFn) {
        let _ = COMBINED_ACCESSOR.set(accessor);
    }

    fn combined_accessor() -> CombinedFn {
        *COMBINED_ACCESSOR
            .get()
            .expect("turmoil-io_uring: combined fs+io_uring accessor not installed")
    }

    /// Run `f` with mutable borrows of both `Fs` and
    /// `IoUringHostState` plus the ambient rng and time.
    pub(crate) fn with_fs_and_io_uring<R>(
        f: impl FnOnce(&mut Fs, &mut IoUringHostState, &mut dyn rand::RngCore, Duration) -> R,
    ) -> R {
        let f_cell = std::cell::Cell::new(Some(f));
        let mut out: Option<R> = None;
        let mut take = |borrow: FsIoUringBorrow<'_>| {
            let f = f_cell
                .take()
                .expect("combined accessor invoked closure twice");
            out = Some(f(borrow.fs, borrow.io_uring, borrow.rng, borrow.now));
        };
        combined_accessor()(&mut take);
        out.expect("combined accessor did not run closure")
    }
}
