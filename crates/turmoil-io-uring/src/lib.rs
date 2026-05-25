//! Simulated [`io_uring`](https://kernel.dk/io_uring.pdf) for turmoil.
//!
//! This crate mirrors the [`io-uring` 0.7](https://docs.rs/io-uring/0.7)
//! crate's Rust API closely enough that consumers can flip a feature
//! flag and swap their `use io_uring::*` imports for
//! `use turmoil_io_uring::*`, then run the same code on macOS or
//! Windows under a turmoil simulation.
//!
//! It is **not** a real `io_uring` — there is no kernel ring, no SQ/CQ
//! mmap, no `io_uring_enter` syscall. Instead, SQEs are queued in
//! per-host Rust state, and CQEs become observable as simulated time
//! advances past per-op completion timestamps drawn from the existing
//! [`turmoil_fs::FsConfig`] latency distribution. Out-of-order completion
//! falls out naturally because every op samples its own timestamp.
//!
//! # Features
//!
//! - `fs` — enable the runtime: `IoUring`, `Submitter`,
//!   `SubmissionQueue`, `CompletionQueue`, `AsyncFd`. Without `fs`,
//!   only the data-shape types (`Entry`, `Flags`, `opcode::*`,
//!   `types::*`) compile — useful for downstream parity tests against
//!   the real `io-uring` crate.
//!
//! All filesystem effects (read returning data, write becoming visible,
//! fsync flushing pending ops, fault injection, page cache, torn writes
//! on crash) flow through the same code paths as
//! [`turmoil_fs::shim::std::fs`] and [`turmoil_fs::shim::tokio::fs`] —
//! there is one source of truth for fs behavior under simulation.
//!
//! # What is supported
//!
//! - [`IoUring::new`], [`IoUring::builder`], submission/completion
//!   queues (shared and exclusive variants).
//! - Opcodes: [`opcode::Read`], [`opcode::Write`], [`opcode::Fsync`],
//!   [`opcode::AsyncCancel`].
//! - [`Submitter::submit`], [`Submitter::submit_and_wait`],
//!   [`Submitter::submit_with_args`].
//! - [`AsyncFd`] mirroring [`tokio::io::unix::AsyncFd`] for the ring's
//!   readiness fd, driven by simulated time.
//!
//! # What is rejected
//!
//! - `IORING_SETUP_SQPOLL`, `IORING_SETUP_IOPOLL`.
//! - Linked SQEs (`IOSQE_IO_LINK`, `IOSQE_IO_HARDLINK`).
//! - Fixed files / registered buffers / BufRing.
//! - Multi-shot ops.
//! - Any opcode other than the four listed above.
//!
//! Submitting any of these surfaces as `EINVAL` — either at submit time
//! or as a CQE result — exactly as the real kernel would.
//!
//! # Stability
//!
//! Normally consumed via `turmoil`'s `unstable-io_uring` feature. The
//! API may change between patch releases.

pub mod cqueue;
pub mod opcode;
pub mod squeue;
pub mod types;

#[cfg(feature = "fs")]
mod async_fd;
#[cfg(feature = "fs")]
pub mod host;
#[cfg(feature = "fs")]
pub(crate) mod sim;
#[cfg(feature = "fs")]
mod submit;

#[cfg(feature = "fs")]
pub use async_fd::{AsyncFd, AsyncFdReadyGuard};
#[cfg(feature = "fs")]
pub use submit::Submitter;
#[cfg(feature = "fs")]
pub use tokio::io::Interest;

#[cfg(feature = "fs")]
mod runtime {
    use crate::host::IoUringContext;
    use crate::{cqueue, sim, squeue};
    use std::io;
    use std::os::fd::{AsRawFd, RawFd};

    /// Simulated `io_uring` instance.
    ///
    /// Mirrors [`io_uring::IoUring`](https://docs.rs/io-uring/0.7/io_uring/struct.IoUring.html).
    /// The two type parameters exist for source-level compatibility with the
    /// real crate's `IoUring<S, C>`; only the default
    /// `(squeue::Entry, cqueue::Entry)` shape is implemented in the
    /// simulation.
    pub struct IoUring<S = squeue::Entry, C = cqueue::Entry>
    where
        S: squeue::EntryMarker,
        C: cqueue::EntryMarker,
    {
        /// Sim-allocated id used to look up [`sim::RingState`] on the
        /// host's `IoUringHostState` and to back [`AsRawFd`].
        ring_fd: RawFd,
        params: Parameters,
        _markers: std::marker::PhantomData<(S, C)>,
    }

    /// Setup parameters reported by [`IoUring::params`].
    ///
    /// Mirrors a small subset of [`io_uring::Parameters`](https://docs.rs/io-uring/0.7/io_uring/struct.Parameters.html).
    #[derive(Clone, Debug, Default)]
    pub struct Parameters {
        sq_entries: u32,
        cq_entries: u32,
    }

    impl Parameters {
        /// Number of submission queue entries.
        pub fn sq_entries(&self) -> u32 {
            self.sq_entries
        }

        /// Number of completion queue entries.
        pub fn cq_entries(&self) -> u32 {
            self.cq_entries
        }
    }

