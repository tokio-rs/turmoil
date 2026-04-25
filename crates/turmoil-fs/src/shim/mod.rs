//! Drop-in shims over the [`kernel`](crate::kernel) filesystem layer.

pub mod std;
pub mod tokio;
