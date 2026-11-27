//! Per-host ring state and scheduling heap.

use super::squeue::{Buffer, Entry as SqEntry, Transfer};
use rand::seq::SliceRandom;
use rand::{Rng, RngCore};
use std::collections::VecDeque;
use std::io::{self, SeekFrom};
use std::os::fd::RawFd;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use turmoil_fs::{Access, FsState, Written};

/// State for one [`crate::IoUring`] instance, hung off
/// [`crate::host::IoUringHostState::rings`].
pub(crate) struct RingState {
    pub(crate) depth: u32,
    /// Entries pushed via `SubmissionQueue::push` but not yet handed
    /// off via `submit()`.
    pub(crate) sq: VecDeque<SqEntry>,
    /// Ops accepted by `submit()` but whose scheduled completion time
    /// is still in the future.
    inflight: Vec<ScheduledCqe>,
    /// CQEs whose scheduled time has elapsed but which the consumer
    /// has not yet drained via `CompletionQueue::next()`. Held in
    /// RNG-shuffled order to model out-of-order kernel completion.
    ready: VecDeque<ScheduledCqe>,
    /// Woken whenever the readiness state of the ring changes:
    /// a new CQE matures, a CQE is posted immediately, or the ring
    /// is being torn down. The [`crate::AsyncFd`] shim
    /// awaits this to avoid spinning.
    pub(crate) cq_notify: Arc<Notify>,
}

impl RingState {
    pub(crate) fn new(depth: u32) -> Self {
        Self {
            depth,
            sq: VecDeque::with_capacity(depth as usize),
            inflight: Vec::new(),
            ready: VecDeque::new(),
            cq_notify: Arc::new(Notify::new()),
        }
    }

    /// Schedule an op to complete at `when`.
    pub(crate) fn schedule(&mut self, user_data: u64, when: Duration, apply: PendingApply) {
        self.inflight.push(ScheduledCqe {
            when,
            user_data,
            apply,
        });
        self.cq_notify.notify_waiters();
    }

    /// Post an immediate error CQE (no fs effect).
    pub(crate) fn post_immediate_error(&mut self, user_data: u64, errno: i32, now: Duration) {
        self.inflight.push(ScheduledCqe {
            when: now,
            user_data,
            apply: PendingApply::ImmediateError(errno),
        });
        self.cq_notify.notify_waiters();
    }

    /// Cancel an op by `user_data`. Posts two CQEs: one `-ECANCELED`
    /// for the target, one `0` for the cancel itself (or `-ENOENT` if
    /// no match anywhere).
    ///
    /// We scan both `inflight` (deadline still in the future) and
    /// `ready` (matured but not yet drained). The latter is critical:
    /// each `ScheduledCqe` carries raw buffer pointers, and we drop
    /// the op rather than execute it — leaving a matured op in
    /// `ready` after cancel would let `pop_ready` later dereference
    /// pointers the consumer may have already freed.
    ///
    /// # Divergence from real Linux
    ///
    /// - Real `io_uring_register(IORING_OP_ASYNC_CANCEL)` is
    ///   best-effort: an op already submitted to the storage stack
    ///   may still complete normally and a write's effects may
    ///   persist. The simulation cancels deterministically and drops
    ///   any pending side effects. A consumer that treats cancel
    ///   outcomes as "may or may not have applied" is unaffected.
    /// - Real cancel of a CQE that has already completed (sat in the
    ///   kernel CQ unread) returns `-EALREADY`. The simulation
    ///   returns `-ENOENT` for "no such user_data anywhere" — close
    ///   enough for consumers that don't distinguish the two.
    pub(crate) fn cancel(&mut self, cancel_ud: u64, target_ud: u64, now: Duration) {
        // Drop the matching op (in either pool) without executing its
        // apply. The raw pointer in a pending transfer dies
        // here, so the consumer can free the buffer the moment the
        // cancel CQE is observed.
        let mut found = false;
        if let Some(idx) = self.inflight.iter().position(|s| s.user_data == target_ud) {
            self.inflight.swap_remove(idx);
            found = true;
        } else if let Some(idx) = self.ready.iter().position(|s| s.user_data == target_ud) {
            self.ready.remove(idx);
            found = true;
        }

        let cancel_result = if found {
            self.inflight.push(ScheduledCqe {
                when: now,
                user_data: target_ud,
                apply: PendingApply::ImmediateError(-ECANCELED),
            });
            0
        } else {
            -ENOENT
        };
        self.inflight.push(ScheduledCqe {
            when: now,
            user_data: cancel_ud,
            apply: PendingApply::ImmediateError(cancel_result),
        });
        self.cq_notify.notify_waiters();
    }

