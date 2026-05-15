//! Compile-only parity check between our shim and the real
//! `io-uring` 0.7 crate.
//!
//! Run `RUSTFLAGS=--cfg parity_io_uring cargo check -p turmoil
//! --features unstable-io_uring --tests` on Linux to verify the
//! shim's API surface matches the real crate's. Any drift — a
//! method that gained an argument, a constructor that changed its
//! return type, a missing builder — fails one build and succeeds
//! the other.
//!
//! The test body does no runtime work; it encodes the contract at
//! the type level via function-pointer coercions and trait-bound
//! calls. To pin a new guarantee, add another line.
//!
//! Linux-only because the real `io-uring` crate is Linux-only;
//! parity checking on macOS/Windows is meaningless.

#![cfg(all(target_os = "linux", feature = "unstable-io_uring"))]
#![allow(dead_code, unused_imports)]

#[cfg(parity_io_uring)]
use io_uring::{cqueue, opcode, squeue, types, Builder, IoUring, Submitter};
#[cfg(not(parity_io_uring))]
use turmoil::io_uring::{cqueue, opcode, squeue, types, Builder, IoUring, Submitter};

use std::fmt::Debug;
use std::io;
use std::os::fd::{AsRawFd, RawFd};

fn debug<T: Debug>() {}
fn as_raw_fd<T: AsRawFd>() {}

#[test]
fn type_surface() {
    // Core types implement Debug. Builder does NOT — real crate
    // derives only Clone+Default, so we pin that here by absence.
    debug::<types::Fd>();
    debug::<squeue::Entry>();
    debug::<cqueue::Entry>();

    // The ring exposes its readiness fd via AsRawFd so AsyncFd can
    // be built on it.
    as_raw_fd::<IoUring>();
}

#[test]
fn ioring_constructors() {
    // `IoUring::new(entries) -> io::Result<IoUring>`.
    let _: fn(u32) -> io::Result<IoUring> = IoUring::new;

    // `IoUring::builder() -> Builder`.
    let _: fn() -> Builder = IoUring::builder;
}

#[test]
fn submitter_methods() {
    // Note: real Submitter::submit takes &self; we accept either
    // shape via type ascription on the result.
    fn check(s: &Submitter<'_>) {
        let _: io::Result<usize> = s.submit();
        let _: io::Result<usize> = s.submit_and_wait(0);
    }
    let _ = check;
}

#[test]
fn opcode_builders() {
    // Read::new(fd, ptr, len) -> Read; .offset(u64) -> Read;
    // .build() -> squeue::Entry.
    fn check_read() {
        let mut buf = [0u8; 0];
        let r = opcode::Read::new(types::Fd(0), buf.as_mut_ptr(), 0).offset(0);
        let _: squeue::Entry = r.build();
    }
    fn check_write() {
        let buf = [0u8; 0];
        let w = opcode::Write::new(types::Fd(0), buf.as_ptr(), 0).offset(0);
        let _: squeue::Entry = w.build();
    }
    fn check_fsync() {
        let f = opcode::Fsync::new(types::Fd(0));
        let _: squeue::Entry = f.build();
    }
    fn check_cancel() {
        let c = opcode::AsyncCancel::new(0);
        let _: squeue::Entry = c.build();
    }
    let _ = (check_read, check_write, check_fsync, check_cancel);
}

#[test]
fn squeue_entry_builders() {
    // Entry chains: .user_data(u64) -> Entry, .flags(Flags) -> Entry.
    fn check(e: squeue::Entry) -> squeue::Entry {
        e.user_data(0).flags(squeue::Flags::empty())
    }
    let _ = check;
}

#[test]
fn cqueue_entry_accessors() {
    fn check(e: &cqueue::Entry) {
        let _: u64 = e.user_data();
        let _: i32 = e.result();
        let _: u32 = e.flags();
    }
    let _ = check;
}

#[test]
fn types_fd_wraps_rawfd() {
    // types::Fd is a tuple struct over RawFd in both crates.
    let _: types::Fd = types::Fd(0 as RawFd);
}
