//! Simulated filesystem for turmoil.
//!
//! This crate will house the filesystem layer currently living in
//! `turmoil::fs`, factored out along the same kernel/shim split used by
//! `turmoil-net`. The [`kernel`] module will hold the authoritative
//! implementation (per-host namespace, durability model, pending/synced
//! state), and [`shim`] will provide drop-in replacements for `std::fs`,
//! `std::os::unix::fs`, and `tokio::fs`.

pub mod kernel;
pub mod shim;
