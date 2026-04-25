//! Drop-in shims over the [`kernel`](crate::kernel) socket layer.
//!
//! Only `tokio` is shimmed — `std::net` is intentionally out of scope.

pub mod tokio;
