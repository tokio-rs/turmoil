//! Submission queue types mirroring [`io_uring::squeue`](https://docs.rs/io-uring/0.7/io_uring/squeue).

// Without the runtime, SQEs can be built but are never dispatched.
#![cfg_attr(not(feature = "fs"), allow(dead_code))]

#[cfg(feature = "fs")]
use crate::host::RingId;
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
    pub(crate) op: OpKind,
    pub(crate) user_data: u64,
    pub(crate) flags: Flags,
}

impl Entry {
    pub(crate) fn new(op: OpKind) -> Self {
        Self {
            op,
            user_data: 0,
            flags: Flags::empty(),
        }
    }

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
pub(crate) enum OpKind {
    Transfer(Transfer),
    Fsync { fd: RawFd },
    AsyncCancel { target_user_data: u64 },
}

#[derive(Clone, Debug)]
pub(crate) struct Transfer {
    pub(crate) fd: RawFd,
    pub(crate) offset: u64,
    pub(crate) len: u32,
    pub(crate) buffer: Buffer,
}

impl Transfer {
    pub(crate) fn entry(fd: RawFd, offset: u64, len: u32, buffer: Buffer) -> Entry {
        let transfer = Self {
            fd,
            offset,
            len,
            buffer,
        };
        Entry::new(OpKind::Transfer(transfer))
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Buffer {
    Read(*mut u8),
    Write(*const u8),
}

// The submitter keeps the buffer valid until completion, with exclusive
// access for reads. The host registry serializes completion on all threads.
unsafe impl Send for Buffer {}
unsafe impl Sync for Buffer {}

/// Submission queue handle. Borrows the ring for the duration of its
/// existence (or, for `submission_shared`, asserts unique writer
/// access — see [`crate::IoUring::submission_shared`]).
#[cfg(feature = "fs")]
pub struct SubmissionQueue<'a> {
    ring: &'a RingId,
}

/// Returned when the SQ is full or its ring is unavailable on the entered host.
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
    pub(crate) fn new(ring: &'a RingId) -> Self {
        Self { ring }
    }

    /// Push one SQE into the ring's submission queue.
    ///
    /// # Safety
    ///
    /// Mirrors the real crate's contract: the caller must be the sole
    /// writer of the SQ. Nonempty buffers must remain valid until completion
    /// or cancellation, with exclusive access for a read into the buffer.
    pub unsafe fn push(&mut self, entry: &Entry) -> Result<(), PushError> {
        let pushed = self.ring.with(|ring, _| {
            if ring.sq.len() >= ring.depth as usize {
                return Err(PushError);
            }
            ring.sq.push_back(entry.clone());
            Ok(())
        });
        pushed.unwrap_or(Err(PushError))
    }

    /// Number of entries currently queued (not yet submitted).
    pub fn len(&self) -> usize {
        self.ring.with(|r, _| r.sq.len()).unwrap_or(0)
    }

    /// Whether the SQ has no queued entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the SQ is at its configured depth.
    pub fn is_full(&self) -> bool {
        self.ring
            .with(|r, _| r.sq.len() >= r.depth as usize)
            .unwrap_or(true)
    }

    /// Maximum SQ depth.
    pub fn capacity(&self) -> usize {
        self.ring.with(|r, _| r.depth as usize).unwrap_or(0)
    }

    /// No-op in the simulation; mirrors the real crate's interface
    /// where it synchronizes the userspace tail with the kernel head.
    pub fn sync(&mut self) {}
}

mod private {
    pub trait Sealed {}
    impl Sealed for super::Entry {}
}