    /// Earliest scheduled completion time. `None` when nothing is in
    /// flight. The [`crate::AsyncFd`] shim sleeps until this
    /// deadline to avoid spinning on the world clock.
    pub(crate) fn next_deadline(&self) -> Option<Duration> {
        if !self.ready.is_empty() {
            return Some(Duration::ZERO);
        }
        self.inflight.iter().map(|s| s.when).min()
    }

    /// Number of CQEs whose scheduled time is `<= now`.
    pub(crate) fn ready_cq_count(&self, now: Duration) -> usize {
        self.ready.len() + self.inflight.iter().filter(|s| s.when <= now).count()
    }

    /// Drain matured ops from `inflight` into `ready`, shuffling them
    /// to model out-of-order kernel completion.
    fn promote_ready(&mut self, now: Duration, rng: &mut dyn RngCore) {
        let mut matured: Vec<ScheduledCqe> = Vec::new();
        let mut i = 0;
        while i < self.inflight.len() {
            if self.inflight[i].when <= now {
                matured.push(self.inflight.swap_remove(i));
            } else {
                i += 1;
            }
        }
        if !matured.is_empty() {
            matured.shuffle(rng);
            self.ready.extend(matured);
        }
    }

    /// Pop one ready scheduled CQE, promoting from `inflight` first.
    pub(crate) fn pop_ready(
        &mut self,
        now: Duration,
        rng: &mut dyn RngCore,
    ) -> Option<ScheduledCqe> {
        self.promote_ready(now, rng);
        self.ready.pop_front()
    }
}

pub(crate) struct ScheduledCqe {
    pub(crate) when: Duration,
    pub(crate) user_data: u64,
    pub(crate) apply: PendingApply,
}

/// What runs against [`FsState`] at completion time.
///
/// Held until the [`super::cqueue::CompletionQueue`] iterator yields
/// the corresponding CQE; only then does the read/write/fsync take
/// effect on persisted state. This lets reads observe writes that
/// completed in the meantime, and writes apply in completion order
/// rather than submission order.
pub(crate) enum PendingApply {
    Transfer(Issued),
    Fsync {
        fd: RawFd,
    },
    /// Posted by `cancel()` and unsupported-op paths.
    ImmediateError(i32),
}

impl PendingApply {
    pub(crate) fn execute(self, fs: &mut FsState) -> i32 {
        match self {
            Self::Transfer(io) => cqe_result(io.execute(fs)),
            Self::Fsync { fd } => exec_fsync(fs, fd),
            Self::ImmediateError(result) => result,
        }
    }
}

// --- fs effect execution ---------------------------------------------
//
// Reads and writes go through `FsState::read_at` and `FsState::write_at`,
// the same calls the [`turmoil_fs::shim`] surface makes. There is one
// source of truth for fs behavior: ops submitted via io_uring observe the
// same access checks, O_DIRECT alignment, capacity and
// io_error_probability / short_read_probability / corruption_probability
// / sync_probability the sync and tokio shims observe.
//
// # Divergence from real Linux: fd lifetime
//
// On real Linux, a file submitted via SQE has its open-file struct
// reference-counted by the kernel; closing the fd does NOT cancel an
// in-flight op. The simulation resolves the fd at completion time by
// looking it up with `FsState::open_file`, so closing the file (dropping
// the `shim::std::fs::File`) between submit and completion turns the
// CQE into `-EBADF` instead of completing normally. Consumers must
// keep the file alive until they've drained the corresponding CQE.

