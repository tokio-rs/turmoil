//! Opcode builders mirroring a small subset of
//! [`io_uring::opcode`](https://docs.rs/io-uring/0.7/io_uring/opcode).
//!
//! Only four ops are exposed: [`Read`], [`Write`], [`Fsync`],
//! [`AsyncCancel`] — the minimum set a typical io_uring consumer
//! actually submits. Other opcodes have no builder here; consumers
//! that need them must either teach this module or stop using the
//! simulation.

use super::squeue::{Entry, Flags, OpKind};
use super::types::Fd;

/// Read at offset, mirroring [`io_uring::opcode::Read`].
pub struct Read {
    fd: Fd,
    ptr: *mut u8,
    len: u32,
    offset: u64,
}

impl Read {
    /// New read SQE for `fd`, target buffer at `ptr` of length `len`.
    pub fn new(fd: Fd, ptr: *mut u8, len: u32) -> Self {
        Self {
            fd,
            ptr,
            len,
            offset: 0,
        }
    }

    /// Set the file offset.
    pub fn offset(mut self, off: u64) -> Self {
        self.offset = off;
        self
    }

    /// Finalize into an [`Entry`] ready to push onto the SQ.
    pub fn build(self) -> Entry {
        Entry {
            op: OpKind::Read {
                fd: self.fd.0,
                ptr: self.ptr,
                len: self.len,
                offset: self.offset,
            },
            user_data: 0,
            flags: Flags::default(),
        }
    }
}

/// Write at offset, mirroring [`io_uring::opcode::Write`].
pub struct Write {
    fd: Fd,
    ptr: *const u8,
    len: u32,
    offset: u64,
}

impl Write {
    /// New write SQE for `fd`, source buffer at `ptr` of length `len`.
    pub fn new(fd: Fd, ptr: *const u8, len: u32) -> Self {
        Self {
            fd,
            ptr,
            len,
            offset: 0,
        }
    }

    /// Set the file offset.
    pub fn offset(mut self, off: u64) -> Self {
        self.offset = off;
        self
    }

    /// Finalize into an [`Entry`] ready to push onto the SQ.
    pub fn build(self) -> Entry {
        Entry {
            op: OpKind::Write {
                fd: self.fd.0,
                ptr: self.ptr,
                len: self.len,
                offset: self.offset,
            },
            user_data: 0,
            flags: Flags::default(),
        }
    }
}

/// `fsync(2)` SQE, mirroring [`io_uring::opcode::Fsync`].
pub struct Fsync {
    fd: Fd,
}

impl Fsync {
    /// New fsync SQE for `fd`.
    ///
    /// `IORING_FSYNC_DATASYNC` (fdatasync semantics) is not modeled —
    /// the simulation always performs a full sync. Add a `.flags()`
    /// method that threads through to `exec_fsync` when a consumer
    /// needs the distinction.
    pub fn new(fd: Fd) -> Self {
        Self { fd }
    }

    /// Finalize into an [`Entry`] ready to push onto the SQ.
    pub fn build(self) -> Entry {
        Entry {
            op: OpKind::Fsync { fd: self.fd.0 },
            user_data: 0,
            flags: Flags::default(),
        }
    }
}

/// `IORING_OP_ASYNC_CANCEL`, mirroring [`io_uring::opcode::AsyncCancel`].
///
/// The simulation matches by the `user_data` of an in-flight op.
/// Targeting a fd or any other kernel match-mode is not modeled.
pub struct AsyncCancel {
    target: u64,
}

impl AsyncCancel {
    /// Build a cancel for the SQE whose CQE will carry `user_data`.
    pub fn new(user_data: u64) -> Self {
        Self { target: user_data }
    }

    /// Finalize into an [`Entry`] ready to push onto the SQ.
    pub fn build(self) -> Entry {
        Entry {
            op: OpKind::AsyncCancel {
                target_user_data: self.target,
            },
            user_data: 0,
            flags: Flags::default(),
        }
    }
}