    /// Builder for [`IoUring`], mirroring
    /// [`io_uring::Builder`](https://docs.rs/io-uring/0.7/io_uring/struct.Builder.html).
    ///
    /// Note: real crate's `Builder` doesn't impl `Debug`, so we don't
    /// either. The parity test pins this contract.
    #[derive(Clone, Default)]
    pub struct Builder {
        setup_sqpoll: Option<u32>,
        setup_iopoll: bool,
    }

    impl Builder {
        /// Reject SQPOLL — the simulation has no kernel thread to model.
        /// Recorded here so [`Builder::build`] can fail with `EINVAL`.
        pub fn setup_sqpoll(&mut self, idle_ms: u32) -> &mut Self {
            self.setup_sqpoll = Some(idle_ms);
            self
        }

        /// Reject IOPOLL.
        pub fn setup_iopoll(&mut self) -> &mut Self {
            self.setup_iopoll = true;
            self
        }

        /// Build the simulated ring with `entries` SQ/CQ slots.
        pub fn build(&self, entries: u32) -> io::Result<IoUring> {
            if self.setup_sqpoll.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "turmoil io_uring shim does not support IORING_SETUP_SQPOLL",
                ));
            }
            if self.setup_iopoll {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "turmoil io_uring shim does not support IORING_SETUP_IOPOLL",
                ));
            }
            IoUring::new_inner(entries)
        }
    }

    impl IoUring {
        /// Allocate a fresh ring on the current host with the given depth.
        pub fn new(entries: u32) -> io::Result<Self> {
            Self::new_inner(entries)
        }

        /// Construct a [`Builder`] for advanced setup options.
        pub fn builder() -> Builder {
            Builder::default()
        }

        fn new_inner(entries: u32) -> io::Result<Self> {
            if entries == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "io_uring entries must be > 0",
                ));
            }
            let depth = entries.next_power_of_two().max(1);
            let ring_fd = IoUringContext::current(|ctx| {
                let fd = ctx.io_uring.alloc_ring_fd();
                ctx.io_uring.rings.insert(fd, sim::RingState::new(depth));
                fd
            });
            Ok(IoUring {
                ring_fd,
                params: Parameters {
                    sq_entries: depth,
                    cq_entries: depth * 2,
                },
                _markers: std::marker::PhantomData,
            })
        }

        /// Setup parameters for this ring (depth, etc.).
        pub fn params(&self) -> &Parameters {
            &self.params
        }

        /// Submission-side handle requiring exclusive access to the ring.
        pub fn submission(&mut self) -> squeue::SubmissionQueue<'_> {
            squeue::SubmissionQueue::new(self.ring_fd)
        }

        /// Submission-side handle that allows shared access. Caller asserts
        /// they are the sole writer of the SQ.
        ///
        /// # Safety
        ///
        /// Mirrors the real crate's contract: the SQ has a single writer
        /// at any time. The simulation does not currently enforce this;
        /// it trusts the caller, same as the real `io-uring` crate.
        pub unsafe fn submission_shared(&self) -> squeue::SubmissionQueue<'_> {
            squeue::SubmissionQueue::new(self.ring_fd)
        }

        /// Completion-side handle requiring exclusive access to the ring.
        pub fn completion(&mut self) -> cqueue::CompletionQueue<'_> {
            cqueue::CompletionQueue::new(self.ring_fd)
        }

        /// Completion-side handle that allows shared access. Caller asserts
        /// they are the sole consumer of the CQ.
        ///
        /// # Safety
        ///
        /// Same single-consumer contract as the real crate.
        pub unsafe fn completion_shared(&self) -> cqueue::CompletionQueue<'_> {
            cqueue::CompletionQueue::new(self.ring_fd)
        }

        /// [`Submitter`] handle for this ring.
        pub fn submitter(&self) -> super::Submitter<'_> {
            super::Submitter::new(self.ring_fd)
        }

        /// Convenience: equivalent to `self.submitter().submit()`.
        pub fn submit(&self) -> io::Result<usize> {
            self.submitter().submit()
        }

        /// Convenience: equivalent to `self.submitter().submit_and_wait(want)`.
        pub fn submit_and_wait(&self, want: usize) -> io::Result<usize> {
            self.submitter().submit_and_wait(want)
        }
    }

    impl<S, C> Drop for IoUring<S, C>
    where
        S: squeue::EntryMarker,
        C: cqueue::EntryMarker,
    {
        fn drop(&mut self) {
            IoUringContext::current_if_set(|ctx| {
                // Wake any AsyncFd::readable() callers before removing
                // the ring so they observe NotFound on the next snapshot
                // instead of sleeping until their cached deadline.
                if let Some(ring) = ctx.io_uring.rings.get(&self.ring_fd) {
                    ring.cq_notify.notify_waiters();
                }
                ctx.io_uring.rings.swap_remove(&self.ring_fd);
            });
        }
    }

    /// Exposes the simulated ring fd so consumers can hand it to
    /// [`super::AsyncFd`].
    impl<S, C> AsRawFd for IoUring<S, C>
    where
        S: squeue::EntryMarker,
        C: cqueue::EntryMarker,
    {
        fn as_raw_fd(&self) -> RawFd {
            self.ring_fd
        }
    }
}

#[cfg(feature = "fs")]
pub use runtime::{Builder, IoUring, Parameters};
