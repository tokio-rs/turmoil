//! Per-host ring state and scheduling heap.

use super::squeue::Entry as SqEntry;
use crate::fs::Fs;
use rand::seq::SliceRandom;
use rand::RngCore;
use std::collections::VecDeque;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// State for one [`crate::io_uring::IoUring`] instance, hung off
/// [`crate::fs::Fs::io_uring_rings`].
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
    /// is being torn down. The [`crate::io_uring::AsyncFd`] shim
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
        // apply. The raw pointers in PendingApply::Read/Write die
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
    /// flight. The [`crate::io_uring::AsyncFd`] shim sleeps until this
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

/// What runs against [`Fs`] at completion time.
///
/// Held until the [`super::cqueue::CompletionQueue`] iterator yields
/// the corresponding CQE; only then does the read/write/fsync take
/// effect on persisted state. This lets reads observe writes that
/// completed in the meantime, and writes apply in completion order
/// rather than submission order.
pub(crate) enum PendingApply {
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
    /// Posted by `cancel()` and unsupported-op paths.
    ImmediateError(i32),
}

// Same Send/Sync rationale as `super::squeue::OpKind`: the buffer
// lifetime is the caller's responsibility (real-io_uring contract),
// and the per-host `Mutex` serializes drainer access.
unsafe impl Send for PendingApply {}
unsafe impl Sync for PendingApply {}

impl PendingApply {
    /// Execute the op against `fs`, returning the CQE result.
    pub(crate) fn execute(self, fs: &mut Fs, rng: &mut dyn RngCore, now: Duration) -> i32 {
        match self {
            PendingApply::ImmediateError(err) => err,
            PendingApply::Read {
                fd,
                ptr,
                len,
                offset,
            } => exec_read(fs, rng, fd, ptr, len, offset),
            PendingApply::Write {
                fd,
                ptr,
                len,
                offset,
            } => exec_write(fs, rng, fd, ptr, len, offset, now),
            PendingApply::Fsync { fd } => exec_fsync(fs, rng, fd),
        }
    }
}

// --- fs effect execution ---------------------------------------------
//
// These touch the existing `Fs` API surface (open_handles, read_file,
// write_file, sync_file) and apply the same probabilistic fault knobs
// the [`crate::fs::shim`] surface does. There is one source of truth
// for fs behavior: ops submitted via io_uring observe the same
// io_error_probability / short_read_probability / corruption_probability
// / sync_probability the sync and tokio shims observe.
//
// # Divergence from real Linux: fd lifetime
//
// On real Linux, a file submitted via SQE has its open-file struct
// reference-counted by the kernel; closing the fd does NOT cancel an
// in-flight op. The simulation resolves the fd at completion time by
// looking it up in `Fs::open_handles`, so closing the file (dropping
// the `shim::std::fs::File`) between submit and completion turns the
// CQE into `-EBADF` instead of completing normally. Consumers must
// keep the file alive until they've drained the corresponding CQE.

fn exec_read(
    fs: &mut Fs,
    rng: &mut dyn RngCore,
    fd: RawFd,
    ptr: *mut u8,
    len: u32,
    offset: u64,
) -> i32 {
    let Some(path) = fs.open_handles.get(&fd).cloned() else {
        return -EBADF;
    };

    // O_DIRECT: enforce ptr/offset/len alignment, mirroring
    // shim::std::fs::File::read_at_internal. Real io_uring on a
    // misaligned O_DIRECT op returns -EINVAL.
    if fs.direct_io_fds.contains(&fd) && !direct_io_aligned(fs, ptr as usize, offset, len) {
        return -EINVAL;
    }

    // io_error_probability: surfaces as -EIO before touching the buf.
    if sample_prob(rng, fs.io_error_probability) {
        return -EIO;
    }

    // SAFETY: caller invariant — buffer remains valid for the in-flight
    // lifetime of the op.
    let buf = unsafe { std::slice::from_raw_parts_mut(ptr, len as usize) };
    let mut n = fs.read_file(&path, buf, offset);

    // short_read_probability: trim the reported count and zero the
    // tail, mirroring shim::std::fs::File::read_at_internal.
    if n > 1 && sample_prob(rng, fs.short_read_probability) {
        let short = sample_range(rng, 1..n);
        buf[short..n].fill(0);
        n = short;
    }

    // corruption_probability: silent bit-flip on a random byte.
    //
    // TODO: when `unstable-barriers` is enabled, fire
    // `crate::barriers::trigger_noop(crate::fs::FsCorruption { ... })`
    // here — same as `shim::std::fs::File::read_at_internal` does —
    // so tests observing FsCorruption events see ring-driven reads
    // alongside shim reads.
    if n > 0 && sample_prob(rng, fs.corruption_probability) {
        let corrupt_offset = sample_range(rng, 0..n);
        let corrupt_byte = (rng.next_u32() & 0xff) as u8;
        // Ensure at least one bit flip.
        buf[corrupt_offset] ^= corrupt_byte.max(1);
    }

    n as i32
}