/// The offset that stands for the position of the file, as -1 does on
/// Linux.
const AT_POSITION: u64 = u64::MAX;

/// A transfer the ring has started on. As on Linux, one that is aimed at
/// the position of the file starts where the position is when the
/// transfer is submitted, wherever the position is by the time it runs.
pub(crate) struct Issued {
    /// The transfer, with the offset that it starts at.
    io: Transfer,
    /// Whether the transfer was aimed at the position of the file, which
    /// it then moves.
    moves: bool,
}

impl Transfer {
    pub(crate) fn issue(mut self, fs: &mut FsState) -> Issued {
        let moves = self.offset == AT_POSITION;
        if moves {
            // With no open file behind the descriptor, the transfer fails
            // when it runs.
            let position = fs.seek(self.fd, SeekFrom::Current(0));
            self.offset = position.unwrap_or_default();
        }
        Issued { io: self, moves }
    }
}

impl Issued {
    pub(crate) fn latency(&self, fs: &mut FsState) -> Duration {
        let (fd, offset) = (self.io.fd, self.io.offset);
        match self.io.buffer {
            Buffer::Read(_) => fs.pending_read_latency(fd, offset),
            Buffer::Write(_) => fs.calculate_latency(false),
        }
    }

    fn execute(self, fs: &mut FsState) -> io::Result<usize> {
        let (fd, offset) = (self.io.fd, self.io.offset);
        let (len, buffer) = (self.io.len as usize, self.io.buffer);
        let access = match buffer {
            Buffer::Read(_) => Access::Read,
            Buffer::Write(_) => Access::Write,
        };
        let direct_io = fs.file_for(fd, access)?.direct_io;
        // The kernel calls on no file to transfer nothing, so that even a
        // directory reads as nothing. Nor does it need a buffer for it.
        if len == 0 {
            return Ok(0);
        }
        let done = match buffer {
            Buffer::Read(ptr) => {
                // SAFETY: the submitter keeps this buffer writable until completion.
                let buf = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
                let len = fs.read_at(fd, buf, offset)?;
                Written { len, offset }
            }
            Buffer::Write(ptr) => {
                // SAFETY: the submitter keeps this buffer readable until completion.
                let buf = unsafe { std::slice::from_raw_parts(ptr, len) };
                fs.write_at(fd, buf, offset)?
            }
        };
        if done.len > 0 {
            fs.cache_page(fd, access, done.offset);
        }
        // The kernel leaves the position where a transfer ended that it
        // completed itself. Direct I/O completes later, on its own, and
        // the position stays where it was.
        if self.moves && !direct_io {
            fs.seek(fd, SeekFrom::Start(done.offset + done.len as u64))?;
        }
        Ok(done.len)
    }
}

fn exec_fsync(fs: &mut FsState, fd: RawFd) -> i32 {
    let p = fs.io_error_probability;
    if fs.open_file(fd).is_some() && p > 0.0 && fs.rng.random_bool(p) {
        return -EIO;
    }
    cqe_result(fs.sync(fd).map(|()| 0))
}

/// The CQE result for an op: the byte count on success, or the negative
/// `errno` the kernel reports for the failure.
fn cqe_result(result: io::Result<usize>) -> i32 {
    match result {
        Ok(n) => n as i32,
        // What the filesystem fails with has the number of its error. A
        // descriptor that was closed has none.
        Err(error) => -error.raw_os_error().unwrap_or(EBADF),
    }
}

const EBADF: i32 = 9;
const EIO: i32 = 5;
const ECANCELED: i32 = 125;
const ENOENT: i32 = 2;
pub(crate) const EINVAL: i32 = 22;
