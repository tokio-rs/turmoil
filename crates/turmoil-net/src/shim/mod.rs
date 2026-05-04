//! Drop-in replacements for `tokio::net` types. Production code
//! imports these behind a `#[cfg]` in tests and compiles unchanged;
//! every type, method, and error kind mirrors `tokio::net` exactly.

pub mod tokio;
