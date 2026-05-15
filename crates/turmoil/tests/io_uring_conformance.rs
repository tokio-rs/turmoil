//! io_uring conformance suite.
//!
//! The same body runs against two backends:
//!
//! - `sim`: `turmoil::io_uring`, driven inside a turmoil simulation.
//!   Runs everywhere turmoil compiles.
//! - `real`: the real `io-uring` 0.7 crate, with `tokio::io::unix::AsyncFd`
//!   on the ring fd. Linux-only, gated by `--features real-uring-conformance`.
//!
//! If both arms pass, consumers can rely on the sim to predict
//! real-kernel behavior on macOS and Windows.

#![cfg(feature = "unstable-io_uring")]

#[path = "io_uring_conformance/sim_arm.rs"]
mod sim_arm;

#[cfg(all(target_os = "linux", feature = "real-uring-conformance"))]
#[path = "io_uring_conformance/real_arm.rs"]
mod real_arm;
