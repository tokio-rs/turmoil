//! Type fragments mirroring [`io_uring::types`](https://docs.rs/io-uring/0.7/io_uring/types).

use std::os::fd::RawFd;
use std::time::Duration;

/// File-descriptor wrapper consumed by opcode builders.
///
/// Holds the same kind of integer the real crate accepts. In the
/// simulation the integer is one allocated by
/// [`turmoil_fs::Fs::alloc_fd`] (file fds) or
/// [`crate::host::IoUringHostState::alloc_ring_fd`] (ring fds),
/// each from a distinct subrange above `SIM_FD_BASE`. Resolved back to
/// per-host state at completion time.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Fd(pub RawFd);

/// Mirrors [`io_uring::types::Timespec`].
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct Timespec {
    pub(crate) sec: u64,
    pub(crate) nsec: u32,
}

impl Timespec {
    /// Construct a zero-duration `Timespec`. Use [`Timespec::sec`]
    /// and [`Timespec::nsec`] to set the components.
    pub const fn new() -> Self {
        Self { sec: 0, nsec: 0 }
    }

    /// Replace the second component.
    pub const fn sec(mut self, sec: u64) -> Self {
        self.sec = sec;
        self
    }

    /// Replace the nanosecond component.
    pub const fn nsec(mut self, nsec: u32) -> Self {
        self.nsec = nsec;
        self
    }
}

impl From<Duration> for Timespec {
    fn from(d: Duration) -> Self {
        Self {
            sec: d.as_secs(),
            nsec: d.subsec_nanos(),
        }
    }
}

/// Arguments for [`crate::Submitter::submit_with_args`].
///
/// The timespec timeout is validated for shape (nsec must be a valid
/// nanosecond) but is not used to bound a wait — `submit_with_args`
/// is sync and the simulation cannot block sync code on simulated
/// time. Tests needing a real wall-time bound should await on
/// [`crate::AsyncFd::readable`] inside a
/// `tokio::time::timeout`. The sigmask field exists for source
/// compatibility but is ignored.
#[derive(Default)]
pub struct SubmitArgs<'a> {
    pub(crate) timeout: Option<Timespec>,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> SubmitArgs<'a> {
    /// Construct empty args.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a timeout for [`crate::Submitter::submit_with_args`].
    pub fn timespec(mut self, ts: &'a Timespec) -> Self {
        self.timeout = Some(*ts);
        self
    }
}
