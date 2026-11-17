//! Simulated networking types that mirror `tokio::net`.
//!
//! These are re-exported from [`turmoil_net`]'s tokio shim, which runs
//! a real TCP/UDP stack per host. Swap them in for `tokio::net` under
//! test, and the simulation routes their packets through the network
//! [`Sim`](crate::Sim) controls — latency, partitions, holds, and
//! crashes included.

pub use turmoil_net::shim::tokio::net::*;
