//! Per-host io_uring state. The embedder enters the host registry once per
//! tick, and each ring operation locks it for the duration of the call.

use super::sim::RingState;
use indexmap::IndexMap;
use std::cell::{Cell, RefCell};
use std::io::{Error, ErrorKind, Result};
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use turmoil_fs::{FsContext, FsState};

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
    /// snapshot. The embedder calls this when the host's software stops,
    /// whether the host crashed or the software is being restarted.
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

type Registry = Arc<Mutex<IoUringHostState>>;

thread_local! {
    static CURRENT_IOU_ARC: RefCell<Option<Registry>> = const { RefCell::new(None) };
    static CURRENT_NOW: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

/// Per-tick context handed to [`enter`].
pub struct EnterCtx {
    /// Simulated time elapsed since unix epoch on the current host.
    pub now: Duration,
}

/// Restores the previous ring registry and clock on drop.
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

/// Enter this host's ring registry on the calling thread.
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

fn current() -> Result<Registry> {
    CURRENT_IOU_ARC
        .with_borrow(Clone::clone)
        .ok_or_else(unregistered)
}

fn unregistered() -> Error {
    Error::new(
        ErrorKind::NotFound,
        "ring is not registered on the entered host",
    )
}

/// A raw fd identifies a ring only within the registry that allocated it.
pub(crate) struct RingId {
    host: Weak<Mutex<IoUringHostState>>,
    pub(crate) fd: RawFd,
}

impl RingId {
    pub(crate) fn register(depth: u32) -> Result<Self> {
        let host = current()?;
        let mut state = host.lock().expect("IoUringHostState mutex poisoned");
        let fd = state.alloc_ring_fd();
        state.rings.insert(fd, RingState::new(depth));
        Ok(Self {
            host: Arc::downgrade(&host),
            fd,
        })
    }

    pub(crate) fn from_fd(fd: RawFd) -> Result<Self> {
        let host = Arc::downgrade(&current()?);
        Ok(Self { host, fd })
    }

    fn registry(&self) -> Result<Registry> {
        let host = current()?;
        std::ptr::eq(self.host.as_ptr(), Arc::as_ptr(&host))
            .then_some(host)
            .ok_or_else(unregistered)
    }

    fn access<R>(&self, host: &Registry, f: impl FnOnce(&mut RingState) -> R) -> Result<R> {
        let mut state = host.lock().expect("IoUringHostState mutex poisoned");
        let ring = state.rings.get_mut(&self.fd).ok_or_else(unregistered)?;
        Ok(f(ring))
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut RingState, Duration) -> R) -> Result<R> {
        self.access(&self.registry()?, |ring| f(ring, CURRENT_NOW.get()))
    }

    pub(crate) fn with_fs<R>(
        &self,
        f: impl FnOnce(&mut FsState, &mut RingState, Duration) -> R,
    ) -> Result<R> {
        let host = self.registry()?;
        // The filesystem lock comes first. Its context dispatches user hooks
        // after both this registry guard and the filesystem guard are gone.
        FsContext::current(|ctx| self.access(&host, |ring| f(ctx.fs, ring, ctx.now)))
    }

    pub(crate) fn close(&self) {
        let Some(host) = self.host.upgrade() else {
            return;
        };
        let Ok(mut state) = host.lock() else { return };
        if let Some(ring) = state.rings.swap_remove(&self.fd) {
            ring.cq_notify.notify_waiters();
        }
    }
}