fn exec_write(
    fs: &mut Fs,
    rng: &mut dyn RngCore,
    fd: RawFd,
    ptr: *const u8,
    len: u32,
    offset: u64,
    now: Duration,
) -> i32 {
    let Some(path) = fs.open_handles.get(&fd).cloned() else {
        return -EBADF;
    };

    // O_DIRECT alignment, see exec_read for the rationale.
    if fs.direct_io_fds.contains(&fd) && !direct_io_aligned(fs, ptr as usize, offset, len) {
        return -EINVAL;
    }

    // io_error_probability: surface -EIO before mutating state.
    if sample_prob(rng, fs.io_error_probability) {
        return -EIO;
    }

    // Capacity check, mirroring shim::std::fs::File::write_at_internal.
    let current_len = fs.file_len(&path);
    let write_end = offset + len as u64;
    let additional = write_end.saturating_sub(current_len);
    if additional > 0 && fs.check_space(additional).is_err() {
        return -ENOSPC;
    }

    // SAFETY: same as above.
    let buf = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    fs.write_file(&path, offset, buf, now);

    // sync_probability: spontaneous flush, again mirroring the shim.
    if sample_prob(rng, fs.sync_probability) {
        let _ = fs.sync_file(&path);
    }

    len as i32
}

fn exec_fsync(fs: &mut Fs, rng: &mut dyn RngCore, fd: RawFd) -> i32 {
    let Some(path) = fs.open_handles.get(&fd).cloned() else {
        return -EBADF;
    };
    if sample_prob(rng, fs.io_error_probability) {
        return -EIO;
    }
    match fs.sync_file(&path) {
        Ok(()) => 0,
        Err(_) => -EIO,
    }
}

fn sample_prob(rng: &mut dyn RngCore, p: f64) -> bool {
    if p <= 0.0 {
        return false;
    }
    if p >= 1.0 {
        return true;
    }
    // 53-bit float in [0, 1). Same shape as rand's `gen_bool` path
    // without forcing an extra trait import.
    let bits = (rng.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64);
    bits < p
}

fn sample_range(rng: &mut dyn RngCore, range: std::ops::Range<usize>) -> usize {
    let span = range.end - range.start;
    debug_assert!(span > 0, "empty range");
    range.start + (rng.next_u64() as usize) % span
}

/// O_DIRECT alignment check: pointer, offset, and length must each be
/// multiples of `Fs::direct_io_alignment`. Mirrors the same check in
/// the sync and tokio shims; real Linux returns `EINVAL` for these.
fn direct_io_aligned(fs: &Fs, ptr: usize, offset: u64, len: u32) -> bool {
    let alignment = fs.direct_io_alignment;
    if alignment == 0 {
        return true;
    }
    let align = alignment as usize;
    ptr.is_multiple_of(align)
        && offset.is_multiple_of(alignment)
        && (len as u64).is_multiple_of(alignment)
}

const EBADF: i32 = 9;
const EIO: i32 = 5;
const ECANCELED: i32 = 125;
const ENOENT: i32 = 2;
const ENOSPC: i32 = 28;
const EINVAL: i32 = 22;
