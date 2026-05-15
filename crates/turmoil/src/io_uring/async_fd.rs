//! `AsyncFd` mirror for the simulated ring fd.
//!
//! Drives readiness off the per-ring `Notify` plus a
//! `tokio::time::sleep_until(next_deadline)` race. Both fire under
//! turmoil's simulated time, so consumers get correct wake
//! semantics without touching real epoll/kqueue/eventfd.

use crate::fs::FsContext;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::Interest;
use tokio::sync::Notify;
use tokio::time::Instant;

/// Mirrors [`tokio::io::unix::AsyncFd`].
///
/// Construction snapshots the ring's `Notify` so subsequent
/// [`AsyncFd::readable`] calls don't need a per-call ring lookup just
/// to resubscribe. If the ring is gone (e.g. host crashed and the
/// underlying state was cleared), [`AsyncFd::readable`] returns an
/// [`io::Error`].
pub struct AsyncFd<T: AsRawFd> {
    inner: T,
    notify: Arc<Notify>,
}

impl<T: AsRawFd> AsyncFd<T> {
    /// New AsyncFd registered for read interest.
    pub fn new(inner: T) -> io::Result<Self> {
        Self::with_interest(inner, Interest::READABLE)
    }

    /// New AsyncFd with a specific interest. The simulation only
    /// models READABLE; other interests are accepted but unused.
    pub fn with_interest(inner: T, _interest: Interest) -> io::Result<Self> {
        let fd = inner.as_raw_fd();
        let notify = FsContext::current(|ctx| {
            ctx.fs
                .io_uring_rings
                .get(&fd)
                .map(|r| r.cq_notify.clone())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("AsyncFd: fd {fd} is not a registered turmoil ring"),
                    )
                })
        })?;
        Ok(Self { inner, notify })
    }

    /// Reference to the wrapped value.
    pub fn get_ref(&self) -> &T {
        &self.inner
    }

    /// Mutable reference to the wrapped value.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Take ownership of the wrapped value.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Resolve when the ring has at least one ready CQE, mirroring
    /// [`tokio::io::unix::AsyncFd::readable`].
    pub async fn readable(&self) -> io::Result<AsyncFdReadyGuard<'_, T>> {
        let fd = self.inner.as_raw_fd();
        loop {
            // Subscribe BEFORE we sample state, so a `notify_waiters()`
            // call that races between the snapshot and the await is
            // observed. `notify_waiters()` only wakes registered
            // waiters, so a `Notified` future that hasn't been polled
            // yet won't see it. `Notified::enable()` registers without
            // requiring an `.await`, closing that race.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            // Snapshot under the fs lock: are we ready right now? If
            // not, what's the next deadline?
            let snapshot = FsContext::current(|ctx| -> io::Result<Snapshot> {
                let ring = ctx.fs.io_uring_rings.get(&fd).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "AsyncFd: ring is no longer registered",
                    )
                })?;
                let now = ctx.now;
                let ready = ring.ready_cq_count(now) > 0;
                let deadline = ring.next_deadline();
                Ok(Snapshot {
                    ready,
                    deadline,
                    now,
                })
            })?;

            if snapshot.ready {
                return Ok(AsyncFdReadyGuard { _async_fd: self });
            }

            match snapshot.deadline {
                Some(d) if d > snapshot.now => {
                    // sleep_until uses simulated time.
                    let until = Instant::now() + (d - snapshot.now);
                    tokio::select! {
                        _ = &mut notified => {}
                        _ = tokio::time::sleep_until(until) => {}
                    }
                }
                Some(_) => {
                    // Already due — loop and recheck. A bare yield
                    // hands control to the runtime so any pending
                    // submit() that would advance time can run.
                    tokio::task::yield_now().await;
                }
                None => {
                    // Nothing in flight. Wait indefinitely for the
                    // next submit() to push something.
                    notified.await;
                }
            }
        }
    }
}

struct Snapshot {
    ready: bool,
    deadline: Option<Duration>,
    now: Duration,
}

impl<T: AsRawFd> AsRawFd for AsyncFd<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

/// Mirrors [`tokio::io::unix::AsyncFdReadyGuard`].
pub struct AsyncFdReadyGuard<'a, T: AsRawFd> {
    _async_fd: &'a AsyncFd<T>,
}

impl<'a, T: AsRawFd> AsyncFdReadyGuard<'a, T> {
    /// No-op in the simulation. The real type re-arms the IO driver's
    /// readiness flag when the consumer drained the fd; in our model
    /// readiness is recomputed on every `readable()` call from ring
    /// state, so there is no flag to clear.
    pub fn clear_ready(&mut self) {}
}
