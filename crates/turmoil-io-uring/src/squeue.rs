//! Submission queue types mirroring [`io_uring::squeue`](https://docs.rs/io-uring/0.7/io_uring/squeue).

#[cfg(feature = "fs")]
use crate::host::IoUringContext;
use std::os::fd::RawFd;

/// Sealed trait selecting between [`Entry`] (default) and the larger
/// `Entry128` shape exposed by the real crate. Only `Entry` is
/// implemented in the simulation.
pub trait EntryMarker: private::Sealed + Sized {}

/// One submission queue entry. Built by [`crate::opcode`]
/// builders and pushed onto a [`SubmissionQueue`].
///
/// The simulation does not preserve the kernel's wire layout — the
/// fields are whatever the simulation needs to execute the op at
/// completion time. Source-level compatibility is preserved: every
/// method the real crate's `Entry` exposes (currently `user_data`,
/// `flags`) returns `Entry` and is chainable.
#[derive(Clone, Debug)]
pub struct Entry {
    #[cfg_attr(not(feature = "fs"), allow(dead_code))]
    pub(crate) op: OpKind,
    pub(crate) user_data: u64,
    pub(crate) flags: Flags,
}

impl Entry {
    /// Replace the `user_data` tag posted in the resulting CQE.
    pub fn user_data(mut self, ud: u64) -> Self {
        self.user_data = ud;
        self
    }

    /// Replace the per-op flags. Most flags are rejected at submit time
    /// (see module docs); `ASYNC` is accepted as a no-op.
    pub fn flags(mut self, f: Flags) -> Self {
        self.flags = f;
        self
    }
}

impl EntryMarker for Entry {}

/// Per-SQE flags. Mirrors `IOSQE_*`.
#[derive(Copy, Clone, Default, Debug, Eq, PartialEq)]
pub struct Flags(pub(crate) u8);

impl Flags {
    pub const FIXED_FILE: Self = Self(1 << 0);
    pub const IO_DRAIN: Self = Self(1 << 1);
    pub const IO_LINK: Self = Self(1 << 2);
    pub const IO_HARDLINK: Self = Self(1 << 3);
    pub const ASYNC: Self = Self(1 << 4);
    pub const BUFFER_SELECT: Self = Self(1 << 5);

    /// No flags. Mirrors `bitflags`'s `empty()` method on the real
    /// crate's `Flags` type.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether any flag the simulation rejects is set.
    #[cfg_attr(not(feature = "fs"), allow(dead_code))]
    pub(crate) fn has_unsupported(self) -> bool {
        const REJECTED: u8 = Flags::FIXED_FILE.0
            | Flags::IO_DRAIN.0
            | Flags::IO_LINK.0
            | Flags::IO_HARDLINK.0
            | Flags::BUFFER_SELECT.0;
        self.0 & REJECTED != 0
    }
}

impl std::ops::BitOr for Flags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Flags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// What the SQE asks the simulation to do at completion time.
///
/// Stored on [`Entry`] in lieu of the kernel's opcode/field union.
/// The buffer pointers are unsafe by construction — same caller
/// invariants as the real crate (buffer must remain valid until the
/// CQE is reaped, no aliasing).
#[derive(Clone, Debug)]
// The fields are only read by the runtime modules (submit/cqueue/sim),
// which are gated behind the `fs` feature. Without `fs`, these are
// "data only" — the consumer (e.g. parity tests against the real
// `io-uring` crate) holds Entry values without dispatching them.
#[cfg_attr(not(feature = "fs"), allow(dead_code))]
pub(crate) enum OpKind {
    Read {
        fd: RawFd,
        ptr: *mut u8,
        len: u32,
        offset: u64,
    },
    Write {
        fd: RawFd,
        ptr: *const u8,
        len: u32,
        offset: u64,
    },
    Fsync {
        fd: RawFd,
    },
    AsyncCancel {
        target_user_data: u64,
    },
}

// Raw pointers are not auto-Send/Sync. Sound here because:
//
// - The buffer's lifetime contract matches the real `io-uring` crate:
//   the caller must keep it valid until the CQE is reaped, with no
//   concurrent aliasing. We rely on that, same as Linux does.
// - The per-host `Mutex<IoUringHostState>` serializes access: an
//   `Entry` only crosses threads while the mutex is held, never as a
//   bare value, so each thread observes exclusive ownership. Aliasing
//   of the underlying buffer between submitter and drainer is the
//   caller's responsibility, identical to the real-io_uring contract.
unsafe impl Send for OpKind {}
unsafe impl Sync for OpKind {}

/// Submission queue handle. Borrows the ring for the duration of its
/// existence (or, for `submission_shared`, asserts unique writer
/// access — see [`crate::IoUring::submission_shared`]).
#[cfg(feature = "fs")]
pub struct SubmissionQueue<'a> {
    ring_fd: RawFd,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

/// Returned by [`SubmissionQueue::push`] when the SQ is full.
#[derive(Debug)]
pub struct PushError;

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("submission queue is full")
    }
}

impl std::error::Error for PushError {}

#[cfg(feature = "fs")]
impl<'a> SubmissionQueue<'a> {
    pub(crate) fn new(ring_fd: RawFd) -> Self {
        Self {
            ring_fd,
            _lifetime: std::marker::PhantomData,
        }
    }

    /// Push one SQE into the ring's submission queue.
    ///
    /// # Safety
    ///
    /// Mirrors the real crate's contract: the caller must be the sole
    /// writer of the SQ. The simulation does not currently enforce
    /// this invariant.
    pub unsafe fn push(&mut self, entry: &Entry) -> Result<(), PushError> {
        IoUringContext::current(|ctx| {
            let ring = ctx.io_uring.rings.get_mut(&self.ring_fd).ok_or(PushError)?;
            if ring.sq.len() >= ring.depth as usize {
                return Err(PushError);
            }
            ring.sq.push_back(entry.clone());
            Ok(())
        })
    }

    /// Number of entries currently queued (not yet submitted).
    pub fn len(&self) -> usize {
        IoUringContext::current(|ctx| {
            ctx.io_uring
                .rings
                .get(&self.ring_fd)
                .map(|r| r.sq.len())
                .unwrap_or(0)
        })
    }

    /// Whether the SQ has no queued entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the SQ is at its configured depth.
    pub fn is_full(&self) -> bool {
        IoUringContext::current(|ctx| {
            ctx.io_uring
                .rings
                .get(&self.ring_fd)
                .map(|r| r.sq.len() >= r.depth as usize)
                .unwrap_or(true)
        })
    }

    /// Maximum SQ depth.
    pub fn capacity(&self) -> usize {
        IoUringContext::current(|ctx| {
            ctx.io_uring
                .rings
                .get(&self.ring_fd)
                .map(|r| r.depth as usize)
                .unwrap_or(0)
        })
    }

    /// No-op in the simulation; mirrors the real crate's interface
    /// where it synchronizes the userspace tail with the kernel head.
    pub fn sync(&mut self) {}
}

mod private {
    pub trait Sealed {}
    impl Sealed for super::Entry {}
}
